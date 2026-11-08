//! `StartDatabaseImport`, `DatabaseImportCommandBuilder`, `DatabaseImportCleanup`
//! and `DatabaseBackupFileValidator`: restoring a backup into a running database.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow, Kind};
use coolify_db::records::Record;
use coolify_remote::{RunOptions, Stream, Target, shell::quote};
use fancy_regex::Regex as FancyRegex;
use rand::Rng as _;
use serde_json::{Value, json};

use super::common::{helper_image, hrtime, now};
use super::s3::{S3Target, valid_bucket_name};
use crate::Jobs;
use crate::server::ServerInfo;

pub const MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const IMPORT_OPERATION: &str = "database_import";

/// `DatabaseImportException`: a message and an HTTP status.
#[derive(Debug, Clone)]
pub struct ImportError {
    pub message: String,
    pub status: u16,
}

fn fail<T>(message: &str) -> Result<T, ImportError> {
    Err(ImportError {
        message: message.to_owned(),
        status: 422,
    })
}

/// `DatabaseImportSource`.
#[derive(Debug, Clone, Default)]
pub struct ImportSource {
    pub kind: String,
    pub upload_id: Option<String>,
    pub path: Option<String>,
    pub s3_storage_uuid: Option<String>,
    pub dump_all: bool,
    pub replace_existing: bool,
    pub keep_owners: bool,
    pub sqlite_database: Option<String>,
    pub restore_mysql_users: bool,
    /// Set when the user cannot use team credentials (`useTeamCredentials`, write on the whole
    /// team): server and S3 imports then take only this database's own backups.
    pub own_backups_only: bool,
}

/// `StartDatabaseImport::OWN_BACKUPS_ONLY_MESSAGE`.
pub const OWN_BACKUPS_ONLY_MESSAGE: &str = "You can import only backups of this database. Importing other files needs write access on the whole team.";

/// `StartDatabaseImport::allowsSourcePath` without the team-credentials shortcut: the path is a
/// backup of this database, in the folders of `DatabaseBackupJob` (S3 keys are the local path
/// without the leading slash). The folder names end with the team id and the database's uuid
/// (`name-serviceuuid` for a service database), so a rename keeps old backups importable.
pub fn is_own_backup_path(d: &DatabaseRow, team_id: i64, path: &str) -> bool {
    let suffix = match d.str("__service_uuid") {
        Some(service) => format!("{}-{service}", d.str("name").unwrap_or_default()),
        None => d.uuid.clone(),
    };
    let pattern = format!(
        "^{}[^/]*-{}/[^/]*-{}/[^/]+$",
        regex::escape("/data/coolify/backups/databases/"),
        regex::escape(&team_id.to_string()),
        regex::escape(&suffix),
    );
    let path = format!("/{}", path.trim_start_matches('/'));
    regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(&path))
}

fn own_backups_only_error<T>() -> Result<T, ImportError> {
    Err(ImportError {
        message: OWN_BACKUPS_ONLY_MESSAGE.to_owned(),
        status: 403,
    })
}

const PRELUDE: &str = r#"fail() { echo "$1" >&2; exit 1; }
is_gzip() { [ "$(head -c 2 "$backup" | od -An -tx1 | tr -d ' \n')" = 1f8b ]; }
stream() { if is_gzip; then gunzip -c "$backup"; else cat "$backup"; fi; }
header() { stream | head -c "$1" | od -An -tx1 | tr -d ' \n'; }
is_tar() { [ "$(stream | head -c 262 | tail -c 5)" = ustar ]; }
is_text() { [ "$(stream | head -c 65536 | tr -d '\000' | wc -c | tr -d ' ')" = "$(stream | head -c 65536 | wc -c | tr -d ' ')" ]; }
extract_tar() { work=$(mktemp -d) && trap 'rm -rf "$work"' EXIT && stream | tar -xf - -C "$work" || fail 'The tar backup cannot be extracted.'; }
use_single_tar_member() { [ "$(find "$work" -type f | wc -l | tr -d ' ')" = 1 ] && backup=$(find "$work" -type f); }
if is_gzip; then
  gunzip -t "$backup" || fail 'The gzip backup is corrupt or incomplete. Nothing was changed.'
fi
"#;

const MYSQL_SYSTEM_DATABASE_FILTER: &str = r#"function database(line) {
  if (index(line, "`") > 0) { sub(/^[^`]*`/, "", line); sub(/`.*$/, "", line); return tolower(line) }
  gsub(/\/\*[^*]*\*\//, " ", line)
  line = tolower(line)
  sub(/^[ \t]*(use|create[ \t]+(database|schema))[ \t]+/, "", line)
  sub(/^if[ \t]+not[ \t]+exists[ \t]+/, "", line)
  sub(/[ \t;].*$/, "", line)
  return line
}
function is_system(name) { return name == "mysql" || name == "sys" || name == "performance_schema" || name == "information_schema" }
/^-- Current Database: `/ { skip = is_system(database($0)) }
tolower($0) ~ /^[ \t]*use[ \t]/ { skip = is_system(database($0)) }
tolower($0) ~ /^[ \t]*create[ \t]+(database|schema)[ \t]/ { if (is_system(database($0))) { next } skip = 0 }
/^\/\*!40103 SET TIME_ZONE=@OLD_TIME_ZONE/ { skip = 0 }
!skip { print }"#;

const POSTGRES_PRELUDE: &str = r#"
check_connection() {
  psql -U "$pg_user" -d template1 -Atc 'SELECT 1' >/dev/null 2>&1 || fail "Cannot connect to PostgreSQL as user \"$pg_user\". Set POSTGRES_USER on the database container to its superuser. Nothing was changed."
}
check_superuser() {
  [ "$(psql -U "$pg_user" -d template1 -Atc 'SELECT rolsuper FROM pg_roles WHERE rolname = current_user' 2>/dev/null)" = t ] || fail "Restoring all databases needs a PostgreSQL superuser, and \"$pg_user\" is not one or cannot connect. Set POSTGRES_USER on the database container to its superuser. Nothing was changed."
}

"#;

const POSTGRES_DUMP_ALL: &str = r#"db=${POSTGRES_DB:-$pg_user}
if [ "$(stream | head -c 5)" = PGDMP ] || is_tar; then
  kind=archive
  stream | pg_restore -l >/dev/null 2>&1 || fail 'pg_restore cannot read this backup archive. Nothing was changed.'
elif is_text; then
  kind=sql
else
  fail 'Unsupported PostgreSQL backup format. Use a pg_dump archive (custom or tar format) or an SQL file. Nothing was changed.'
fi
check_superuser
psql -U "$pg_user" -d template1 -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IS NOT NULL AND pid <> pg_backend_pid()" || exit 1
psql -U "$pg_user" -d template1 -t -c "SELECT datname FROM pg_database WHERE NOT datistemplate" | xargs -I {} dropdb -U "$pg_user" --maintenance-db=template1 --if-exists {} || exit 1
createdb -U "$pg_user" "$db" || exit 1
[ "$db" = postgres ] || createdb -U "$pg_user" postgres || exit 1
if [ "$kind" = archive ]; then
  stream | pg_restore -U "$pg_user" -d "$db"
else
  stream | psql -U "$pg_user" -d "$db"
fi"#;

const POSTGRES_REPLACE_SQL: &str = r#"

  check_connection
  echo 'SQL backups cannot replace single objects. The backup is restored into a new database first; the current database is replaced only when that restore succeeds.'
  if ! existing=$(printf '%s\n' "SELECT 1 FROM pg_database WHERE datname IN (:'new', :'old');" | psql -v ON_ERROR_STOP=1 -At -v new="$new" -v old="$old" -U "$pg_user" -d template1); then
    fail 'The temporary database names could not be checked. Nothing was changed.'
  fi
  [ -z "$existing" ] || fail 'A temporary database name is already in use. Nothing was changed.'
  createdb -U "$pg_user" "$new" || exit 1
  if ! stream | psql -v ON_ERROR_STOP=1 --single-transaction -U "$pg_user" -d "$new"; then
    PGOPTIONS='-c client_min_messages=warning' dropdb --maintenance-db=template1 -U "$pg_user" --if-exists "$new"
    fail 'The SQL restore failed. The current database was not changed.'
  fi
  if ! printf '%s\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = :'db' AND pid <> pg_backend_pid();" 'ALTER DATABASE :"db" RENAME TO :"old";' 'ALTER DATABASE :"new" RENAME TO :"db";' | psql -v ON_ERROR_STOP=1 -v db="$db" -v old="$old" -v new="$new" -U "$pg_user" -d template1 >/dev/null; then
    # Undo a half-done swap: the current database keeps its name and data.
    printf '%s\n' 'ALTER DATABASE :"old" RENAME TO :"db";' | psql -v db="$db" -v old="$old" -U "$pg_user" -d template1 >/dev/null 2>&1
    PGOPTIONS='-c client_min_messages=warning' dropdb --maintenance-db=template1 -U "$pg_user" --if-exists "$new"
    fail 'The current database is in use and could not be replaced. Nothing was changed.'
  fi
  PGOPTIONS='-c client_min_messages=warning' dropdb --maintenance-db=template1 -U "$pg_user" --if-exists "$old" || exit 1"#;

/// `PostgresUser::shellExpression`.
const PG_USER_EXPRESSION: &str = "${POSTGRES_USER:-${PGUSER:-${POSTGRESQL_USERNAME:-postgres}}}";

fn postgres_single(replace_existing: bool, keep_owners: bool) -> String {
    let owners = if keep_owners {
        ""
    } else {
        " --no-owner --no-acl"
    };
    let clean = if replace_existing {
        " --clean --if-exists"
    } else {
        ""
    };
    let sql = if replace_existing {
        let suffix: String = {
            let mut rng = rand::thread_rng();
            (0..16)
                .map(|_| format!("{:02x}", rng.r#gen::<u8>()))
                .collect()
        };
        format!(
            "\n  new=coolify_restore_new_{suffix}\n  old=coolify_restore_old_{suffix}\n{POSTGRES_REPLACE_SQL}"
        )
    } else {
        "\n  check_connection\n  stream | psql -v ON_ERROR_STOP=1 --single-transaction -U \"$pg_user\" -d \"$db\"".to_owned()
    };
    format!(
        r#"db=${{POSTGRES_DB:-$pg_user}}
if [ "$(stream | head -c 5)" = PGDMP ] || is_tar; then
  check_connection
  stream | pg_restore --exit-on-error --single-transaction{owners}{clean} -U "$pg_user" -d "$db"
elif ! is_text; then
  fail 'Unsupported PostgreSQL backup format. Use a pg_dump archive (custom or tar format) or an SQL file.'
elif stream | head -c 4096 | grep -q 'PostgreSQL database cluster dump'; then
  fail 'This backup contains all databases. Select "Backup contains all databases" to restore it.'
else{sql}
fi"#
    )
}

/// `MYSQL_CLIENT_COMMAND_PATTERN`: a short mysql/mariadb client command (`\\!`, `\\T`, `\\P`, `\\e`,
/// `\\.`) after an odd run of backslashes. mysqldump escapes a backslash in data as `\\\\`.
const MYSQL_CLIENT_COMMAND_PATTERN: &str = r"(^|[^\\])(\\\\)*\\[!TPe.]";
/// `MYSQL_NAMED_CLIENT_COMMAND_PATTERN`: a long client command at the start of a line.
const MYSQL_NAMED_CLIENT_COMMAND_PATTERN: &str =
    "^[[:space:]]*(system|tee|pager|edit|source)([[:space:]]|;|$)";

/// `DatabaseImportCommandBuilder::mysql()`. A dump with a client command that runs a shell command
/// or uses a file is refused, and the client runs with its client commands off where it can
/// (`--sandbox` on MariaDB, `--commands=FALSE` on MySQL).
fn mysql_script(binary: &str, prefix: &str, dump_all: bool, restore_users: bool) -> String {
    let mut preflight = format!(
        "if is_tar; then\n  extract_tar\n  use_single_tar_member || fail 'A tar backup must contain exactly one SQL dump. Nothing was changed.'\nfi\nis_text || fail 'Unsupported backup format. Use an SQL dump. Nothing was changed.'\nif stream | LC_ALL=C grep -Eq {} || stream | LC_ALL=C grep -Eiq {}; then\n  fail 'Blocked restore: the backup contains a {binary} client command (system, tee, pager, edit or source). If you trust this dump, restore it from the database container terminal. Nothing was changed.'\nfi\n",
        quote(MYSQL_CLIENT_COMMAND_PATTERN),
        quote(MYSQL_NAMED_CLIENT_COMMAND_PATTERN)
    );
    let client_safety = format!(
        "client_safety=\nif {binary} --help 2>/dev/null | grep -q -e '--sandbox'; then\n  client_safety=--sandbox\nelif {binary} --help 2>/dev/null | grep -q -e '--commands'; then\n  client_safety=--commands=FALSE\nfi\n"
    );
    if !dump_all {
        return format!(
            "{preflight}{client_safety}if [ \"$(stream | grep -c '^USE `')\" -gt 1 ]; then\n  fail 'This backup contains more than one database. Select \"Backup contains all databases\" to restore it. Nothing was changed.'\nfi\nstream | {binary} $client_safety -u ${prefix}_USER -p${prefix}_PASSWORD ${prefix}_DATABASE"
        );
    }
    let root = format!("{binary} -u root -p${{{prefix}_ROOT_PASSWORD}}");
    let database = format!("${{{prefix}_DATABASE:-default}}");
    let source = if restore_users {
        "stream".to_owned()
    } else {
        preflight.push_str("command -v awk >/dev/null 2>&1 || fail 'awk is required to skip the system databases of the backup. Nothing was changed.'\n");
        format!(
            "{{ stream | awk {} || echo 'Coolify could not filter the system databases of the backup;'; }}",
            quote(MYSQL_SYSTEM_DATABASE_FILTER)
        )
    };
    format!(
        "{preflight}{client_safety}for pid in $({root} -N -e \"SELECT id FROM information_schema.processlist WHERE user != 'root';\"); do {root} -e \"KILL $pid\" 2>/dev/null || true; done\n{root} -N -e \"SELECT CONCAT('DROP DATABASE IF EXISTS \\`',schema_name,'\\`;') FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema','mysql','performance_schema','sys');\" | {root} || exit 1\n{root} -e \"CREATE DATABASE IF NOT EXISTS \\`{database}\\`;\" || exit 1\n{source} | {binary} $client_safety -u root -p${{{prefix}_ROOT_PASSWORD}} {database}"
    )
}

fn mongodb_script(replace_existing: bool) -> String {
    let drop = if replace_existing { " --drop" } else { "" };
    format!(
        r#"restore() {{ mongorestore --authenticationDatabase=admin --username $MONGO_INITDB_ROOT_USERNAME --password $MONGO_INITDB_ROOT_PASSWORD --uri mongodb://localhost:27017{drop} "$@"; }}
if is_tar; then
  extract_tar
  if ! use_single_tar_member; then
    bson=$(find "$work" -type f \( -name '*.bson' -o -name '*.bson.gz' \) | head -n 1)
    [ -n "$bson" ] || fail 'The tar backup does not contain a mongodump directory. Nothing was changed.'
    root=$(dirname "$(dirname "$bson")")
    if find "$root" -type f -name '*.bson.gz' | grep -q .; then restore --gzip --dir="$root"; else restore --dir="$root"; fi
    exit $?
  fi
fi
[ "$(header 4)" = 6de29981 ] || fail 'Unsupported MongoDB backup format. Use a mongodump archive or a dump directory packed as tar. Single .bson files are not supported. Nothing was changed.'
if is_gzip; then restore --gzip --archive="$backup"; else restore --archive="$backup"; fi"#
    )
}

fn sqlite_script(file: &str) -> String {
    let file = quote(file);
    format!(
        r#"stream > "$backup.db" || fail 'The backup cannot be read. Nothing was changed.'
[ "$(head -c 15 "$backup.db")" = 'SQLite format 3' ] || {{ rm -f "$backup.db"; fail 'The backup is not a SQLite database. Nothing was changed.'; }}
sqlite3 -bail {file} '.timeout 10000' ".restore $backup.db"; status=$?; rm -f "$backup.db"; exit $status"#
    )
}

/// `buildRestoreCommand`.
pub fn build_restore_command(
    d: &DatabaseRow,
    path: &str,
    s: &ImportSource,
    sqlite_file: Option<&str>,
) -> String {
    let script = match d.kind {
        Kind::Postgresql => {
            let body = if s.dump_all {
                POSTGRES_DUMP_ALL.to_owned()
            } else {
                postgres_single(s.replace_existing, s.keep_owners)
            };
            format!("pg_user={PG_USER_EXPRESSION}{POSTGRES_PRELUDE}{body}")
        }
        Kind::Mysql => mysql_script("mysql", "MYSQL", s.dump_all, s.restore_mysql_users),
        Kind::Mariadb => mysql_script("mariadb", "MARIADB", s.dump_all, s.restore_mysql_users),
        Kind::Mongodb => mongodb_script(s.replace_existing),
        Kind::Sqlite => sqlite_script(&format!(
            "/var/lib/sqlite/{}",
            sqlite_file.unwrap_or_default()
        )),
        _ => String::new(),
    };
    format!("backup={}\n{PRELUDE}{script}", quote(path))
}

/// `buildNormalizeScript`.
pub fn build_normalize_script(source: &str, target: &str) -> String {
    let source = quote(source);
    let target = quote(target);
    format!(
        r#"f={source}
out={target}
case "$(head -c 6 "$f" | od -An -tx1 | tr -d ' \n')" in
  425a68*) bunzip2 -c "$f" > "$out" || {{ echo 'The bz2 backup cannot be decompressed.' >&2; exit 1; }} ;;
  fd377a585a00) unxz -c "$f" > "$out" || {{ echo 'The xz backup cannot be decompressed.' >&2; exit 1; }} ;;
  504b0304*)
    d=$(mktemp -d) && unzip -q "$f" -d "$d" || {{ echo 'The zip backup cannot be extracted.' >&2; exit 1; }}
    [ "$(find "$d" -type f | wc -l | tr -d ' ')" = 1 ] || {{ echo 'A zip backup must contain exactly one file.' >&2; exit 1; }}
    mv "$(find "$d" -type f)" "$out" ;;
  *) mv "$f" "$out" ;;
esac"#
    )
}

/// `POSTGRES_COPY_DATA_FILTER`: a PostgreSQL SQL dump without the table data of its
/// `COPY ... FROM stdin` blocks.
const POSTGRES_COPY_DATA_FILTER: &str =
    r#"data { if ($0 == "\\.") data = 0; next } { print } /^COPY .* FROM stdin;$/ { data = 1 }"#;

/// `buildPostgresRestoreScanScript`. The table data of `COPY ... FROM stdin` blocks is not
/// scanned (it is data, not SQL); when awk fails, the scan gets a line that matches.
fn postgres_scan_script(path: &str) -> String {
    let escaped = quote(path);
    let sep = "([[:space:]]|~|/\\*.*\\*/|--[^~]*~)";
    let sql_pattern = format!(
        "(from|to){sep}+program|lo_(import|export){sep}*\\(|alter{sep}+system{sep}+(set|reset)"
    );
    let psql_pattern = r#"(^|[^\\])(\\\\)*\\(!|(o|out|gx?|w|write)[[:space:]][^\\]*\||copy[[:space:]].*program|(setenv|e|edit|ef|ev|i|ir|include|include_relative)([[:space:]]|$)|([^nbv[:space:]\\]|[nbv][^[:space:]\\])[^[:space:]\\]*[[:space:]][^\\]*`)"#;
    let escaped_sql = quote(&sql_pattern);
    let escaped_psql = quote(psql_pattern);
    let contents = format!("{{ gunzip -cf {escaped} 2>/dev/null || cat {escaped}; }}");
    let without_copy_data = |source: &str| {
        format!(
            "{{ {source} | LC_ALL=C awk {} || echo '\\! from program'; }}",
            quote(POSTGRES_COPY_DATA_FILTER)
        )
    };
    let scan = |source: &str| {
        let filtered = without_copy_data(source);
        format!(
            "{filtered} | LC_ALL=C grep -Eiq {escaped_psql} || {filtered} | tr -d '~' | tr '\\n\\r' '~~' | LC_ALL=C grep -Eiq {escaped_sql}"
        )
    };
    let custom_scan = scan("pg_restore -f - \"$inspect\" 2>/dev/null");
    let sql_scan = scan(&contents);
    let blocked_program = "echo 'Blocked PostgreSQL restore: the backup contains a COPY ... PROGRAM, psql shell command, lo_import/lo_export, or ALTER SYSTEM pattern. If you trust this dump, restore it from the database container terminal.'; exit 1";
    let blocked_inspect =
        "echo 'Blocked PostgreSQL restore: unable to inspect custom archive.'; exit 1";
    format!(
        r#"command -v awk >/dev/null 2>&1 || {{ echo 'Blocked PostgreSQL restore: awk is required to scan the backup.'; exit 1; }}
header=$({contents} | head -c 5)
tar_magic=$({contents} | head -c 262 | tail -c 5)
if [ "$header" = 'PGDMP' ] || [ "$tar_magic" = 'ustar' ]; then
  inspect=$(mktemp)
  trap 'rm -f "$inspect"' EXIT
  if ! {contents} > "$inspect"; then
    {blocked_inspect}
  fi
  if ! pg_restore -l "$inspect" >/dev/null 2>&1; then
    {blocked_inspect}
  fi
  if {custom_scan}; then
    {blocked_program}
  fi
elif {sql_scan}; then
  {blocked_program}
fi"#
    )
}

// ---------------------------------------------------------------------------
// DatabaseBackupFileValidator
// ---------------------------------------------------------------------------

const ALLOWED_EXTENSIONS: &[&str] = &[
    "sql",
    "sql.gz",
    "gz",
    "zip",
    "tar",
    "tar.gz",
    "tgz",
    "dump",
    "bak",
    "bson",
    "bson.gz",
    "archive",
    "archive.gz",
    "bz2",
    "xz",
    "dmp",
    "db",
    "db.gz",
    "sqlite",
    "sqlite.gz",
    "sqlite3",
    "sqlite3.gz",
];
const DANGEROUS_EXTENSIONS: &[&str] = &[
    "asp", "aspx", "bat", "bash", "cgi", "cmd", "com", "exe", "htm", "html", "jar", "js", "jsp",
    "php", "php3", "php4", "php5", "phtml", "pl", "ps1", "py", "rb", "sh",
];

fn extension_for(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let mut sorted: Vec<&'static str> = ALLOWED_EXTENSIONS.to_vec();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.len()));
    for ext in sorted {
        let suffix = format!(".{ext}");
        let Some(stem) = lower.strip_suffix(&suffix) else {
            continue;
        };
        if stem.is_empty() || stem.ends_with('.') {
            return None;
        }
        if stem
            .split('.')
            .any(|p| !p.is_empty() && DANGEROUS_EXTENSIONS.contains(&p))
        {
            return None;
        }
        return Some(ext);
    }
    None
}

/// `DatabaseBackupFileValidator::hasAllowedExtension`.
pub fn has_allowed_extension(name: &str) -> bool {
    extension_for(name).is_some()
}

static PSQL_META: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r#"(?mi)(?:^|[^\\])(?:\\\\)*\\(?:!|(?:o|out|gx?|w|write)[^\S\n][^\\\n]*\||copy[^\S\n].*program|(?:setenv|e|edit|ef|ev|i|ir|include|include_relative)(?:\s|$)|(?:[^nbv\s\\]|[nbv][^\s\\])[^\s\\]*[^\S\n][^\\\n]*`)"#,
    )
    .unwrap_or_else(|_| unreachable!())
});
static SQL_PROGRAM: LazyLock<FancyRegex> = LazyLock::new(|| {
    let sep = r"(?:\s|/\*.*?\*/|--[^\r\n]*[\r\n])";
    FancyRegex::new(&format!(
        r"(?is)(?:from|to){sep}+program|lo_(?:import|export){sep}*\(|alter{sep}+system{sep}+(?:set|reset)"
    ))
    .unwrap_or_else(|_| unreachable!())
});

/// `containsPostgresqlProgramExecution`.
pub fn contains_postgres_program_execution(sql: &str) -> bool {
    if sql.starts_with("PGDMP") {
        return false;
    }
    let sql = without_copy_data(sql);
    PSQL_META.is_match(&sql).unwrap_or(true) || SQL_PROGRAM.is_match(&sql).unwrap_or(true)
}

/// `withoutCopyData()`: the table data of `COPY ... FROM stdin` blocks is removed, from the line
/// pg_dump writes (`COPY ... FROM stdin;`) to the line `\.`; rows are data, not SQL.
fn without_copy_data(sql: &str) -> String {
    static COPY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?m)^COPY .* FROM stdin;$").unwrap_or_else(|_| unreachable!())
    });
    static END: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?m)^\\\.$").unwrap_or_else(|_| unreachable!()));
    let mut kept = String::new();
    let mut offset = 0;
    while let Some(m) = COPY.find_at(sql, offset) {
        kept.push_str(&sql[offset..m.end()]);
        offset = END
            .find_at(sql, m.end())
            .map_or(sql.len(), |e| e.start() + 2);
    }
    kept.push_str(&sql[offset..]);
    kept
}

/// `contentMatchesExtension`.
fn content_matches(path: &Path, extension: &str, file_size: u64) -> bool {
    let Ok(sample) = read_head(path, 4096) else {
        return false;
    };
    let text = |s: &[u8]| !s.is_empty() && !s.contains(&0) && std::str::from_utf8(s).is_ok();
    match extension {
        "sql" => text(&sample),
        "sql.gz" | "gz" | "tar.gz" | "tgz" | "bson.gz" | "archive.gz" | "db.gz" | "sqlite.gz"
        | "sqlite3.gz" => sample.starts_with(&[0x1f, 0x8b]),
        "zip" => {
            sample.starts_with(b"PK\x03\x04")
                || sample.starts_with(b"PK\x05\x06")
                || sample.starts_with(b"PK\x07\x08")
        }
        "tar" => sample.get(257..262) == Some(b"ustar"),
        "bz2" => sample.starts_with(b"BZh"),
        "xz" => sample.starts_with(b"\xfd7zXZ\x00"),
        "dump" | "bak" | "archive" | "dmp" => sample.starts_with(b"PGDMP") || text(&sample),
        "bson" => {
            if sample.len() < 5 {
                return false;
            }
            let length = u64::from(u32::from_le_bytes([
                sample[0], sample[1], sample[2], sample[3],
            ]));
            length >= 5 && length <= file_size
        }
        "db" | "sqlite" | "sqlite3" => sample.starts_with(b"SQLite format 3\0"),
        _ => false,
    }
}

fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let mut read = 0;
    while read < n {
        let got = f.read(&mut buf[read..])?;
        if got == 0 {
            break;
        }
        read += got;
    }
    buf.truncate(read);
    Ok(buf)
}

/// `isUploadAllowed`.
pub fn is_upload_allowed(path: &Path, original_name: &str, max_bytes: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > max_bytes {
        return false;
    }
    extension_for(original_name).is_some_and(|e| content_matches(path, e, meta.len()))
}

/// The directory of a finished upload: `upload/imports/{team}/{uuid}/{upload_id}`.
pub fn upload_dir(data_dir: &Path, team_id: i64, uuid: &str, upload_id: &str) -> PathBuf {
    data_dir
        .join("upload")
        .join("imports")
        .join(team_id.to_string())
        .join(uuid)
        .join(upload_id)
}

// ---------------------------------------------------------------------------
// StartDatabaseImport
// ---------------------------------------------------------------------------

/// `assertServerPath` / `assertS3Path`: the forbidden characters.
fn unsafe_path(p: &str) -> bool {
    p.contains("..") || p.chars().any(|c| "$()`|;&><\r\n\0'\"\\".contains(c))
}

/// `$resource->defaultRestoreFile($backupName)`.
fn default_restore_file(files: &[String], backup_name: Option<&str>) -> Option<String> {
    let name = backup_name
        .map(|b| b.rsplit('/').next().unwrap_or(b))
        .unwrap_or_default();
    let mut matches: Vec<&String> = files
        .iter()
        .filter(|f| {
            name == f.as_str()
                || name.starts_with(&format!("{f}."))
                || name.starts_with(&format!("sqlite-backup-{f}-"))
        })
        .collect();
    matches.sort_by_key(|f| std::cmp::Reverse(f.len()));
    matches
        .first()
        .map(|f| (*f).clone())
        .or_else(|| files.first().cloned())
}

/// `databaseFiles()`.
fn sqlite_files(d: &DatabaseRow) -> Vec<String> {
    d.str("sqlite_databases")
        .unwrap_or_default()
        .split(',')
        .map(|f| f.trim().to_owned())
        .filter(|f| !f.is_empty() && super::backup::sqlite_file_valid(f))
        .collect()
}

impl Jobs {
    /// `StartDatabaseImport::handle`: queues the import; returns the activity id and its status.
    pub async fn start_database_import(
        &self,
        d: &DatabaseRow,
        source: ImportSource,
        team_id: i64,
        data_dir: &Path,
    ) -> Result<(i64, String), ImportError> {
        if !matches!(
            d.kind,
            Kind::Postgresql | Kind::Mysql | Kind::Mariadb | Kind::Mongodb | Kind::Sqlite
        ) {
            return fail("Database imports are not supported for this database type.");
        }
        if !d.str("status").unwrap_or_default().starts_with("running") {
            return fail("The database must be running before an import can start.");
        }
        if d.str("destination_type")
            .is_some_and(|t| t.contains("SwarmDocker"))
        {
            return Err(ImportError {
                message: "Database imports are not supported for Swarm servers yet.".to_owned(),
                status: 501,
            });
        }
        let sqlite_file = if d.kind == Kind::Sqlite {
            let files = sqlite_files(d);
            let chosen = source.sqlite_database.clone().or_else(|| {
                default_restore_file(
                    &files,
                    if source.kind == "upload" {
                        None
                    } else {
                        source.path.as_deref()
                    },
                )
            });
            match chosen {
                Some(f) if files.contains(&f) => Some(f),
                _ => {
                    return fail(
                        "The SQLite database file is not one of the files of this database.",
                    );
                }
            }
        } else {
            None
        };
        let server = self.server_for(d).await.map_err(|_| ImportError {
            message: "The database server or container is invalid.".to_owned(),
            status: 400,
        })?;
        if !self.reserve_database(&d.uuid) {
            return Err(ImportError {
                message: super::IN_PROGRESS.to_owned(),
                status: 409,
            });
        }
        let started = self
            .prepare_and_spawn_import(d, source, team_id, &server, sqlite_file, data_dir)
            .await;
        self.release_database(&d.uuid);
        started
    }

    async fn active_operation(&self, uuid: &str, operation: &str) -> Vec<i64> {
        dbs::active_activities(&self.0.db, uuid, operation)
            .await
            .unwrap_or_default()
    }

    async fn prepare_and_spawn_import(
        &self,
        d: &DatabaseRow,
        source: ImportSource,
        team_id: i64,
        server: &ServerInfo,
        sqlite_file: Option<String>,
        data_dir: &Path,
    ) -> Result<(i64, String), ImportError> {
        if !self
            .active_operation(&d.uuid, IMPORT_OPERATION)
            .await
            .is_empty()
        {
            return Err(ImportError {
                message: "A database import is already running.".to_owned(),
                status: 409,
            });
        }
        if !self
            .active_operation(&d.uuid, "database-start")
            .await
            .is_empty()
        {
            return Err(ImportError {
                message: super::IN_PROGRESS.to_owned(),
                status: 409,
            });
        }
        let t = server.target(self).await.map_err(|_| ImportError {
            message: "The database server or container is invalid.".to_owned(),
            status: 400,
        })?;
        let operation = uuid4();
        let container = d.container().to_owned();
        let network = d
            .destination
            .as_ref()
            .and_then(|x| x.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("coolify")
            .to_owned();
        let container_path = format!("/tmp/restore_{operation}");
        let script_path = format!("/tmp/restore_{operation}.sh");
        let mut commands: Vec<String> = Vec::new();
        let mut cleanup = json!({
            "container": container,
            "containerTmpPath": container_path,
            "scriptPath": script_path,
            "serverId": server.id,
            "operationUuid": operation,
        });
        match source.kind.as_str() {
            "upload" => {
                let upload = source.upload_id.clone().unwrap_or_default();
                let dir = upload_dir(data_dir, team_id, &d.uuid, &upload);
                let staged = dir.join("restore");
                if !staged.exists() {
                    return fail("The completed upload was not found.");
                }
                if d.kind == Kind::Postgresql && file_has_program_execution(&staged) {
                    let _ = std::fs::remove_file(&staged);
                    return fail(
                        "The uploaded backup contains disallowed PostgreSQL restore directives.",
                    );
                }
                let server_path = format!("/tmp/database-import-{operation}");
                let magic = read_head(&staged, 6).unwrap_or_default();
                let magic: String = magic.iter().map(|b| format!("{b:02x}")).collect();
                self.remote()
                    .copy_file(&t, &staged, &server_path)
                    .await
                    .map_err(|e| ImportError {
                        message: e.message,
                        status: 422,
                    })?;
                let _ = std::fs::remove_dir_all(&dir);
                commands.extend(
                    self.copy_into_container(
                        &server_path,
                        &magic,
                        &t,
                        &operation,
                        &container,
                        &container_path,
                        &mut cleanup,
                    )
                    .await?,
                );
                commands.push(format!("rm -f {}", quote(&server_path)));
                cleanup["serverTmpPath"] = json!(server_path);
            }
            "server" => {
                let path = source.path.clone().unwrap_or_default();
                if path.is_empty() || !path.starts_with('/') || unsafe_path(&path) {
                    return fail("The server path is invalid.");
                }
                if source.own_backups_only && !is_own_backup_path(d, team_id, &path) {
                    return own_backups_only_error();
                }
                let stat = format!("stat -c %s -- {}", quote(&path));
                let size: u64 = self
                    .remote()
                    .run(&t, &[stat.as_str()], RunOptions::op("database.import"))
                    .await
                    .map_err(|e| ImportError {
                        message: e.message,
                        status: 500,
                    })?
                    .and_then(|o| o.trim().parse().ok())
                    .unwrap_or(0);
                if !(1..=MAX_BYTES).contains(&size) {
                    return fail("The backup file is empty or exceeds the 10 GiB limit.");
                }
                let head = format!("head -c 6 -- {} | od -An -tx1", quote(&path));
                let magic = self
                    .remote()
                    .run(&t, &[head.as_str()], RunOptions::op("database.import"))
                    .await
                    .map_err(|e| ImportError {
                        message: e.message,
                        status: 500,
                    })?
                    .unwrap_or_default();
                commands.extend(
                    self.copy_into_container(
                        &path,
                        &magic,
                        &t,
                        &operation,
                        &container,
                        &container_path,
                        &mut cleanup,
                    )
                    .await?,
                );
            }
            _ => {
                let wanted = source.s3_storage_uuid.clone().unwrap_or_default();
                let storage = dbs::team_usable_s3(&self.0.db, team_id, &wanted)
                    .await
                    .ok()
                    .flatten();
                let Some(storage) = storage.filter(|r: &Record| {
                    valid_bucket_name(parts::str_of(r, "bucket").unwrap_or_default())
                }) else {
                    return fail("S3 storage was not found or has an invalid bucket.");
                };
                let key = source
                    .path
                    .clone()
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_owned();
                let name = key.rsplit('/').next().unwrap_or(&key);
                if key.is_empty() || unsafe_path(&key) || !has_allowed_extension(name) {
                    return fail("The S3 path is invalid.");
                }
                if source.own_backups_only && !is_own_backup_path(d, team_id, &key) {
                    return own_backups_only_error();
                }
                let enc = self.0.encrypter.as_ref().ok_or(ImportError {
                    message: "No application encryption key has been specified.".to_owned(),
                    status: 500,
                })?;
                let s3 = S3Target::from_row(enc, &storage).map_err(|e| ImportError {
                    message: e,
                    status: 500,
                })?;
                match s3.object_size(&key).await {
                    Ok(Some(size)) if size <= MAX_BYTES => {}
                    _ => return fail("The S3 backup was not found or exceeds the 10 GiB limit."),
                }
                let helper = format!("s3-restore-{operation}");
                self.start_s3_helper(&s3, &t, &helper, &network).await?;
                let source_arg = quote(&format!("s3temp/{}/{key}", s3.bucket));
                commands.push(format!(
                    "docker exec {} mc cp {source_arg} /tmp/restore",
                    quote(&helper)
                ));
                commands.extend(stream_from_helper(&helper, &container, &container_path));
                commands.push(format!(
                    "docker rm -f {} 2>/dev/null || true",
                    quote(&helper)
                ));
                cleanup["containerName"] = json!(helper);
            }
        }
        if d.kind == Kind::Postgresql {
            commands.push(format!(
                "docker exec {container} sh -c {}",
                quote(&postgres_scan_script(&container_path))
            ));
        }
        let restore = super::common::b64(&build_restore_command(
            d,
            &container_path,
            &source,
            sqlite_file.as_deref(),
        ));
        commands.push(format!(
            "echo {} | base64 -d > {}",
            quote(&restore),
            quote(&script_path)
        ));
        commands.push(format!("chmod +x {}", quote(&script_path)));
        commands.push(format!(
            "docker cp {} {}",
            quote(&script_path),
            quote(&format!("{container}:{script_path}"))
        ));
        commands.push(format!("rm -f {}", quote(&script_path)));
        commands.push(format!(
            "docker exec {} sh -c {} {}",
            quote(&container),
            quote(r#""$0"; status=$?; [ "$status" -ne 124 ] && [ "$status" -ne 255 ] || status=1; exit "$status""#),
            quote(&script_path)
        ));

        let props = json!({
            "server_uuid": server.uuid,
            "type": "inline",
            "type_uuid": d.uuid,
            "status": "queued",
            "team_id": server.team_id,
            "operation": IMPORT_OPERATION,
            "resource_kind": if d.is_service() { "service_database" } else { "standalone_database" },
            "operation_uuid": operation,
            "cleanup": cleanup,
        });
        let activity = dbs::insert_activity(&self.0.db, d.morph(), d.id, &props, now())
            .await
            .map_err(|e| ImportError {
                message: e.to_string(),
                status: 500,
            })?;
        self.spawn_import(d.clone(), t, commands, activity, cleanup);
        Ok((activity, "queued".to_owned()))
    }

    fn spawn_import(
        &self,
        d: DatabaseRow,
        t: Target,
        commands: Vec<String>,
        activity: i64,
        cleanup: Value,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let _ = dbs::update_activity(
                &jobs.0.db,
                activity,
                &json!({"status": "in_progress"}),
                None,
                now(),
            )
            .await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
            let db = jobs.0.db.clone();
            let writer = tokio::spawn(async move {
                let mut entries: Vec<Value> = Vec::new();
                while let Some((stream, chunk)) = rx.recv().await {
                    let output = coolify_compat::redact::redact_text(&chunk, &[], &[]);
                    if output.is_empty() {
                        continue;
                    }
                    entries.push(json!({"type": stream.as_str(), "output": output, "timestamp": hrtime(), "batch": 1, "order": entries.len() + 1}));
                    let _ = dbs::update_activity(
                        &db,
                        activity,
                        &json!({}),
                        Some(&serde_json::to_string(&entries).unwrap_or_default()),
                        now(),
                    )
                    .await;
                }
            });
            let result = jobs
                .remote()
                .run_streaming(
                    &t,
                    &commands,
                    RunOptions::op("database.import").timeout(Duration::from_secs(3600)),
                    tx,
                )
                .await;
            let _ = writer.await;
            let merge = match &result {
                Ok(_) => json!({"status": "finished", "exitCode": 0}),
                Err(e) => json!({"status": "error", "exitCode": e.code}),
            };
            let _ = dbs::update_activity(&jobs.0.db, activity, &merge, None, now()).await;
            if let Err(e) = result
                && matches!(e.code, Some(124 | 255))
            {
                tracing::warn!(database = %d.uuid, code = ?e.code, "database import ended without the result of the restore");
            }
            jobs.cleanup_import(&t, &cleanup).await;
        });
    }

    /// `CleanupDatabaseImport`: helper containers and temporary files.
    async fn cleanup_import(&self, t: &Target, cleanup: &Value) {
        let text = |k: &str| cleanup.get(k).and_then(Value::as_str).map(str::to_owned);
        let mut commands: Vec<String> = Vec::new();
        if let Some(helper) = text("containerName") {
            commands.push(format!(
                "docker rm -f {} >/dev/null 2>&1 || true",
                quote(&helper)
            ));
        }
        if let (Some(container), Some(path)) = (text("container"), text("containerTmpPath")) {
            commands.push(format!(
                "docker exec {} rm -f {} {} >/dev/null 2>&1 || true",
                quote(&container),
                quote(&path),
                quote(&format!("{path}.sh"))
            ));
        }
        if let Some(path) = text("serverTmpPath") {
            commands.push(format!("rm -f {} >/dev/null 2>&1 || true", quote(&path)));
        }
        if let Some(path) = text("scriptPath") {
            commands.push(format!("rm -f {} >/dev/null 2>&1 || true", quote(&path)));
        }
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.remote()
            .run_quiet(t, &refs, RunOptions::op("database.import"))
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn copy_into_container(
        &self,
        path: &str,
        magic: &str,
        t: &Target,
        operation: &str,
        container: &str,
        container_path: &str,
        cleanup: &mut Value,
    ) -> Result<Vec<String>, ImportError> {
        let magic: String = magic
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect::<String>()
            .to_lowercase();
        if !(magic.starts_with("425a68")
            || magic.starts_with("fd377a585a00")
            || magic.starts_with("504b0304"))
        {
            return Ok(vec![format!(
                "docker cp {} {}",
                quote(path),
                quote(&format!("{container}:{container_path}"))
            )]);
        }
        let helper = format!("backup-decompress-{operation}");
        let image = quote(&helper_image(self));
        let start = [
            format!("docker rm -f {} 2>/dev/null || true", quote(&helper)),
            format!(
                "docker run -d --network none --name {} {image} sleep 86400",
                quote(&helper)
            ),
        ];
        let refs: Vec<&str> = start.iter().map(String::as_str).collect();
        if self
            .remote()
            .run(t, &refs, RunOptions::op("database.import"))
            .await
            .is_err()
        {
            let remove = format!("docker rm -f {} 2>/dev/null || true", quote(&helper));
            self.remote()
                .run_quiet(t, &[remove.as_str()], RunOptions::op("database.import"))
                .await;
            return fail("Unable to start the backup decompression helper.");
        }
        cleanup["containerName"] = json!(helper);
        let mut out = vec![format!(
            "docker cp {} {}",
            quote(path),
            quote(&format!("{helper}:/tmp/restore"))
        )];
        out.extend(stream_from_helper(&helper, container, container_path));
        out.push(format!(
            "docker rm -f {} 2>/dev/null || true",
            quote(&helper)
        ));
        Ok(out)
    }

    /// `startS3Helper()`: starts the S3 restore helper and sets its `s3temp` mc alias. The S3
    /// keys reach mc on its stdin, so they are not in the arguments of any process or in the
    /// helper's environment. Upstream pipes them into a separate ssh process; here they travel
    /// inside the script (like every command input of coolify-rs) through the shell's builtin
    /// `echo`. The helper is removed when either step fails.
    async fn start_s3_helper(
        &self,
        s3: &S3Target,
        t: &Target,
        helper: &str,
        network: &str,
    ) -> Result<(), ImportError> {
        use base64::Engine as _;
        let image = quote(&helper_image(self));
        let start = [
            format!("docker rm -f {} 2>/dev/null || true", quote(helper)),
            format!(
                "docker run -d --network {} --name {} -e {} {image} sleep 86400",
                quote(network),
                quote(helper),
                quote(&format!("MC_REGION={}", s3.region)),
            ),
        ];
        let refs: Vec<&str> = start.iter().map(String::as_str).collect();
        let keys = format!("{}\n{}\n", s3.key, s3.secret);
        let alias = format!(
            "echo '{}' | base64 -d | docker exec -i {} mc alias set{} s3temp {} >/dev/null",
            base64::engine::general_purpose::STANDARD.encode(keys.as_bytes()),
            quote(helper),
            super::s3::minio_client_alias_options(s3.path_style),
            quote(&s3.endpoint)
        );
        let started = self
            .remote()
            .run(t, &refs, RunOptions::op("database.import"))
            .await
            .is_ok()
            && self
                .remote()
                .run(t, &[alias.as_str()], RunOptions::op("database.import"))
                .await
                .is_ok();
        if !started {
            let remove = format!("docker rm -f {} 2>/dev/null || true", quote(helper));
            self.remote()
                .run_quiet(t, &[remove.as_str()], RunOptions::op("database.import"))
                .await;
            return fail("Unable to start the S3 restore helper.");
        }
        Ok(())
    }
}

fn stream_from_helper(helper: &str, container: &str, container_path: &str) -> Vec<String> {
    let target = quote(container_path);
    let writer = format!(
        "cat > {target} && [ -s {target} ] || {{ echo 'The backup is empty or could not be read.' >&2; exit 1; }}"
    );
    vec![
        format!(
            "docker exec {} sh -c {}",
            quote(helper),
            quote(&build_normalize_script(
                "/tmp/restore",
                "/tmp/restore.prepared"
            ))
        ),
        format!(
            "docker exec {} cat /tmp/restore.prepared | docker exec -i {} sh -c {}",
            quote(helper),
            quote(container),
            quote(&writer)
        ),
    ]
}

/// `fileContainsPostgresqlProgramExecution`: a plain or gzip file, the first 50 MiB.
fn file_has_program_execution(path: &Path) -> bool {
    let Ok(head) = read_head(path, 2) else {
        return false;
    };
    if head == [0x1f, 0x8b] {
        // The remote scan reads gzip files; the local check covers plain files.
        return false;
    }
    let Ok(sample) = read_head(path, 50 * 1024 * 1024) else {
        return false;
    };
    contains_postgres_program_execution(&String::from_utf8_lossy(&sample))
}

/// `Str::uuid()`: version 4.
pub(super) fn uuid4() -> String {
    let mut b: [u8; 16] = rand::thread_rng().r#gen();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..].concat()
    )
}

/// Laravel `Carbon` JSON of a stored timestamp.
pub fn activity_time(s: &str) -> String {
    coolify_db::records::laravel_datetime(s)
}

/// The time the import stored a failure at.
pub fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert!(has_allowed_extension("a.sql.gz"));
        assert!(!has_allowed_extension("a.php.sql"));
        assert!(!has_allowed_extension("a.txt"));
        assert!(!has_allowed_extension(".sql"));
    }

    #[test]
    fn program_scan() {
        assert!(contains_postgres_program_execution(
            "COPY t FROM PROGRAM 'id';"
        ));
        assert!(contains_postgres_program_execution("\\! id\n"));
        assert!(!contains_postgres_program_execution("SELECT 1;"));
        assert!(!contains_postgres_program_execution(
            "PGDMP anything from program"
        ));
    }

    #[test]
    fn copy_data_is_not_scanned() {
        let dump = "CREATE TABLE t (a text);\nCOPY public.t (a) FROM stdin;\nhow to program\n\\.\nSELECT 1;\n";
        assert!(!contains_postgres_program_execution(dump));
        assert_eq!(
            without_copy_data(dump),
            "CREATE TABLE t (a text);\nCOPY public.t (a) FROM stdin;\nSELECT 1;\n"
        );
        // SQL after the data block is still scanned.
        assert!(contains_postgres_program_execution(&format!(
            "{dump}COPY t FROM PROGRAM 'id';\n"
        )));
        let script = postgres_scan_script("/tmp/x.sql");
        assert!(script.starts_with("command -v awk >/dev/null 2>&1 || { echo 'Blocked PostgreSQL restore: awk is required to scan the backup.'; exit 1; }\n"));
        assert!(script.contains(
            r#"{ { gunzip -cf '/tmp/x.sql' 2>/dev/null || cat '/tmp/x.sql'; } | LC_ALL=C awk 'data { if ($0 == "\\.") data = 0; next } { print } /^COPY .* FROM stdin;$/ { data = 1 }' || echo '\! from program'; } | LC_ALL=C grep -Eiq "#
        ));
    }

    #[test]
    fn mysql_restores_refuse_client_commands() {
        let script = mysql_script("mariadb", "MARIADB", false, false);
        assert!(script.contains(
            "if stream | LC_ALL=C grep -Eq '(^|[^\\\\])(\\\\\\\\)*\\\\[!TPe.]' || stream | LC_ALL=C grep -Eiq '^[[:space:]]*(system|tee|pager|edit|source)([[:space:]]|;|$)'; then\n  fail 'Blocked restore: the backup contains a mariadb client command"
        ));
        assert!(script.contains("client_safety=\nif mariadb --help 2>/dev/null | grep -q -e '--sandbox'; then\n  client_safety=--sandbox\nelif mariadb --help 2>/dev/null | grep -q -e '--commands'; then\n  client_safety=--commands=FALSE\nfi\nif [ "));
        assert!(script.ends_with(
            "stream | mariadb $client_safety -u $MARIADB_USER -p$MARIADB_PASSWORD $MARIADB_DATABASE"
        ));
        let all = mysql_script("mysql", "MYSQL", true, false);
        assert!(all.contains("fi\ncommand -v awk >/dev/null 2>&1 || fail 'awk is required to skip the system databases of the backup. Nothing was changed.'\nclient_safety=\n"));
        assert!(all.ends_with(
            "|| echo 'Coolify could not filter the system databases of the backup;'; } | mysql $client_safety -u root -p${MYSQL_ROOT_PASSWORD} ${MYSQL_DATABASE:-default}"
        ));
        // The patterns as grep -E sees them.
        let matches = |pattern: &str, flags: &str, text: &str| {
            std::process::Command::new("grep")
                .args([flags, pattern])
                .env("LC_ALL", "C")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write as _;
                    child.stdin.take().map(|mut i| i.write_all(text.as_bytes()));
                    child.wait()
                })
                .is_ok_and(|s| s.success())
        };
        for blocked in ["\\! id\n", "SELECT 1;\\T /tmp/x\n", "a\\\\\\. x\n"] {
            assert!(
                matches(MYSQL_CLIENT_COMMAND_PATTERN, "-Eq", blocked),
                "{blocked}"
            );
        }
        for data in [
            "INSERT INTO t VALUES ('a\\\\!b');\n",
            "INSERT INTO t VALUES ('\\n');\n",
        ] {
            assert!(
                !matches(MYSQL_CLIENT_COMMAND_PATTERN, "-Eq", data),
                "{data}"
            );
        }
        assert!(matches(
            MYSQL_NAMED_CLIENT_COMMAND_PATTERN,
            "-Eiq",
            "  SYSTEM id\n"
        ));
        assert!(!matches(
            MYSQL_NAMED_CLIENT_COMMAND_PATTERN,
            "-Eiq",
            "INSERT INTO system VALUES (1);\n"
        ));
    }

    #[test]
    fn restore_files() {
        let files = vec!["a.sqlite".to_owned(), "b.sqlite".to_owned()];
        assert_eq!(
            default_restore_file(&files, Some("/x/sqlite-backup-b.sqlite-1-u.gz")).as_deref(),
            Some("b.sqlite")
        );
        assert_eq!(
            default_restore_file(&files, None).as_deref(),
            Some("a.sqlite")
        );
    }
}
