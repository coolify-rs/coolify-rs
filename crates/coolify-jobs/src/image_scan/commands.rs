//! `App\Services\ImageScan\ScannerCommands`: the shell commands that run Syft, Grype, and regctl
//! on a server.
//!
//! Each method returns one command list for `instant_remote_process()` (one `bash -se` batch).
//! The lists work as root and through `parse_commands_by_line_for_sudo()`, and they are safe to
//! replay after an SSH retry: each list first removes the container `coolify-scan-{runId}` and
//! the work directory, and writes the SBOM to `{name}.json.tmp-{runId}` before it moves it into
//! place.
//!
//! Output contract (stdout of the batch):
//! - Every tool run prints one marker line `coolify-scan-exit {step} {code}`; steps that produce
//!   output add the full output size in bytes: `coolify-scan-exit {step} {code} {bytes}`.
//!   `{code}` is the exit code of the run: 0 success, 124 timeout, 137 killed (timeout kill or
//!   memory limit), 125 `docker run` failed (for example the image could not be pulled), any
//!   other value is an error of the tool. A tool whose output is captured (Syft, Grype, regctl)
//!   runs without `--rm`: after it stops, Docker's `State.OOMKilled` decides whether the memory
//!   limit stopped it (line `coolify-scan-oom {step}`, before the exit marker), and the container
//!   is removed in the same script. Only that line means "out of memory"; a 137 without it was a
//!   `timeout` kill. Steps: `pull-syft`, `pull-grype`, `pull-regctl` (prepare), `pull` (pull),
//!   `db-update`, `db-status`, `disk-db` and `disk-sboms` (disk_usage, size = bytes), `inspect`,
//!   `save` and `sbom` (sbom_from_local), `sbom` (sbom_from_registry), `copy` and `sbom`
//!   (sbom_from_host), `grype`, `packages`, `tags`, `digest`.
//! - A failed run does not stop the batch, and the container and the work directory are still
//!   removed.
//! - Methods with a payload print the line `coolify-scan-payload` after the markers, then at most
//!   10485761 bytes of the tool output (`head -c`), so the output is cut when
//!   `bytes > OUTPUT_LIMIT`. The file that holds the output stops at 20 MiB (`ulimit -f`); a run
//!   that reached it reports exit code 0 with the size of the file, so it reads as cut output,
//!   not as a failed run. Use [`parse_output`]: it reads markers only before the separator, so
//!   the payload cannot fake one. Only setup commands (mkdir, chown, chmod) stop the batch when
//!   they fail; call [`ScannerCommands::cleanup`] after every scan, also after an error.
//! - `inspect`: one line `{Id}|{RepoDigest or empty}|{os}/{arch}|{size}`. `disk_free`:
//!   `{docker root free} {scan dir free}` in bytes.
//!
//! Containers get only a scratch directory as writable mount. The host never reads, moves, or
//! changes the owner of a file that a container could write: tool output goes through stdout into
//! root-owned files, and the work directory is removed with `rm -rf`. A `trap` cannot be used:
//! the sudo parser would turn it (and `W=$(mktemp ...)`) into `sudo trap`, so the work directory
//! is `/data/coolify/image-scans/tmp-{runId}`.

use std::fmt;
use std::sync::LazyLock;

use base64::Engine as _;
use coolify_compat::php_json;
use coolify_db::image_scans::ServerScanSettings;
use coolify_remote::shell::quote;
use regex::Regex;
use serde_json::Value;

use super::image_ref::{ImageRef, InvalidImageRef};

pub const BASE_DIR: &str = "/data/coolify/image-scans";
pub const SBOM_DIR: &str = "/data/coolify/image-scans/sboms";
pub const DB_VOLUME: &str = "coolify-grype-db";
pub const OUTPUT_LIMIT: usize = 10_485_760;
/// Tool output written to disk stops at this size (`ulimit -f`, in the 512-byte blocks that dash,
/// bash in POSIX mode, and BusyBox sh use): 20 MiB for captured output, 1 GiB for an SBOM.
pub const OUTPUT_FILE_LIMIT_BLOCKS: usize = 40_960;
pub const SBOM_FILE_LIMIT_BLOCKS: usize = 2_097_152;
pub const MARKER: &str = "coolify-scan-exit";
pub const PAYLOAD_SEPARATOR: &str = "coolify-scan-payload";
pub const OOM_MARKER: &str = "coolify-scan-oom";
/// SBOM name of the server OS packages (`sboms/host.json`); image SBOMs are named by their
/// 64-character ID.
pub const HOST_SBOM: &str = "host";

/// The os-release files and the package databases that the OS catalogers read.
const HOST_PACKAGE_PATHS: [&str; 8] = [
    "/etc/os-release",
    "/usr/lib/os-release",
    "/var/lib/dpkg/status",
    "/var/lib/dpkg/status.d",
    "/lib/apk/db/installed",
    "/var/lib/rpm",
    "/usr/lib/sysimage/rpm",
    "/var/lib/pacman/local",
];
const HOST_PACKAGE_DATABASES: [&str; 6] = [
    "/var/lib/dpkg/status",
    "/var/lib/dpkg/status.d",
    "/lib/apk/db/installed",
    "/var/lib/rpm",
    "/usr/lib/sysimage/rpm",
    "/var/lib/pacman/local",
];
const HOST_CATALOGERS: &str =
    "dpkg-db-cataloger,rpm-db-cataloger,apk-db-cataloger,alpm-db-cataloger";
const SCANNER_USER: &str = "65534:65534";

/// `docker save` of a local image. Syft has a time limit per server, see `syft_timeout()`.
const TIMEOUT_SAVE: i64 = 600;
/// Syft `convert` of a stored SBOM. Grype has a time limit per server, see `grype_timeout()`.
const TIMEOUT_PACKAGES: i64 = 300;
/// Highest Syft and Grype time limits a server can set
/// (`ServerSetting::IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS`).
pub const MAX_SYFT_TIMEOUT: i64 = 3600;
pub const MAX_GRYPE_TIMEOUT: i64 = 3600;
const TIMEOUT_REGISTRY: i64 = 30;
const TIMEOUT_DB_UPDATE: i64 = 900;
const TIMEOUT_DB_STATUS: i64 = 60;
const TIMEOUT_PULL: i64 = 600;
const TIMEOUT_VOLUME_SETUP: i64 = 120;

/// `ServerSetting::DEFAULT_IMAGE_SCAN_MEMORY_MB`.
pub const DEFAULT_IMAGE_SCAN_MEMORY_MB: i64 = 1024;
/// `ServerSetting::IMAGE_SCAN_MEMORY_OPTIONS_MB`.
pub const IMAGE_SCAN_MEMORY_OPTIONS_MB: [i64; 4] = [1024, 2048, 4096, 8192];
/// `ServerSetting::DEFAULT_IMAGE_SCAN_TIMEOUT_SECONDS`: the time limit of one Syft run and of one
/// Grype run.
pub const DEFAULT_IMAGE_SCAN_TIMEOUT_SECONDS: i64 = 600;
/// `ServerSetting::IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS`.
pub const IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS: [i64; 5] = [300, 600, 1200, 1800, 3600];
/// `ServerSetting::DEFAULT_IMAGE_SCAN_CPUS`: CPUs of each scanner container.
pub const DEFAULT_IMAGE_SCAN_CPUS: f64 = 1.0;
/// `ServerSetting::IMAGE_SCAN_CPU_OPTIONS`.
pub const IMAGE_SCAN_CPU_OPTIONS: [f64; 4] = [0.5, 1.0, 2.0, 4.0];

/// A scanner image of `config('constants.image_scan.images')`. Scanner images run on managed
/// servers, pinned by digest. Change them only with a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerImage {
    pub tool: &'static str,
    pub repo: &'static str,
    pub version: &'static str,
    pub digest: &'static str,
}

/// `config('constants.image_scan.images')`: syft, grype, regctl, init.
pub const SCANNER_IMAGES: [ScannerImage; 4] = [
    ScannerImage {
        tool: "syft",
        repo: "anchore/syft",
        version: "1.55.0",
        digest: "sha256:4cc92b64ca74ffca4c94d4c8fcc01f217731375300be14d0b6eeb2e82eaa110a",
    },
    ScannerImage {
        tool: "grype",
        repo: "anchore/grype",
        version: "0.121.0",
        digest: "sha256:621d56d1d68106b787022c81546f7e0a36202a25d1239f0c20195fab8c16f7ea",
    },
    ScannerImage {
        tool: "regctl",
        repo: "regclient/regctl",
        version: "0.11.7",
        digest: "sha256:45b57933c2b67bb1e280b4baaa220ef1ab212c81268ab19589fa1e83719c7c82",
    },
    ScannerImage {
        tool: "init",
        repo: "alpine",
        version: "3.22.7",
        digest: "sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f",
    },
];

/// `resources/image-scan/grype.tmpl` and `syft.tmpl`, byte for byte.
pub const GRYPE_TEMPLATE: &str = include_str!("templates/grype.tmpl");
pub const SYFT_TEMPLATE: &str = include_str!("templates/syft.tmpl");

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

static RUN_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-z0-9]{8,32}\z"));
static IMAGE_HEX: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-f0-9]{64}\z"));
static IMAGE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\Asha256:([a-f0-9]{64})\z"));
static PLATFORM: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-z0-9]+/[a-z0-9_]+(/[a-z0-9]+)?\z"));
static SBOM_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-z0-9][a-z0-9-]{0,99}\z"));
static BASE64: LazyLock<Regex> = LazyLock::new(|| re(r"\A[A-Za-z0-9+/]+={0,2}\z"));
static OOM_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"\Acoolify-scan-oom ([a-z-]+)\z"));
static MARKER_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\Acoolify-scan-exit ([a-z-]+) (\d+)(?: +(\d+))?\z"));
static URL_SCHEME: LazyLock<Regex> = LazyLock::new(|| re(r"\Ahttps?://"));

/// `InvalidArgumentException` of `ScannerCommands`: a value that must not reach a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidArgument(pub String);

impl fmt::Display for InvalidArgument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidArgument {}

impl From<InvalidImageRef> for InvalidArgument {
    fn from(e: InvalidImageRef) -> Self {
        Self(e.0.to_owned())
    }
}

fn invalid(message: &str) -> InvalidArgument {
    InvalidArgument(message.to_owned())
}

/// What the commands read of a server: its SSH user, the scanner limits of its settings, and its
/// size (`server_metadata`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScannerHost {
    /// `$server->isNonRoot()`.
    pub non_root: bool,
    /// `$server->settings?->image_scan_memory_mb`.
    pub memory_mb: Option<i64>,
    /// `$server->settings?->image_scan_syft_timeout`.
    pub syft_timeout: Option<i64>,
    /// `$server->settings?->image_scan_grype_timeout`.
    pub grype_timeout: Option<i64>,
    /// `$server->settings?->image_scan_cpus`.
    pub cpus: Option<f64>,
    /// `data_get($server->server_metadata, 'memory_bytes', 0)`.
    pub total_memory_bytes: i64,
    /// `data_get($server->server_metadata, 'cpus', 0)`.
    pub cpu_count: i64,
}

impl Default for ScannerHost {
    /// A root server without settings and of unknown size.
    fn default() -> Self {
        Self {
            non_root: false,
            memory_mb: None,
            syft_timeout: None,
            grype_timeout: None,
            cpus: None,
            total_memory_bytes: 0,
            cpu_count: 0,
        }
    }
}

/// `optionsUpTo()`: the options that fit the capacity. All options while the capacity is not
/// known; at least the smallest option.
fn options_up_to<T: Copy + PartialOrd>(
    options: &[T],
    capacity: i64,
    as_f64: impl Fn(T) -> f64,
) -> Vec<T> {
    if capacity <= 0 {
        return options.to_vec();
    }
    let fitting: Vec<T> = options
        .iter()
        .copied()
        .filter(|o| as_f64(*o) <= capacity as f64)
        .collect();
    if fitting.is_empty() {
        options.first().copied().into_iter().collect()
    } else {
        fitting
    }
}

/// `clampTimeout()`.
fn clamp_timeout(seconds: Option<i64>) -> i64 {
    let options = IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS;
    seconds
        .unwrap_or(DEFAULT_IMAGE_SCAN_TIMEOUT_SECONDS)
        .clamp(options[0], options[options.len() - 1])
}

impl ScannerHost {
    /// The host of a server row with its scan settings (`None`: no settings row).
    pub fn new(
        non_root: bool,
        settings: Option<&ServerScanSettings>,
        server_metadata: Option<&Value>,
    ) -> Self {
        let metadata = |key: &str| {
            server_metadata
                .and_then(|m| m.get(key))
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .unwrap_or(0)
        };
        Self {
            non_root,
            memory_mb: settings.map(|s| s.image_scan_memory_mb),
            syft_timeout: settings.map(|s| s.image_scan_syft_timeout),
            grype_timeout: settings.map(|s| s.image_scan_grype_timeout),
            cpus: settings.map(|s| s.image_scan_cpus),
            total_memory_bytes: metadata("memory_bytes"),
            cpu_count: metadata("cpus"),
        }
    }

    /// `ScannerCommands::syftTimeout($server)`: seconds that one Syft SBOM run may take on the
    /// server; a stored value outside the options is clamped.
    pub fn syft_timeout(&self) -> i64 {
        clamp_timeout(self.syft_timeout)
    }

    /// `ScannerCommands::grypeTimeout($server)`: seconds that one Grype run may take.
    pub fn grype_timeout(&self) -> i64 {
        clamp_timeout(self.grype_timeout)
    }

    /// `ScannerCommands::cpus($server)`: CPUs of one scanner container, as the
    /// `docker run --cpus` value.
    pub fn cpus(&self) -> String {
        let options = IMAGE_SCAN_CPU_OPTIONS;
        let mut cpus = self
            .cpus
            .unwrap_or(DEFAULT_IMAGE_SCAN_CPUS)
            .clamp(options[0], options[options.len() - 1]);
        // Docker refuses `--cpus` above the CPU count, for example after the server got smaller.
        let highest = self.cpu_options().into_iter().fold(f64::MIN, f64::max);
        cpus = cpus.min(highest);
        let text = format!("{cpus:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }

    /// `ScannerCommands::memoryOptionsMb($server)`: the memory options, in MB, that fit the total
    /// memory of the server.
    pub fn memory_options_mb(&self) -> Vec<i64> {
        options_up_to(
            &IMAGE_SCAN_MEMORY_OPTIONS_MB,
            self.total_memory_bytes / (1024 * 1024),
            |o| o as f64,
        )
    }

    /// `ScannerCommands::cpuOptions($server)`: the CPU options that fit the CPU count of the
    /// server (Docker refuses `--cpus` above it).
    pub fn cpu_options(&self) -> Vec<f64> {
        options_up_to(&IMAGE_SCAN_CPU_OPTIONS, self.cpu_count, |o| o)
    }

    /// `memoryMb()`.
    fn memory(&self) -> i64 {
        self.memory_mb.unwrap_or(DEFAULT_IMAGE_SCAN_MEMORY_MB)
    }
}

/// `serverDockerConfigPrefix($server)`: the registry login of a non-root SSH user. No `&&` or
/// pipe: with them, the sudo parser can wrap the line in `sudo bash -c`, where `$HOME` is `/root`.
fn server_docker_config_prefix(non_root: bool) -> &'static str {
    if non_root {
        r#"DOCKER_CONTEXT=default DOCKER_CONFIG="$(find "$HOME/.docker/config.json" -maxdepth 0 -exec dirname {} \; 2>/dev/null)" "#
    } else {
        ""
    }
}

/// The marker lines of a batch and its payload, see [`parse_output`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedOutput {
    /// The exit code of each step, in the order the steps first reported.
    pub exit_codes: Vec<(String, i64)>,
    /// The output size in bytes of the steps that report one.
    pub sizes: Vec<(String, i64)>,
    /// The steps whose container Docker stopped at the memory limit.
    pub oom: Vec<String>,
    pub payload: String,
}

impl ParsedOutput {
    pub fn exit_code(&self, step: &str) -> Option<i64> {
        self.exit_codes
            .iter()
            .find(|(s, _)| s == step)
            .map(|(_, code)| *code)
    }

    pub fn size(&self, step: &str) -> Option<i64> {
        self.sizes
            .iter()
            .find(|(s, _)| s == step)
            .map(|(_, size)| *size)
    }

    /// `json_encode($result['exit_codes'])`.
    pub fn exit_codes_json(&self) -> String {
        if self.exit_codes.is_empty() {
            return "[]".to_owned();
        }
        let map: serde_json::Map<String, Value> = self
            .exit_codes
            .iter()
            .map(|(step, code)| (step.clone(), Value::from(*code)))
            .collect();
        php_json::encode(&Value::Object(map))
    }
}

fn put(list: &mut Vec<(String, i64)>, key: &str, value: i64) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => list.push((key.to_owned(), value)),
    }
}

/// `ScannerCommands::parseOutput($output)`: splits batch output into the marker lines before
/// `coolify-scan-payload` and the payload after it.
pub fn parse_output(output: Option<&str>) -> ParsedOutput {
    let lines: Vec<&str> = output
        .unwrap_or_default()
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let separator = lines.iter().position(|line| *line == PAYLOAD_SEPARATOR);
    let mut parsed = ParsedOutput::default();
    for line in &lines[..separator.unwrap_or(lines.len())] {
        if let Some(c) = OOM_LINE.captures(line) {
            if !parsed.oom.iter().any(|step| step == &c[1]) {
                parsed.oom.push(c[1].to_owned());
            }
            continue;
        }
        let Some(c) = MARKER_LINE.captures(line) else {
            continue;
        };
        put(
            &mut parsed.exit_codes,
            &c[1],
            c[2].parse().unwrap_or(i64::MAX),
        );
        if let Some(size) = c.get(3) {
            put(
                &mut parsed.sizes,
                &c[1],
                size.as_str().parse().unwrap_or(i64::MAX),
            );
        }
    }
    if let Some(separator) = separator {
        parsed.payload = lines[separator + 1..].join("\n");
    }
    parsed
}

/// A Docker config with one registry in `auths`, after validation.
#[derive(Debug, Clone, PartialEq)]
struct AuthConfig(Value);

/// `json_decode($json, true)` then `json_encode()` turns an empty object into an empty list.
fn php_array_round_trip(value: Value) -> Value {
    match value {
        Value::Object(map) if map.is_empty() => Value::Array(Vec::new()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, php_array_round_trip(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(php_array_round_trip).collect()),
        other => other,
    }
}

/// `credentialsMatchRegistry($key, $registry)`.
fn credentials_match_registry(key: &str, registry: &str) -> bool {
    let host = URL_SCHEME.replace(key, "");
    let host = host.trim_end_matches('/');
    if registry == "docker.io" {
        return [
            "index.docker.io/v1",
            "index.docker.io",
            "docker.io",
            "registry-1.docker.io",
        ]
        .contains(&host);
    }
    host == registry
}

/// `authConfig($authConfigB64, $image)`: accepts a Docker config with one registry in `auths` and
/// nothing else (no credential helpers). The registry must be the registry of the image.
fn auth_config(
    auth_config_b64: Option<&str>,
    image: &ImageRef,
) -> Result<Option<AuthConfig>, InvalidArgument> {
    let Some(encoded) = auth_config_b64 else {
        return Ok(None);
    };
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let config: Option<Value> = BASE64
        .is_match(encoded)
        .then(|| engine.decode(encoded).ok())
        .flatten()
        .and_then(|json| serde_json::from_slice(&json).ok());
    let wrong = || invalid("The registry credentials must be a Docker config for one registry.");
    let Some(Value::Object(config)) = config else {
        return Err(wrong());
    };
    if config.len() != 1 {
        return Err(wrong());
    }
    let (key, first) = match config.get("auths") {
        Some(Value::Object(auths)) if auths.len() == 1 => match auths.iter().next() {
            Some((key, first)) => (key.clone(), first),
            None => return Err(wrong()),
        },
        // A JSON list with one entry has the key 0.
        Some(Value::Array(auths)) if auths.len() == 1 => ("0".to_owned(), &auths[0]),
        _ => return Err(wrong()),
    };
    if !(first.is_object() || first.is_array()) {
        return Err(wrong());
    }
    if !credentials_match_registry(&key, &image.registry) {
        return Err(invalid(
            "The registry credentials are not for the registry of the image.",
        ));
    }
    Ok(Some(AuthConfig(php_array_round_trip(Value::Object(
        config,
    )))))
}

/// The awk program of the host filter, see [`ScannerCommands::grype`].
const HOST_DEDUPE: &str = concat!(
    r#"{ if (!match($0, /"id":"[^"]*"/)) { line[++n] = $0; next }"#,
    " ",
    r#"k = substr($0, RSTART, RLENGTH); image = 0;"#,
    " ",
    r#"if (match($0, /"pkg":"[^"]*"/)) { p = substr($0, RSTART, RLENGTH);"#,
    " ",
    r#"if (p ~ /^"pkg":"(linux|kernel)[-"]/) { image = (p ~ /^"pkg":"(linux-(image|modules)-[^"]*|kernel(-core)?)"$/); p = "kernel" } k = k p }"#,
    " ",
    r#"if (match($0, /"ver":"[^"]*"/)) k = k substr($0, RSTART, RLENGTH);"#,
    " ",
    r#"if (match($0, /"fix":"[^"]*"/)) k = k substr($0, RSTART, RLENGTH);"#,
    " ",
    r#"if (match($0, /"fixv":\[[^]]*\]/)) k = k substr($0, RSTART, RLENGTH);"#,
    " ",
    r#"if (k in at) { if (image && !isImage[k]) { line[at[k]] = $0; isImage[k] = 1 } next }"#,
    " ",
    r#"at[k] = ++n; isImage[k] = image; line[n] = $0 }"#,
    " ",
    r#"END { for (i = 1; i <= n; i++) print line[i] }"#,
);

/// The command lists of one scan run on one server.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannerCommands {
    pub host: ScannerHost,
    pub run_id: String,
}

impl ScannerCommands {
    /// `new ScannerCommands($server, $runId)`: the run ID is 8 to 32 lowercase letters and digits.
    pub fn new(host: ScannerHost, run_id: &str) -> Result<Self, InvalidArgument> {
        if !RUN_ID.is_match(run_id) {
            return Err(invalid("Invalid scan run ID."));
        }
        Ok(Self {
            host,
            run_id: run_id.to_owned(),
        })
    }

    /// `prepare()`: creates the SBOM directory, pulls the scanner images, and sets up the Grype
    /// database volume.
    pub fn prepare(&self) -> Vec<String> {
        let mut commands = vec![self.remove_container()];
        commands.extend(self.sbom_directory());
        for tool in ["syft", "grype", "regctl", "init"] {
            commands.push(step(
                &format!("pull-{tool}"),
                &format!(
                    "timeout -k 10 {TIMEOUT_PULL} docker pull {} >/dev/null 2>&1",
                    quote(&image(tool))
                ),
            ));
        }
        commands.push(self.database_volume());
        commands
    }

    /// `scannerImages()`: checks that all scanner images are on the server. `docker image
    /// inspect` fails when one of them is missing.
    pub fn scanner_images(&self) -> Vec<String> {
        let images = ["syft", "grype", "regctl", "init"]
            .map(|tool| quote(&image(tool)))
            .join(" ");
        vec![
            self.remove_container(),
            step(
                "scanner-images",
                &format!("docker image inspect {images} >/dev/null 2>&1"),
            ),
        ]
    }

    /// `dbUpdate()`.
    pub fn db_update(&self) -> Vec<String> {
        let env = [("GRYPE_DB_CACHE_DIR", "/db"), ("TMPDIR", "/db/tmp")];
        let mut commands = self.start(&["scratch"]);
        commands.extend(self.own_work_dir(false));
        commands.push(self.database_volume());
        commands.push(step(
            "db-update",
            &format!(
                "{} >/dev/null 2>&1",
                self.docker_run(
                    "grype",
                    TIMEOUT_DB_UPDATE,
                    true,
                    &env,
                    &[format!("type=volume,src={DB_VOLUME},dst=/db")],
                    &["db", "update"],
                )
            ),
        ));
        commands.extend(self.finish(&[]));
        commands
    }

    /// `dbStatus()`: prints `grype db status -o json` (database build date and schema) as payload.
    pub fn db_status(&self) -> Vec<String> {
        let run = self.docker_run(
            "grype",
            TIMEOUT_DB_STATUS,
            false,
            &grype_database_env(),
            &[database_mount()],
            &["db", "status", "-o", "json"],
        );
        self.capture("db-status", &run, None, None, false)
    }

    /// `pull($imageRef)`: pulls an image that is not on the server yet, with the registry login
    /// of the server user.
    pub fn pull(&self, image_ref: &str) -> Result<Vec<String>, InvalidArgument> {
        let reference = quote(&ImageRef::parse(image_ref)?.to_string());
        Ok(vec![
            self.remove_container(),
            step(
                "pull",
                &format!(
                    "{}timeout -k 10 {TIMEOUT_PULL} docker pull {reference} >/dev/null 2>&1",
                    server_docker_config_prefix(self.host.non_root)
                ),
            ),
        ])
    }

    /// `inspect($imageRef)`.
    pub fn inspect(&self, image_ref: &str) -> Result<Vec<String>, InvalidArgument> {
        let reference = quote(&ImageRef::parse(image_ref)?.to_string());
        let format = quote(
            "{{.Id}}|{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}|{{.Os}}/{{.Architecture}}|{{.Size}}",
        );
        Ok(vec![
            self.remove_container(),
            step(
                "inspect",
                &format!("docker image inspect --format '{{{{.Id}}}}' {reference} >/dev/null 2>&1"),
            ),
            format!("echo {PAYLOAD_SEPARATOR}"),
            format!("docker image inspect --format {format} {reference} 2>/dev/null || true"),
        ])
    }

    /// `diskFree()`.
    pub fn disk_free(&self) -> Vec<String> {
        let base = quote(BASE_DIR);
        vec![
            self.remove_container(),
            format!("mkdir -p -m 0755 {base}"),
            r#"df -P -B1 "$(docker info -f '{{.DockerRootDir}}')" | awk 'NR==2 {printf "%s ", $4}'"#
                .to_owned(),
            format!("df -P -B1 {base} | awk 'NR==2 {{print $4}}'"),
        ]
    }

    /// `diskUsage()`: prints the bytes that scanner data uses on the server:
    /// `coolify-scan-exit disk-db 0 {bytes}` for the Grype database volume and
    /// `coolify-scan-exit disk-sboms 0 {bytes}` for `/data/coolify/image-scans`. A missing volume
    /// or directory counts as 0 bytes.
    pub fn disk_usage(&self) -> Vec<String> {
        let mountpoint = format!(
            "\"$(docker volume inspect -f '{{{{.Mountpoint}}}}' {DB_VOLUME} 2>/dev/null)\""
        );
        vec![
            self.remove_container(),
            shell(&usage_marker("disk-db", &mountpoint)),
            shell(&usage_marker("disk-sboms", &quote(BASE_DIR))),
        ]
    }

    /// `sbomFromLocal($imageRef, $imageIdHex)`: saves a local image by its ID (not by the ref: a
    /// tag can move after inspect) and writes its SBOM to `sboms/{imageIdHex}.json`. The ref is
    /// only validated.
    pub fn sbom_from_local(
        &self,
        image_ref: &str,
        image_id_hex: &str,
    ) -> Result<Vec<String>, InvalidArgument> {
        ImageRef::parse(image_ref)?;
        if !IMAGE_HEX.is_match(image_id_hex) {
            return Err(invalid("Invalid image ID."));
        }
        let work = self.work_dir();
        let archive = format!("{work}/in/image.tar");
        let tmp = quote(&self.sbom_tmp_path(image_id_hex));
        let mut commands = vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(&work)),
        ];
        commands.extend(self.sbom_directory());
        commands.push(format!("rm -f {tmp} || true"));
        commands.push(format!(
            "mkdir -p -m 0700 {} {}",
            quote(&format!("{work}/in")),
            quote(&format!("{work}/scratch"))
        ));
        commands.push(step(
            "save",
            &format!(
                "timeout -k 10 {TIMEOUT_SAVE} docker save -o {} {} >/dev/null 2>&1",
                quote(&archive),
                quote(&format!("sha256:{image_id_hex}"))
            ),
        ));
        commands.extend(self.own_work_dir(false));
        commands.push(self.write_sbom(
            image_id_hex,
            &self.docker_run(
                "syft",
                self.host.syft_timeout(),
                false,
                &[],
                &[format!("type=bind,src={work}/in,dst=/in,readonly")],
                &[
                    "scan",
                    "docker-archive:/in/image.tar",
                    "-o",
                    "syft-json",
                    "-q",
                ],
            ),
        ));
        commands.extend(self.finish(&[format!("rm -f {tmp} || true")]));
        Ok(commands)
    }

    /// `hostPackagesSize()`: prints `coolify-scan-exit packages-size {code} {KiB}`: the size of
    /// the host package databases that [`sbom_from_host`](Self::sbom_from_host) copies (`du -k`:
    /// BusyBox has no `-b`). Missing paths are skipped; the code is 1 when a path that exists
    /// cannot be measured.
    pub fn host_packages_size(&self) -> Vec<String> {
        let paths = HOST_PACKAGE_PATHS.map(quote).join(" ");
        vec![shell(&format!(
            r#"total=0; failed=0; for p in {paths}; do if [ -e "$p" ]; then if out=$(du -skL "$p" 2>/dev/null); then total=$((total + ${{out%%[[:space:]]*}})); else failed=1; fi; fi; done; echo "{MARKER} packages-size $failed $total""#
        ))]
    }

    /// `sbomFromHost()`: writes the SBOM of the OS packages of the server to `sboms/host.json`.
    /// Only the package databases are copied (`cp -RL`, so the copy has no links), then Syft
    /// reads the copy without network with the OS package catalogers. Prints
    /// `coolify-scan-exit copy {code}` (1 when no package database was found, 2 when a copy
    /// failed, so a partial database never reads as a complete scan) and the `sbom` marker.
    pub fn sbom_from_host(&self) -> Vec<String> {
        let work = self.work_dir();
        let host = format!("{work}/in/host");
        let mut script = vec!["failed=0".to_owned()];
        for path in HOST_PACKAGE_PATHS {
            let source = quote(path);
            let target = quote(&format!("{host}{path}"));
            let skip_link = if path == "/var/lib/rpm" {
                format!(" && [ ! -L {source} ]")
            } else {
                String::new()
            };
            script.push(format!(
                "if [ -e {source} ]{skip_link}; then mkdir -p \"$(dirname {target})\" && cp -RL {source} {target} 2>/dev/null || failed=1; fi"
            ));
        }
        let found = HOST_PACKAGE_DATABASES
            .map(|path| quote(&format!("{host}{path}")))
            .join(" ");
        script.push(format!(
            "found=0; for p in {found}; do if [ -e \"$p\" ]; then found=1; fi; done; if [ $failed -eq 1 ]; then echo '{MARKER} copy 2'; elif [ $found -eq 1 ]; then echo '{MARKER} copy 0'; else echo '{MARKER} copy 1'; fi"
        ));
        let tmp = quote(&self.sbom_tmp_path(HOST_SBOM));
        let mut commands = vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(&work)),
        ];
        commands.extend(self.sbom_directory());
        commands.push(format!("rm -f {tmp} || true"));
        commands.push(format!(
            "mkdir -p -m 0700 {} {}",
            quote(&host),
            quote(&format!("{work}/scratch"))
        ));
        commands.push(shell(&script.join("; ")));
        commands.extend(self.own_work_dir(false));
        commands.push(self.write_sbom(
            HOST_SBOM,
            &self.docker_run(
                "syft",
                self.host.syft_timeout(),
                false,
                &[],
                &[format!("type=bind,src={work}/in,dst=/in,readonly")],
                &[
                    "scan",
                    "dir:/in/host",
                    "--override-default-catalogers",
                    HOST_CATALOGERS,
                    "-o",
                    "syft-json",
                    "-q",
                ],
            ),
        ));
        commands.extend(self.finish(&[format!("rm -f {tmp} || true")]));
        commands
    }

    /// `sbomFromRegistry()`: writes the SBOM of a registry image to `sboms/{outName}.json`
    /// (network on, no socket).
    pub fn sbom_from_registry(
        &self,
        image_ref: &str,
        platform: &str,
        out_name: &str,
        auth_config_b64: Option<&str>,
    ) -> Result<Vec<String>, InvalidArgument> {
        let parsed = ImageRef::parse(image_ref)?;
        let reference = parsed.to_string();
        let platform = valid_platform(platform)?;
        valid_sbom_name(out_name)?;
        let auth = auth_config(auth_config_b64, &parsed)?;
        let tmp = quote(&self.sbom_tmp_path(out_name));
        let registry = format!("registry:{reference}");
        let mut commands = vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(&self.work_dir())),
        ];
        commands.extend(self.sbom_directory());
        commands.push(format!("rm -f {tmp} || true"));
        commands.extend(self.work_dir_setup(auth.as_ref()));
        commands.push(self.write_sbom(
            out_name,
            &self.docker_run(
                "syft",
                self.host.syft_timeout(),
                true,
                &auth_env(auth.as_ref()),
                &self.auth_mounts(auth.as_ref()),
                &[
                    "scan",
                    "--platform",
                    platform,
                    registry.as_str(),
                    "-o",
                    "syft-json",
                    "-q",
                ],
            ),
        ));
        commands.extend(self.finish(&[format!("rm -f {tmp} || true")]));
        Ok(commands)
    }

    /// `grype($sbomName)`: matches a stored SBOM against the cached database; payload is the
    /// NDJSON of `grype.tmpl`.
    ///
    /// For the host SBOM the output keeps one line per vulnerability ID, package, package
    /// version, fix state, and fix versions, and drops the problems that the distribution will
    /// not fix, before the size is counted (see upstream's `capture()` for the reasons).
    pub fn grype(&self, sbom_name: &str) -> Result<Vec<String>, InvalidArgument> {
        valid_sbom_name(sbom_name)?;
        let run = self.docker_run(
            "grype",
            self.host.grype_timeout(),
            false,
            &grype_database_env(),
            &[
                database_mount(),
                self.sbom_mount(sbom_name),
                format!("type=bind,src={}/tpl,dst=/tpl,readonly", self.work_dir()),
            ],
            &[
                "sbom:/sbom/sbom.json",
                "-o",
                "template",
                "-t",
                "/tpl/grype.tmpl",
                "-q",
            ],
        );
        Ok(self.capture(
            "grype",
            &run,
            Some(("grype.tmpl", GRYPE_TEMPLATE)),
            None,
            sbom_name == HOST_SBOM,
        ))
    }

    /// `packages($sbomName)`: lists the packages of a stored SBOM; payload is the NDJSON of
    /// `syft.tmpl`.
    pub fn packages(&self, sbom_name: &str) -> Result<Vec<String>, InvalidArgument> {
        valid_sbom_name(sbom_name)?;
        let run = self.docker_run(
            "syft",
            TIMEOUT_PACKAGES,
            false,
            &[],
            &[
                self.sbom_mount(sbom_name),
                format!("type=bind,src={}/tpl,dst=/tpl,readonly", self.work_dir()),
            ],
            &[
                "convert",
                "/sbom/sbom.json",
                "-o",
                "template",
                "-t",
                "/tpl/syft.tmpl",
                "-q",
            ],
        );
        Ok(self.capture(
            "packages",
            &run,
            Some(("syft.tmpl", SYFT_TEMPLATE)),
            None,
            false,
        ))
    }

    /// `tags($repository, $authConfigB64)`: lists the tags of a repository, one per line.
    pub fn tags(
        &self,
        repository: &str,
        auth_config_b64: Option<&str>,
    ) -> Result<Vec<String>, InvalidArgument> {
        let parsed = ImageRef::parse(repository)?;
        let name = parsed.qualified_name();
        let auth = auth_config(auth_config_b64, &parsed)?;
        let run = self.docker_run(
            "regctl",
            TIMEOUT_REGISTRY,
            true,
            &auth_env(auth.as_ref()),
            &self.auth_mounts(auth.as_ref()),
            &["tag", "ls", name.as_str()],
        );
        Ok(self.capture("tags", &run, None, auth.as_ref(), false))
    }

    /// `digest($imageRef, $platform, $authConfigB64)`: prints the registry digest of an image
    /// (the index digest, or the platform manifest digest with a platform).
    pub fn digest(
        &self,
        image_ref: &str,
        platform: Option<&str>,
        auth_config_b64: Option<&str>,
    ) -> Result<Vec<String>, InvalidArgument> {
        let parsed = ImageRef::parse(image_ref)?;
        let reference = parsed.to_string();
        let mut args = vec!["image", "digest"];
        if let Some(platform) = platform {
            args.push("--platform");
            args.push(valid_platform(platform)?);
        }
        args.push(reference.as_str());
        let auth = auth_config(auth_config_b64, &parsed)?;
        let run = self.docker_run(
            "regctl",
            TIMEOUT_REGISTRY,
            true,
            &auth_env(auth.as_ref()),
            &self.auth_mounts(auth.as_ref()),
            &args,
        );
        Ok(self.capture("digest", &run, None, auth.as_ref(), false))
    }

    /// `cleanup()`: removes the container and the work directory of this run, and leftovers of
    /// runs that crashed over an hour ago.
    pub fn cleanup(&self) -> Vec<String> {
        vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(&self.work_dir())),
            format!(
                "find {} -maxdepth 1 -name 'tmp-*' -mmin +60 -exec rm -rf {{}} + 2>/dev/null || true",
                quote(BASE_DIR)
            ),
            format!(
                "find {} -maxdepth 1 -name '*.json.tmp-*' -mmin +60 -delete 2>/dev/null || true",
                quote(SBOM_DIR)
            ),
        ]
    }

    /// `deleteScannerData()`: removes all SBOMs, the Grype database volume, and the scanner
    /// images from the server.
    pub fn delete_scanner_data(&self) -> Vec<String> {
        let images = ["syft", "grype", "regctl"]
            .map(|tool| quote(&image(tool)))
            .join(" ");
        vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(BASE_DIR)),
            format!("docker volume rm -f {DB_VOLUME} >/dev/null 2>&1 || true"),
            format!("docker image rm {images} >/dev/null 2>&1 || true"),
        ]
    }

    /// `runningContainers()`: prints `coolify-scan-exit containers {code}`, then one JSON line
    /// per running container as payload: `{"id": image ID, "ref": image ref, "labels": {…}}`.
    pub fn running_containers(&self) -> Vec<String> {
        let format = quote(
            r#"{"id":{{json .Image}},"ref":{{json .Config.Image}},"labels":{{json .Config.Labels}}}"#,
        );
        vec![
            self.remove_container(),
            shell(&format!(
                r#"ids=$(docker ps -q --no-trunc 2>/dev/null); rc=$?; echo "{MARKER} containers $rc"; echo {PAYLOAD_SEPARATOR}; if [ -n "$ids" ]; then docker inspect --format {format} $ids 2>/dev/null | head -c {}; fi"#,
                OUTPUT_LIMIT + 1
            )),
        ]
    }

    /// `deleteSboms($imageIds)`: removes the SBOM files of the given image IDs (`sha256:{hex}`)
    /// and prints `coolify-scan-exit sbom-delete {code}`.
    pub fn delete_sboms(&self, image_ids: &[String]) -> Result<Vec<String>, InvalidArgument> {
        if image_ids.is_empty() {
            return Err(invalid("No SBOM to delete."));
        }
        let mut paths = Vec::with_capacity(image_ids.len());
        for image_id in image_ids {
            let Some(id) = IMAGE_ID.captures(image_id) else {
                return Err(invalid("Invalid image ID."));
            };
            paths.push(quote(&sbom_path(&id[1])));
        }
        Ok(vec![
            self.remove_container(),
            shell(&format!(
                "rm -f {}; echo \"{MARKER} sbom-delete $?\"",
                paths.join(" ")
            )),
        ])
    }

    fn container_name(&self) -> String {
        format!("coolify-scan-{}", self.run_id)
    }

    fn work_dir(&self) -> String {
        format!("{BASE_DIR}/tmp-{}", self.run_id)
    }

    /// Per run, so a replay or a parallel run never moves another run's partial file into place.
    fn sbom_tmp_path(&self, name: &str) -> String {
        format!("{}.tmp-{}", sbom_path(name), self.run_id)
    }

    fn remove_container(&self) -> String {
        format!(
            "docker rm -f {} >/dev/null 2>&1 || true",
            self.container_name()
        )
    }

    /// `oomCheck($step)`: shell script part that runs right after a `docker_run()` (no `--rm`):
    /// prints `coolify-scan-oom {step}` when Docker stopped the container at the memory limit,
    /// then removes the container. Keeps `$?` of the run in `rc`.
    fn oom_check(&self, step: &str) -> String {
        let name = self.container_name();
        format!(
            "if [ \"$(docker inspect -f '{{{{.State.OOMKilled}}}}' {name} 2>/dev/null)\" = true ]; then echo '{OOM_MARKER} {step}'; fi; docker rm -f {name} >/dev/null 2>&1"
        )
    }

    /// `dockerRun()`: a `timeout`-wrapped `docker run` with the hardening flags. Values are
    /// escaped; mounts use `--mount`, which fails on a missing source. Without `--rm`, so
    /// `oom_check()` can read why the container stopped; every list removes the container
    /// afterwards.
    fn docker_run(
        &self,
        tool: &str,
        timeout: i64,
        network: bool,
        env: &[(&str, &str)],
        mounts: &[String],
        args: &[&str],
    ) -> String {
        let memory = self.host.memory();
        // By default Syft runs NumCPU × 4 workers (`--cpus` does not lower NumCPU) and records
        // metadata of every file a package owns. Together they need more than the default 1 GB
        // limit for an app image with many files. Grype and the SBOM use only the packages.
        // Go collects garbage only when the heap doubles, so RSS can pass the container limit
        // while the live data still fits: a soft limit below the container limit makes Go
        // collect sooner instead of being killed.
        let go_mem_limit = format!("{}MiB", memory * 85 / 100);
        let mut merged: Vec<(String, String)> = [
            ("SYFT_CHECK_FOR_APP_UPDATE", "false"),
            ("SYFT_PARALLELISM", "1"),
            ("SYFT_FILE_METADATA_SELECTION", "none"),
            ("GRYPE_CHECK_FOR_APP_UPDATE", "false"),
            ("GOMEMLIMIT", go_mem_limit.as_str()),
            ("TMPDIR", "/scratch"),
            ("XDG_CACHE_HOME", "/scratch/cache"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        // `array_merge()`: a key that exists keeps its place and takes the new value.
        for (key, value) in env {
            match merged.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = (*value).to_owned(),
                None => merged.push(((*key).to_owned(), (*value).to_owned())),
            }
        }

        let mut parts = vec![
            format!("timeout -k 10 {timeout} docker run"),
            format!("--name {}", self.container_name()),
            "--cap-drop ALL --security-opt no-new-privileges --read-only --pids-limit 256"
                .to_owned(),
            format!(
                "--user {SCANNER_USER} --cpus {} --memory {memory}m --memory-swap {memory}m --log-driver none",
                self.host.cpus()
            ),
        ];
        if !network {
            parts.push("--network none".to_owned());
        }
        for (key, value) in &merged {
            parts.push(format!("-e {}", quote(&format!("{key}={value}"))));
        }
        parts.push(format!(
            "--mount {}",
            quote(&format!(
                "type=bind,src={}/scratch,dst=/scratch",
                self.work_dir()
            ))
        ));
        for mount in mounts {
            parts.push(format!("--mount {}", quote(mount)));
        }
        parts.push(quote(&image(tool)));
        for arg in args {
            parts.push(quote(arg));
        }
        parts.join(" ")
    }

    /// `capture()`: runs a tool whose stdout is the payload: stdout goes to a root-owned file in
    /// the work directory (not mounted into the container), then at most 10485761 bytes of it
    /// are printed.
    ///
    /// With `host_filter` the filter runs in the pipe, before the file limit, because the raw
    /// output of a host can be larger than the limit; the exit code of the run goes through
    /// `out.rc`, as `sh` has no `pipefail`. When the run succeeded, the exit code of `awk` is the
    /// exit code of the step, so a write failure (a full disk) fails the scan instead of storing
    /// a short report; a stop at the file limit still reads as cut output.
    fn capture(
        &self,
        step: &str,
        docker_run: &str,
        template: Option<(&str, &str)>,
        auth: Option<&AuthConfig>,
        host_filter: bool,
    ) -> Vec<String> {
        let work = self.work_dir();
        let out = quote(&format!("{work}/out"));
        let run = if host_filter {
            format!(
                "( {docker_run} 2>/dev/null; echo $? > {out}.rc ) | grep -v -F '\"fix\":\"wont-fix\"' | awk '{HOST_DEDUPE}' > {out}; frc=$?; rc=$(cat {out}.rc 2>/dev/null || echo 1); if [ \"$rc\" -eq 0 ]; then rc=$frc; fi"
            )
        } else {
            format!("{docker_run} > {out} 2>/dev/null; rc=$?")
        };
        let subdirectories: &[&str] = if template.is_some() {
            &["scratch", "tpl"]
        } else {
            &["scratch"]
        };
        let mut commands = self.start(subdirectories);
        if let Some((name, content)) = template {
            commands.push(write_file(content, &format!("{work}/tpl/{name}")));
        }
        if let Some(auth) = auth {
            commands.extend(self.auth_setup(auth));
        }
        commands.extend(self.own_work_dir(auth.is_some()));
        commands.push(shell(&format!(
            "ulimit -f {OUTPUT_FILE_LIMIT_BLOCKS}; {run}; {}; size=$(wc -c < {out} 2>/dev/null | tr -cd 0-9); size=${{size:-0}}; if [ \"$size\" -ge {} ]; then rc=0; fi; echo \"{MARKER} {step} $rc $size\"",
            self.oom_check(step),
            OUTPUT_FILE_LIMIT_BLOCKS * 512
        )));
        commands.extend(self.finish(&[
            format!("echo {PAYLOAD_SEPARATOR}"),
            format!("head -c {} {out} 2>/dev/null || true", OUTPUT_LIMIT + 1),
        ]));
        commands
    }

    /// `writeSbom()`: Syft writes the SBOM to stdout; only a complete run is moved into place.
    /// An SBOM over the file limit fails.
    fn write_sbom(&self, name: &str, docker_run: &str) -> String {
        let tmp = quote(&self.sbom_tmp_path(name));
        let destination = quote(&sbom_path(name));
        shell(&format!(
            "ulimit -f {SBOM_FILE_LIMIT_BLOCKS}; {docker_run} > {tmp} 2>/dev/null; rc=$?; {}; if [ $rc -eq 0 ]; then chown {SCANNER_USER} {tmp} && chmod 600 {tmp} && mv -f {tmp} {destination}; rc=$?; fi; echo \"{MARKER} sbom $rc\"",
            self.oom_check("sbom")
        ))
    }

    /// `start($subdirectories)`.
    fn start(&self, subdirectories: &[&str]) -> Vec<String> {
        let work = self.work_dir();
        vec![
            self.remove_container(),
            format!("rm -rf {} || true", quote(&work)),
            format!(
                "mkdir -p -m 0700 {}",
                subdirectories
                    .iter()
                    .map(|dir| quote(&format!("{work}/{dir}")))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ]
    }

    /// `finish($before)`.
    fn finish(&self, before: &[String]) -> Vec<String> {
        let mut commands = vec![self.remove_container()];
        commands.extend(before.iter().cloned());
        commands.push(format!("rm -rf {} || true", quote(&self.work_dir())));
        commands
    }

    /// `ownWorkDir($hasAuth)`: runs before any container used the directory, so nothing in it
    /// can be a planted link.
    fn own_work_dir(&self, has_auth: bool) -> Vec<String> {
        let work = self.work_dir();
        let mut commands = vec![
            format!("chown -R {SCANNER_USER} {}", quote(&work)),
            format!("chmod 700 {}", quote(&work)),
        ];
        if has_auth {
            commands.push(format!(
                "chmod 600 {}",
                quote(&format!("{work}/auth/config.json"))
            ));
        }
        commands
    }

    /// `workDirSetup($auth)`.
    fn work_dir_setup(&self, auth: Option<&AuthConfig>) -> Vec<String> {
        let mut commands = vec![format!(
            "mkdir -p -m 0700 {}",
            quote(&format!("{}/scratch", self.work_dir()))
        )];
        if let Some(auth) = auth {
            commands.extend(self.auth_setup(auth));
        }
        commands.extend(self.own_work_dir(auth.is_some()));
        commands
    }

    /// `sbomDirectory()`.
    fn sbom_directory(&self) -> Vec<String> {
        let dir = quote(SBOM_DIR);
        vec![
            format!("mkdir -p -m 0700 {dir}"),
            format!("chown {SCANNER_USER} {dir}"),
            format!("chmod 700 {dir}"),
        ]
    }

    /// `databaseVolume()`: creates the database volume and gives it to the scanner user from
    /// inside a container, so the shell never needs the host path of the volume (`chown -h` does
    /// not follow a link that a database update could have left). Prints
    /// `coolify-scan-exit db-volume {rc}`.
    fn database_volume(&self) -> String {
        let script = "mkdir -p /db/tmp && chown 65534:65534 /db && chown -h 65534:65534 /db/tmp && chmod 700 /db/tmp";
        let run = [
            format!("timeout -k 10 {TIMEOUT_VOLUME_SETUP} docker run"),
            format!("--rm --name {}", self.container_name()),
            "--network none --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER"
                .to_owned(),
            "--security-opt no-new-privileges --read-only --pids-limit 64".to_owned(),
            "--user 0:0 --cpus 0.5 --memory 64m --memory-swap 64m --log-driver none".to_owned(),
            format!("--mount type=volume,src={DB_VOLUME},dst=/db"),
            quote(&image("init")),
            format!("sh -c {}", quote(script)),
        ]
        .join(" ");
        shell(&step(
            "db-volume",
            &format!("docker volume create {DB_VOLUME} >/dev/null && {run} >/dev/null 2>&1"),
        ))
    }

    fn sbom_mount(&self, name: &str) -> String {
        format!(
            "type=bind,src={},dst=/sbom/sbom.json,readonly",
            sbom_path(name)
        )
    }

    /// `authSetup($auth)`.
    fn auth_setup(&self, auth: &AuthConfig) -> Vec<String> {
        let work = self.work_dir();
        // `json_encode($auth, JSON_UNESCAPED_SLASHES)`.
        let json = php_json::encode(&auth.0).replace("\\/", "/");
        vec![
            format!("mkdir -p -m 0700 {}", quote(&format!("{work}/auth"))),
            write_file(&json, &format!("{work}/auth/config.json")),
        ]
    }

    /// `authMounts($auth)`.
    fn auth_mounts(&self, auth: Option<&AuthConfig>) -> Vec<String> {
        match auth {
            Some(_) => vec![format!(
                "type=bind,src={}/auth,dst=/auth,readonly",
                self.work_dir()
            )],
            None => Vec::new(),
        }
    }
}

fn sbom_path(name: &str) -> String {
    format!("{SBOM_DIR}/{name}.json")
}

/// `image($tool)`: the scanner image, pinned to a sha256 digest.
fn image(tool: &str) -> String {
    SCANNER_IMAGES
        .iter()
        .find(|i| i.tool == tool)
        .map(|i| format!("{}@{}", i.repo, i.digest))
        .unwrap_or_default()
}

/// `step($step, $command)`: `if` keeps a failed run from stopping the batch; `$?` in `else` is
/// the exit code of the run.
fn step(step: &str, command: &str) -> String {
    format!("if {command}; then echo '{MARKER} {step} 0'; else echo \"{MARKER} {step} $?\"; fi")
}

fn usage_marker(step: &str, path: &str) -> String {
    format!(
        "echo \"{MARKER} {step} 0 $(du -sb {path} 2>/dev/null | cut -f1 | grep -E '^[0-9]+$' || echo 0)\""
    )
}

/// `shell($script)`: one `sh -c` line: the sudo parser prefixes it with `sudo` and leaves the
/// script as it is.
fn shell(script: &str) -> String {
    format!("sh -c {}", quote(script))
}

fn grype_database_env() -> [(&'static str, &'static str); 2] {
    [
        ("GRYPE_DB_CACHE_DIR", "/db"),
        ("GRYPE_DB_AUTO_UPDATE", "false"),
    ]
}

fn database_mount() -> String {
    format!("type=volume,src={DB_VOLUME},dst=/db,readonly")
}

/// `writeFile($content, $path)`: `echo` is a shell builtin and a sudo parser keyword: the content
/// never shows up in a process argument list or in the sudo log; only `base64 -d` and `tee` run
/// with sudo.
fn write_file(content: &str, path: &str) -> String {
    format!(
        "echo {} | base64 -d | tee {} >/dev/null",
        quote(&base64::engine::general_purpose::STANDARD.encode(content)),
        quote(path)
    )
}

/// `authEnv($auth)`.
fn auth_env(auth: Option<&AuthConfig>) -> Vec<(&'static str, &'static str)> {
    match auth {
        Some(_) => vec![("DOCKER_CONFIG", "/auth")],
        None => Vec::new(),
    }
}

fn valid_platform(platform: &str) -> Result<&str, InvalidArgument> {
    if PLATFORM.is_match(platform) {
        Ok(platform)
    } else {
        Err(invalid("Invalid platform."))
    }
}

fn valid_sbom_name(name: &str) -> Result<(), InvalidArgument> {
    if SBOM_NAME.is_match(name) {
        Ok(())
    } else {
        Err(invalid("Invalid SBOM name."))
    }
}

#[cfg(test)]
mod tests {
    use coolify_remote::sudo::parse_commands_by_line_for_sudo;
    use serde_json::json;

    use super::*;

    const RUN: &str = "run1234abcd";
    const INIT_IMAGE: &str =
        "alpine@sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f";
    const HEX: &str = "aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999";

    fn non_root() -> ScannerCommands {
        ScannerCommands::new(
            ScannerHost {
                non_root: true,
                ..ScannerHost::default()
            },
            RUN,
        )
        .unwrap()
    }

    fn with(host: ScannerHost) -> ScannerCommands {
        ScannerCommands::new(host, RUN).unwrap()
    }

    fn b64(v: &Value) -> String {
        base64::engine::general_purpose::STANDARD.encode(php_json::encode(v))
    }

    fn ghcr_auth() -> String {
        b64(
            &json!({"auths": {"ghcr.io": {"auth": base64::engine::general_purpose::STANDARD.encode("user:token")}}}),
        )
    }

    fn lists(c: &ScannerCommands) -> Vec<(&'static str, Vec<String>)> {
        let auth = ghcr_auth();
        let digest_ref = format!("ghcr.io/a/b@sha256:{}", "ab".repeat(32));
        vec![
            ("prepare", c.prepare()),
            ("dbUpdate", c.db_update()),
            ("dbStatus", c.db_status()),
            ("scannerImages", c.scanner_images()),
            ("inspect", c.inspect("localhost:5000/team/app:1.2").unwrap()),
            ("pull", c.pull("localhost:5000/team/app:1.2").unwrap()),
            ("diskFree", c.disk_free()),
            ("diskUsage", c.disk_usage()),
            ("sbomFromLocal", c.sbom_from_local("nginx", HEX).unwrap()),
            ("sbomFromHost", c.sbom_from_host()),
            (
                "sbomFromRegistry",
                c.sbom_from_registry(&digest_ref, "linux/amd64", &format!("cand-{HEX}"), None)
                    .unwrap(),
            ),
            (
                "sbomFromRegistry with auth",
                c.sbom_from_registry("ghcr.io/a/b:1", "linux/arm64/v8", "cand-1", Some(&auth))
                    .unwrap(),
            ),
            ("grype", c.grype(HEX).unwrap()),
            ("packages", c.packages(HEX).unwrap()),
            ("tags", c.tags("nginx", None).unwrap()),
            (
                "tags with auth",
                c.tags("ghcr.io/a/b", Some(&auth)).unwrap(),
            ),
            ("digest", c.digest("nginx:1.27", None, None).unwrap()),
            (
                "digest with platform and auth",
                c.digest("ghcr.io/a/b:1", Some("linux/amd64"), Some(&auth))
                    .unwrap(),
            ),
            ("cleanup", c.cleanup()),
            ("deleteScannerData", c.delete_scanner_data()),
            ("runningContainers", c.running_containers()),
            (
                "deleteSboms",
                c.delete_sboms(&[format!("sha256:{HEX}")]).unwrap(),
            ),
        ]
    }

    /// Unwraps `sh -c '<script>'` lines so assertions read the script as the shell runs it.
    fn unwrapped(commands: &[String]) -> Vec<String> {
        commands
            .iter()
            .map(|line| match line.strip_prefix("sh -c '") {
                Some(rest) => rest[..rest.len() - 1].replace("'\\''", "'"),
                None => line.clone(),
            })
            .collect()
    }

    fn docker_runs(commands: &[String]) -> Vec<String> {
        unwrapped(commands)
            .into_iter()
            .filter(|line| line.contains("docker run") && !line.contains(INIT_IMAGE))
            .collect()
    }

    #[test]
    fn scanner_images_are_pinned_by_digest() {
        for i in SCANNER_IMAGES {
            assert!(ImageRef::is_valid_digest(i.digest), "{}", i.tool);
            assert!(super::super::image_ref::is_valid_docker_image_name(i.repo));
        }
        let all: String = lists(&non_root())
            .into_iter()
            .flat_map(|(_, l)| l)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains(
            "'anchore/syft@sha256:4cc92b64ca74ffca4c94d4c8fcc01f217731375300be14d0b6eeb2e82eaa110a'"
        ));
        assert!(all.contains("'anchore/grype@sha256:621d56d1d68106b787022c81546f7e0a36202a25d1239f0c20195fab8c16f7ea'"));
        assert!(all.contains("'regclient/regctl@sha256:45b57933c2b67bb1e280b4baaa220ef1ab212c81268ab19589fa1e83719c7c82'"));
        assert!(!all.contains(":latest"));
        assert!(!all.contains("docker.sock"));
    }

    #[test]
    fn refuses_a_run_id_that_is_not_lowercase_letters_and_digits() {
        for bad in [
            "short",
            "UPPERCASE123",
            "run-1234567",
            "a;rm -rf /",
            &"a".repeat(33),
        ] {
            assert!(
                ScannerCommands::new(ScannerHost::default(), bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn every_list_starts_by_removing_a_leftover_container() {
        for (name, commands) in lists(&non_root()) {
            assert_eq!(
                commands[0],
                format!("docker rm -f coolify-scan-{RUN} >/dev/null 2>&1 || true"),
                "{name}"
            );
        }
    }

    #[test]
    fn every_scanner_container_has_each_hardening_flag_once() {
        let flags = [
            format!("--name coolify-scan-{RUN} "),
            "--cap-drop ALL ".into(),
            "--security-opt no-new-privileges ".into(),
            "--read-only ".into(),
            "--pids-limit 256 ".into(),
            "--user 65534:65534 ".into(),
            "--cpus 1 ".into(),
            "--memory 1024m ".into(),
            "--memory-swap 1024m ".into(),
            "-e 'SYFT_CHECK_FOR_APP_UPDATE=false' ".into(),
            "-e 'GRYPE_CHECK_FOR_APP_UPDATE=false' ".into(),
            "-e 'XDG_CACHE_HOME=/scratch/cache' ".into(),
        ];
        let timeout = Regex::new(r"timeout -k 10 \d+ docker run ").unwrap();
        let mut runs = 0;
        for (name, commands) in lists(&non_root()) {
            for line in docker_runs(&commands) {
                runs += 1;
                assert!(timeout.is_match(&line), "{name}");
                assert!(!line.contains("--rm "), "{name}");
                for flag in &flags {
                    assert_eq!(line.matches(flag.as_str()).count(), 1, "{name}: {flag}");
                }
            }
        }
        // dbUpdate, dbStatus, three SBOM sources (the registry one twice), grype, packages, and
        // tags and digest twice each.
        assert_eq!(runs, 12);
    }

    #[test]
    fn the_exact_grype_run() {
        let commands = with(ScannerHost::default()).grype(HEX).unwrap();
        let work = format!("/data/coolify/image-scans/tmp-{RUN}");
        let run = format!(
            "timeout -k 10 600 docker run --name coolify-scan-{RUN} --cap-drop ALL --security-opt no-new-privileges --read-only --pids-limit 256 \
             --user 65534:65534 --cpus 1 --memory 1024m --memory-swap 1024m --log-driver none --network none \
             -e 'SYFT_CHECK_FOR_APP_UPDATE=false' -e 'SYFT_PARALLELISM=1' -e 'SYFT_FILE_METADATA_SELECTION=none' -e 'GRYPE_CHECK_FOR_APP_UPDATE=false' \
             -e 'GOMEMLIMIT=870MiB' -e 'TMPDIR=/scratch' -e 'XDG_CACHE_HOME=/scratch/cache' -e 'GRYPE_DB_CACHE_DIR=/db' -e 'GRYPE_DB_AUTO_UPDATE=false' \
             --mount 'type=bind,src={work}/scratch,dst=/scratch' --mount 'type=volume,src=coolify-grype-db,dst=/db,readonly' \
             --mount 'type=bind,src=/data/coolify/image-scans/sboms/{HEX}.json,dst=/sbom/sbom.json,readonly' --mount 'type=bind,src={work}/tpl,dst=/tpl,readonly' \
             'anchore/grype@sha256:621d56d1d68106b787022c81546f7e0a36202a25d1239f0c20195fab8c16f7ea' 'sbom:/sbom/sbom.json' '-o' 'template' '-t' '/tpl/grype.tmpl' '-q'"
        );
        let script = format!(
            "ulimit -f 40960; {run} > '{work}/out' 2>/dev/null; rc=$?; if [ \"$(docker inspect -f '{{{{.State.OOMKilled}}}}' coolify-scan-{RUN} 2>/dev/null)\" = true ]; then echo 'coolify-scan-oom grype'; fi; docker rm -f coolify-scan-{RUN} >/dev/null 2>&1; size=$(wc -c < '{work}/out' 2>/dev/null | tr -cd 0-9); size=${{size:-0}}; if [ \"$size\" -ge 20971520 ]; then rc=0; fi; echo \"coolify-scan-exit grype $rc $size\""
        );
        let template = base64::engine::general_purpose::STANDARD.encode(GRYPE_TEMPLATE);
        assert_eq!(
            unwrapped(&commands),
            vec![
                format!("docker rm -f coolify-scan-{RUN} >/dev/null 2>&1 || true"),
                format!("rm -rf '{work}' || true"),
                format!("mkdir -p -m 0700 '{work}/scratch' '{work}/tpl'"),
                format!("echo '{template}' | base64 -d | tee '{work}/tpl/grype.tmpl' >/dev/null"),
                format!("chown -R 65534:65534 '{work}'"),
                format!("chmod 700 '{work}'"),
                script,
                format!("docker rm -f coolify-scan-{RUN} >/dev/null 2>&1 || true"),
                "echo coolify-scan-payload".to_owned(),
                format!("head -c 10485761 '{work}/out' 2>/dev/null || true"),
                format!("rm -rf '{work}' || true"),
            ]
        );
    }

    #[test]
    fn network_is_cut_only_for_runs_that_need_no_registry() {
        let all = lists(&non_root());
        let of = |name: &str| {
            all.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| docker_runs(l))
                .unwrap()
        };
        for name in [
            "dbStatus",
            "sbomFromLocal",
            "sbomFromHost",
            "grype",
            "packages",
        ] {
            assert!(
                of(name).iter().all(|l| l.contains("--network none ")),
                "{name}"
            );
        }
        for name in ["dbUpdate", "sbomFromRegistry", "tags", "digest"] {
            assert!(of(name).iter().all(|l| !l.contains("--network")), "{name}");
        }
    }

    #[test]
    fn the_work_directory_goes_to_the_scanner_user_before_a_container_uses_it() {
        let chown = format!("chown -R 65534:65534 '/data/coolify/image-scans/tmp-{RUN}'");
        for (name, commands) in lists(&non_root()) {
            let Some(run) = commands
                .iter()
                .position(|l| l.contains("docker run") && !l.contains(INIT_IMAGE))
            else {
                continue;
            };
            let at = commands.iter().position(|l| *l == chown);
            assert!(at.is_some_and(|at| at < run), "{name}");
        }
    }

    #[test]
    fn limits_come_from_the_server_settings() {
        let c = with(ScannerHost {
            memory_mb: Some(4096),
            syft_timeout: Some(1800),
            grype_timeout: Some(1200),
            cpus: Some(2.0),
            ..ScannerHost::default()
        });
        for (_, commands) in lists(&c) {
            for line in docker_runs(&commands) {
                assert!(line.contains("--memory 4096m --memory-swap 4096m "));
                assert!(line.contains("-e 'GOMEMLIMIT=3481MiB' "));
            }
        }
        let local = unwrapped(&c.sbom_from_local("nginx", HEX).unwrap()).join("\n");
        assert!(local.contains("timeout -k 10 600 docker save "));
        assert!(
            docker_runs(&c.sbom_from_local("nginx", HEX).unwrap())[0]
                .contains("; timeout -k 10 1800 docker run ")
        );
        assert!(docker_runs(&c.sbom_from_host())[0].contains("; timeout -k 10 1800 docker run "));
        assert!(
            docker_runs(
                &c.sbom_from_registry("nginx:1", "linux/amd64", "cand-1", None)
                    .unwrap()
            )[0]
            .contains("; timeout -k 10 1800 docker run ")
        );
        let grype = &docker_runs(&c.grype(HEX).unwrap())[0];
        assert!(grype.contains("; timeout -k 10 1200 docker run "));
        assert!(grype.contains("--cpus 2 --memory 4096m "));
        assert!(
            docker_runs(&c.packages(HEX).unwrap())[0].contains("; timeout -k 10 300 docker run ")
        );
    }

    #[test]
    fn stored_limits_stay_inside_the_options() {
        let host = ScannerHost {
            syft_timeout: Some(99999),
            grype_timeout: Some(5),
            cpus: Some(0.1),
            ..ScannerHost::default()
        };
        assert_eq!(host.syft_timeout(), 3600);
        assert_eq!(host.grype_timeout(), 300);
        assert_eq!(host.cpus(), "0.5");
        assert_eq!(ScannerHost::default().cpus(), "1");
        // Docker refuses `--cpus` above the CPU count.
        let small = ScannerHost {
            cpus: Some(4.0),
            cpu_count: 2,
            total_memory_bytes: 3 * 1024 * 1024 * 1024,
            ..ScannerHost::default()
        };
        assert_eq!(small.cpus(), "2");
        assert_eq!(small.cpu_options(), vec![0.5, 1.0, 2.0]);
        assert_eq!(small.memory_options_mb(), vec![1024, 2048]);
        let tiny = ScannerHost {
            total_memory_bytes: 512 * 1024 * 1024,
            ..ScannerHost::default()
        };
        assert_eq!(tiny.memory_options_mb(), vec![1024]);
        assert_eq!(
            ScannerHost::default().memory_options_mb(),
            vec![1024, 2048, 4096, 8192]
        );
    }

    #[test]
    fn image_refs_are_escaped_shell_arguments() {
        let c = non_root();
        let digest_ref = format!("ghcr.io/a/b@sha256:{}", "ab".repeat(32));
        let script = |commands: Vec<String>| unwrapped(&commands).join("\n");
        assert!(
            script(c.inspect("localhost:5000/team/app:1.2").unwrap())
                .contains("'localhost:5000/team/app:1.2'")
        );
        assert!(
            script(
                c.sbom_from_registry(&digest_ref, "linux/amd64", "c1", None)
                    .unwrap()
            )
            .contains(&format!(
                "'--platform' 'linux/amd64' 'registry:{digest_ref}'"
            ))
        );
        assert!(
            script(c.digest(&digest_ref, None, None).unwrap())
                .contains(&format!("'image' 'digest' '{digest_ref}'"))
        );
        assert!(
            script(c.tags("nginx:1", None).unwrap())
                .contains("'tag' 'ls' 'docker.io/library/nginx'")
        );
    }

    #[test]
    fn refuses_values_that_could_break_out_of_the_command() {
        let c = non_root();
        assert!(c.inspect("nginx;rm -rf /").is_err());
        assert!(c.sbom_from_local("nginx$(id)", HEX).is_err());
        assert!(
            c.sbom_from_local("nginx", &format!("sha256:{HEX}"))
                .is_err()
        );
        assert!(c.grype("../etc/passwd").is_err());
        assert!(c.packages("a b").is_err());
        assert!(
            c.sbom_from_registry("nginx", "linux/amd64", "x;id", None)
                .is_err()
        );
        assert!(
            c.sbom_from_registry("nginx", "linux/amd64;id", "c1", None)
                .is_err()
        );
        assert!(c.digest("nginx", Some("linux"), None).is_err());
        assert!(c.tags("nginx|id", None).is_err());
        assert!(c.tags("nginx", Some("abc'; id; '")).is_err());
        let two = b64(&json!({"auths": {"a.io": {"auth": "x"}, "b.io": {"auth": "y"}}}));
        assert!(c.tags("nginx", Some(&two)).is_err());
        let helper = b64(&json!({"auths": {"a.io": {"auth": "x"}}, "credsStore": "desktop"}));
        assert!(c.tags("nginx", Some(&helper)).is_err());
        for bad in ["sha256:../../etc/passwd", "sha256:abc", HEX] {
            assert!(c.delete_sboms(&[bad.to_owned()]).is_err(), "{bad}");
        }
        assert!(c.delete_sboms(&[]).is_err());
    }

    #[test]
    fn the_sbom_is_written_to_a_temp_file_and_moved_after_syft_succeeds() {
        let commands = non_root().sbom_from_local("nginx", HEX).unwrap();
        let index = commands
            .iter()
            .position(|l| l.contains("docker-archive:"))
            .unwrap();
        let syft = &unwrapped(&commands)[index];
        let tmp = format!("/data/coolify/image-scans/sboms/{HEX}.json.tmp-{RUN}");
        let dest = format!("/data/coolify/image-scans/sboms/{HEX}.json");
        assert!(commands[index].starts_with("sh -c 'ulimit -f 2097152; "));
        assert!(syft.contains(&format!("> '{tmp}' 2>/dev/null; rc=$?; ")));
        assert!(syft.contains(&format!(
            "if [ $rc -eq 0 ]; then chown 65534:65534 '{tmp}' && chmod 600 '{tmp}' && mv -f '{tmp}' '{dest}'; rc=$?; fi; echo \"coolify-scan-exit sbom $rc\""
        )));
        assert!(commands.contains(&format!("rm -f '{tmp}' || true")));
        assert!(syft.contains("-e 'SYFT_PARALLELISM=1' "));
        assert!(syft.contains("-e 'SYFT_FILE_METADATA_SELECTION=none' "));

        // The local image is saved by its ID so a moved tag cannot change the cached SBOM.
        let save = non_root()
            .sbom_from_local("nginx:1", HEX)
            .unwrap()
            .into_iter()
            .find(|l| l.contains("docker save"))
            .unwrap();
        assert!(save.contains(&format!(
            "docker save -o '/data/coolify/image-scans/tmp-{RUN}/in/image.tar' 'sha256:{HEX}' >/dev/null 2>&1"
        )));
        assert!(!save.contains("nginx"));
        assert!(!save.contains("OOMKilled"));
    }

    #[test]
    fn output_limits_match() {
        assert_eq!(OUTPUT_FILE_LIMIT_BLOCKS * 512, 2 * OUTPUT_LIMIT);
        assert_eq!(SBOM_FILE_LIMIT_BLOCKS * 512, 1024 * 1024 * 1024);
        for (name, commands) in lists(&non_root()) {
            for line in commands {
                if line.contains("docker run") && line.contains("> ") {
                    assert!(line.starts_with("sh -c 'ulimit -f "), "{name}");
                }
            }
        }
    }

    #[test]
    fn registry_credentials_go_into_a_private_file_mounted_read_only() {
        let c = non_root();
        let auth = ghcr_auth();
        let commands = c.tags("ghcr.io/a/b", Some(&auth)).unwrap();
        let config = format!("/data/coolify/image-scans/tmp-{RUN}/auth/config.json");
        assert!(commands.contains(&format!("chmod 600 '{config}'")));
        assert!(
            commands
                .join("\n")
                .contains(&format!("| base64 -d | tee '{config}' >/dev/null"))
        );
        let run = &docker_runs(&commands)[0];
        assert!(run.contains("-e 'DOCKER_CONFIG=/auth'"));
        assert!(run.contains("dst=/auth,readonly"));
        let plain = c.tags("ghcr.io/a/b", None).unwrap().join("\n");
        assert!(!plain.contains("DOCKER_CONFIG"));
        assert!(!plain.contains("/auth"));

        // The file holds the config with unescaped slashes.
        let secret = base64::engine::general_purpose::STANDARD.encode("user:secret/token");
        let auth = b64(&json!({"auths": {"https://ghcr.io/": {"auth": secret}}}));
        let written = base64::engine::general_purpose::STANDARD.encode(format!(
            "{{\"auths\":{{\"https://ghcr.io/\":{{\"auth\":\"{secret}\"}}}}}}"
        ));
        let commands = c.digest("ghcr.io/a/b:1", None, Some(&auth)).unwrap();
        assert!(
            commands
                .join("\n")
                .contains(&format!("echo '{written}' | base64 -d"))
        );
        let sudo = parse_commands_by_line_for_sudo(&commands, "ubuntu").join("\n");
        assert!(sudo.contains(&format!("echo '{written}' | sudo base64 -d")));
    }

    #[test]
    fn credentials_must_be_for_the_registry_of_the_image() {
        let c = non_root();
        let auth = |key: &str| b64(&json!({"auths": {key: {"auth": "eA=="}}}));
        for (key, reference) in [
            ("ghcr.io", "ghcr.io/a/b:1"),
            ("https://ghcr.io", "ghcr.io/a/b:1"),
            ("localhost:5000", "localhost:5000/team/app:1.2"),
            ("https://index.docker.io/v1/", "nginx"),
            ("index.docker.io", "grafana/grafana"),
            ("docker.io", "nginx"),
            ("registry-1.docker.io", "nginx"),
        ] {
            assert!(c.digest(reference, None, Some(&auth(key))).is_ok(), "{key}");
        }
        for (key, reference) in [
            ("ghcr.io", "nginx"),
            ("https://index.docker.io/v1/", "ghcr.io/a/b"),
            ("ghcr.io.evil.com", "ghcr.io/a/b"),
            ("localhost:5001", "localhost:5000/app"),
        ] {
            assert_eq!(
                c.tags(reference, Some(&auth(key))),
                Err(invalid(
                    "The registry credentials are not for the registry of the image."
                )),
                "{key}"
            );
        }
    }

    #[test]
    fn cleanup_and_delete() {
        let c = non_root();
        assert_eq!(
            c.cleanup(),
            vec![
                format!("docker rm -f coolify-scan-{RUN} >/dev/null 2>&1 || true"),
                format!("rm -rf '/data/coolify/image-scans/tmp-{RUN}' || true"),
                "find '/data/coolify/image-scans' -maxdepth 1 -name 'tmp-*' -mmin +60 -exec rm -rf {} + 2>/dev/null || true".to_owned(),
                "find '/data/coolify/image-scans/sboms' -maxdepth 1 -name '*.json.tmp-*' -mmin +60 -delete 2>/dev/null || true".to_owned(),
            ]
        );
        let delete = c.delete_scanner_data();
        assert!(delete.contains(&"rm -rf '/data/coolify/image-scans' || true".to_owned()));
        assert!(
            delete.contains(
                &"docker volume rm -f coolify-grype-db >/dev/null 2>&1 || true".to_owned()
            )
        );
        let other = "1".repeat(64);
        let commands = c
            .delete_sboms(&[format!("sha256:{HEX}"), format!("sha256:{other}")])
            .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            unwrapped(&commands)[1],
            format!(
                "rm -f '/data/coolify/image-scans/sboms/{HEX}.json' '/data/coolify/image-scans/sboms/{other}.json'; echo \"coolify-scan-exit sbom-delete $?\""
            )
        );
    }

    #[test]
    fn inspect_pull_and_disk_lines() {
        let c = non_root();
        let inspect = c.inspect("nginx:1").unwrap();
        assert_eq!(
            inspect[1],
            "if docker image inspect --format '{{.Id}}' 'nginx:1' >/dev/null 2>&1; then echo 'coolify-scan-exit inspect 0'; else echo \"coolify-scan-exit inspect $?\"; fi"
        );
        assert_eq!(inspect[2], "echo coolify-scan-payload");
        assert_eq!(
            inspect[3],
            "docker image inspect --format '{{.Id}}|{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}|{{.Os}}/{{.Architecture}}|{{.Size}}' 'nginx:1' 2>/dev/null || true"
        );

        let pull = parse_commands_by_line_for_sudo(&c.pull("ghcr.io/a/b:1").unwrap(), "ubuntu");
        assert_eq!(
            pull[1],
            "if sudo DOCKER_CONTEXT=default DOCKER_CONFIG=\"$(sudo find \"$HOME/.docker/config.json\" -maxdepth 0 -exec dirname {} \\; 2>/dev/null)\" timeout -k 10 600 docker pull 'ghcr.io/a/b:1' >/dev/null 2>&1; then echo 'coolify-scan-exit pull 0'; else echo \"coolify-scan-exit pull $?\"; fi"
        );
        assert_eq!(
            with(ScannerHost::default()).pull("ghcr.io/a/b:1").unwrap()[1],
            "if timeout -k 10 600 docker pull 'ghcr.io/a/b:1' >/dev/null 2>&1; then echo 'coolify-scan-exit pull 0'; else echo \"coolify-scan-exit pull $?\"; fi"
        );

        let free = parse_commands_by_line_for_sudo(&c.disk_free(), "ubuntu");
        assert_eq!(
            free[2],
            "sudo df -P -B1 \"$(sudo docker info -f '{{.DockerRootDir}}')\" | sudo awk 'NR==2 {printf \"%s \", $4}'"
        );
        assert_eq!(
            free[3],
            "sudo df -P -B1 '/data/coolify/image-scans' | sudo awk 'NR==2 {print $4}'"
        );

        let usage = c.disk_usage();
        assert_eq!(usage.len(), 3);
        assert_eq!(
            unwrapped(&usage)[1],
            "echo \"coolify-scan-exit disk-db 0 $(du -sb \"$(docker volume inspect -f '{{.Mountpoint}}' coolify-grype-db 2>/dev/null)\" 2>/dev/null | cut -f1 | grep -E '^[0-9]+$' || echo 0)\""
        );
        assert_eq!(
            unwrapped(&usage)[2],
            "echo \"coolify-scan-exit disk-sboms 0 $(du -sb '/data/coolify/image-scans' 2>/dev/null | cut -f1 | grep -E '^[0-9]+$' || echo 0)\""
        );
    }

    #[test]
    fn sudo_keeps_every_shell_script_as_one_line() {
        for (name, commands) in lists(&non_root()) {
            let parsed = parse_commands_by_line_for_sudo(&commands, "ubuntu");
            for (index, line) in parsed.iter().enumerate() {
                let original = &commands[index];
                assert!(!line.contains("sudo sudo"), "{name}");
                if original.starts_with("sh -c ") {
                    assert_eq!(*line, format!("sudo {original}"), "{name}");
                    assert_eq!(line.matches("sudo").count(), 1, "{name}");
                }
            }
        }
    }

    #[test]
    fn the_database_volume_is_set_up_inside_a_container() {
        let c = non_root();
        for (name, commands) in [("prepare", c.prepare()), ("dbUpdate", c.db_update())] {
            assert!(!commands.join("\n").contains("Mountpoint"), "{name}");
            let setup = unwrapped(&commands)
                .into_iter()
                .find(|l| l.contains("coolify-scan-exit db-volume"))
                .unwrap();
            assert_eq!(
                setup,
                format!(
                    "if docker volume create coolify-grype-db >/dev/null && timeout -k 10 120 docker run --rm --name coolify-scan-{RUN} \
                     --network none --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER \
                     --security-opt no-new-privileges --read-only --pids-limit 64 --user 0:0 --cpus 0.5 --memory 64m --memory-swap 64m --log-driver none \
                     --mount type=volume,src=coolify-grype-db,dst=/db '{INIT_IMAGE}' \
                     sh -c 'mkdir -p /db/tmp && chown 65534:65534 /db && chown -h 65534:65534 /db/tmp && chmod 700 /db/tmp' >/dev/null 2>&1; \
                     then echo 'coolify-scan-exit db-volume 0'; else echo \"coolify-scan-exit db-volume $?\"; fi"
                ),
                "{name}"
            );
        }
        let prepare = c.prepare();
        let pull = prepare
            .iter()
            .position(|l| l.contains(&format!("docker pull '{INIT_IMAGE}'")))
            .unwrap();
        let setup = prepare
            .iter()
            .position(|l| l.contains("coolify-scan-exit db-volume"))
            .unwrap();
        assert!(setup > pull);
        assert_eq!(
            prepare[4],
            "if timeout -k 10 600 docker pull 'anchore/syft@sha256:4cc92b64ca74ffca4c94d4c8fcc01f217731375300be14d0b6eeb2e82eaa110a' >/dev/null 2>&1; then echo 'coolify-scan-exit pull-syft 0'; else echo \"coolify-scan-exit pull-syft $?\"; fi"
        );
    }

    #[test]
    fn db_update_overrides_the_temp_directory() {
        let run = &docker_runs(&non_root().db_update())[0];
        assert!(run.contains(
            "-e 'TMPDIR=/db/tmp' -e 'XDG_CACHE_HOME=/scratch/cache' -e 'GRYPE_DB_CACHE_DIR=/db' "
        ));
        assert!(run.contains("--mount 'type=volume,src=coolify-grype-db,dst=/db' "));
        assert!(run.ends_with("'db' 'update' >/dev/null 2>&1; then echo 'coolify-scan-exit db-update 0'; else echo \"coolify-scan-exit db-update $?\"; fi"));
    }

    #[test]
    fn host_commands() {
        let c = non_root();
        assert_eq!(
            unwrapped(&c.host_packages_size()),
            vec![
                "total=0; failed=0; for p in '/etc/os-release' '/usr/lib/os-release' '/var/lib/dpkg/status' '/var/lib/dpkg/status.d' '/lib/apk/db/installed' '/var/lib/rpm' '/usr/lib/sysimage/rpm' '/var/lib/pacman/local'; do if [ -e \"$p\" ]; then if out=$(du -skL \"$p\" 2>/dev/null); then total=$((total + ${out%%[[:space:]]*})); else failed=1; fi; fi; done; echo \"coolify-scan-exit packages-size $failed $total\"".to_owned()
            ]
        );
        let host = format!("/data/coolify/image-scans/tmp-{RUN}/in/host");
        let copy = unwrapped(&c.sbom_from_host())
            .into_iter()
            .find(|l| l.starts_with("failed=0; "))
            .unwrap();
        assert!(copy.starts_with(&format!(
            "failed=0; if [ -e '/etc/os-release' ]; then mkdir -p \"$(dirname '{host}/etc/os-release')\" && cp -RL '/etc/os-release' '{host}/etc/os-release' 2>/dev/null || failed=1; fi; "
        )));
        assert!(copy.contains(&format!(
            "if [ -e '/var/lib/rpm' ] && [ ! -L '/var/lib/rpm' ]; then mkdir -p \"$(dirname '{host}/var/lib/rpm')\" && cp -RL '/var/lib/rpm' '{host}/var/lib/rpm' 2>/dev/null || failed=1; fi; "
        )));
        assert!(copy.ends_with(&format!(
            "found=0; for p in '{host}/var/lib/dpkg/status' '{host}/var/lib/dpkg/status.d' '{host}/lib/apk/db/installed' '{host}/var/lib/rpm' '{host}/usr/lib/sysimage/rpm' '{host}/var/lib/pacman/local'; do if [ -e \"$p\" ]; then found=1; fi; done; if [ $failed -eq 1 ]; then echo 'coolify-scan-exit copy 2'; elif [ $found -eq 1 ]; then echo 'coolify-scan-exit copy 0'; else echo 'coolify-scan-exit copy 1'; fi"
        )));
        let syft = &docker_runs(&c.sbom_from_host())[0];
        assert!(syft.contains("'scan' 'dir:/in/host' '--override-default-catalogers' 'dpkg-db-cataloger,rpm-db-cataloger,apk-db-cataloger,alpm-db-cataloger' '-o' 'syft-json' '-q' > '/data/coolify/image-scans/sboms/host.json.tmp-"));

        let containers = c.running_containers();
        assert_eq!(containers.len(), 2);
        assert_eq!(
            unwrapped(&containers)[1],
            "ids=$(docker ps -q --no-trunc 2>/dev/null); rc=$?; echo \"coolify-scan-exit containers $rc\"; echo coolify-scan-payload; if [ -n \"$ids\" ]; then docker inspect --format '{\"id\":{{json .Image}},\"ref\":{{json .Config.Image}},\"labels\":{{json .Config.Labels}}}' $ids 2>/dev/null | head -c 10485761; fi"
        );
    }

    /// Runs the filter of the host Grype step over lines, when `sh`, `grep` and `awk` exist.
    fn host_filter(lines: &[&str]) -> Option<Vec<String>> {
        use std::io::Write as _;
        let script = unwrapped(&non_root().grype(HOST_SBOM).unwrap()).join("\n");
        let filter = Regex::new(r"\| (grep -v -F .*?) > '").unwrap();
        let filter = filter.captures(&script).unwrap()[1].to_owned();
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&filter)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        child
            .stdin
            .take()?
            .write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .ok()?;
        let out = child.wait_with_output().ok()?;
        assert!(out.status.success());
        Some(
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .split('\n')
                .map(str::to_owned)
                .collect(),
        )
    }

    #[test]
    fn host_filter_keeps_a_fixed_match_next_to_an_unfixed_one() {
        let lines = [
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-1","pkg":"linux-image","ver":"6.1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-1","pkg":"linux-headers","ver":"6.1"}"#,
            r#"{"fix":"fixed","fixv":["6.2"],"id":"CVE-1","pkg":"linux-libc-dev","ver":"6.1"}"#,
            r#"{"fix":"wont-fix","fixv":[],"id":"CVE-2","pkg":"linux-image","ver":"6.1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-1","pkg":"linux-image","ver":"6.0"}"#,
        ];
        let Some(kept) = host_filter(&lines) else {
            return;
        };
        assert_eq!(kept, vec![lines[0], lines[2], lines[4]]);
    }

    #[test]
    fn host_filter_keeps_one_match_per_package_and_one_line_per_kernel() {
        let lines = [
            r#"{"eol":null}"#,
            r#"{"fix":"fixed","fixv":["3.0.15-1"],"id":"CVE-1","pkg":"libssl3","ver":"3.0.14-1"}"#,
            r#"{"fix":"fixed","fixv":["3.0.15-1"],"id":"CVE-1","pkg":"openssl","ver":"3.0.14-1"}"#,
            r#"{"fix":"fixed","fixv":["3.0.15-1"],"id":"CVE-1","pkg":"openssl","ver":"3.0.14-1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-2","pkg":"linux-headers-6.1.0-18-common","ver":"6.1.76-1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-2","pkg":"linux-image-6.1.0-18-amd64","ver":"6.1.76-1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-2","pkg":"linux-libc-dev","ver":"6.1.76-1"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-3","pkg":"kernel-core","ver":"5.14.0-503.el9"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-3","pkg":"kernel-modules","ver":"5.14.0-503.el9"}"#,
            r#"{"fix":"not-fixed","fixv":[],"id":"CVE-3","pkg":"kernelshark","ver":"5.14.0-503.el9"}"#,
        ];
        let Some(kept) = host_filter(&lines) else {
            return;
        };
        assert_eq!(
            kept,
            vec![lines[0], lines[1], lines[2], lines[5], lines[7], lines[9]]
        );
    }

    #[test]
    fn host_grype_reports_the_exit_code_of_the_run_or_of_the_filter() {
        let script = unwrapped(&non_root().grype(HOST_SBOM).unwrap())
            .into_iter()
            .find(|l| l.starts_with("ulimit -f "))
            .unwrap();
        let out = format!("'/data/coolify/image-scans/tmp-{RUN}/out'");
        assert!(script.starts_with("ulimit -f 40960; ( timeout -k 10 600 docker run "));
        assert!(script.contains(&format!(
            " 2>/dev/null; echo $? > {out}.rc ) | grep -v -F '\"fix\":\"wont-fix\"' | awk '{{ if (!match($0, /\"id\":\"[^\"]*\"/)) {{ line[++n] = $0; next }} k = substr($0, RSTART, RLENGTH); image = 0; "
        )));
        assert!(script.contains(&format!(
            "END {{ for (i = 1; i <= n; i++) print line[i] }}' > {out}; frc=$?; rc=$(cat {out}.rc 2>/dev/null || echo 1); if [ \"$rc\" -eq 0 ]; then rc=$frc; fi; if [ \"$(docker inspect"
        )));
        // An image SBOM is not filtered.
        assert!(
            !unwrapped(&non_root().grype(HEX).unwrap())
                .join("\n")
                .contains("grep -v")
        );
    }

    #[test]
    fn parses_exit_markers_before_the_payload_separator() {
        let parsed = parse_output(Some(
            "coolify-scan-exit save 0\ncoolify-scan-exit sbom 0\ncoolify-scan-exit grype 0 12\ncoolify-scan-payload\n{\"a\":1}\ncoolify-scan-payload\n{\"b\":2}",
        ));
        assert_eq!(
            parsed,
            ParsedOutput {
                exit_codes: vec![("save".into(), 0), ("sbom".into(), 0), ("grype".into(), 0)],
                sizes: vec![("grype".into(), 12)],
                oom: vec![],
                payload: "{\"a\":1}\ncoolify-scan-payload\n{\"b\":2}".into(),
            }
        );
        let no_payload = parse_output(Some("coolify-scan-exit save 0\r\ncoolify-scan-exit sbom 1"));
        assert_eq!(no_payload.exit_code("sbom"), Some(1));
        assert_eq!(no_payload.payload, "");
        assert_eq!(no_payload.exit_codes_json(), r#"{"save":0,"sbom":1}"#);
        assert_eq!(parse_output(None), ParsedOutput::default());
        assert_eq!(parse_output(None).exit_codes_json(), "[]");
    }

    #[test]
    fn ignores_payload_lines_that_look_like_markers() {
        let parsed = parse_output(Some(
            "coolify-scan-exit grype 2 9\ncoolify-scan-payload\ncoolify-scan-exit grype 0 9\n{}",
        ));
        assert_eq!(parsed.exit_code("grype"), Some(2));
        assert_eq!(parsed.size("grype"), Some(9));
        assert_eq!(parsed.payload, "coolify-scan-exit grype 0 9\n{}");

        let oom = parse_output(Some(
            "coolify-scan-oom grype\ncoolify-scan-exit grype 137 0\ncoolify-scan-payload\ncoolify-scan-oom packages",
        ));
        assert_eq!(oom.oom, vec!["grype".to_owned()]);
        assert_eq!(oom.exit_codes, vec![("grype".to_owned(), 137)]);
        assert!(
            parse_output(Some(
                "coolify-scan-exit grype 137 0\ncoolify-scan-payload\n"
            ))
            .oom
            .is_empty()
        );

        let sizes = parse_output(Some(
            "coolify-scan-exit disk-db 0 1048576\ncoolify-scan-exit disk-sboms 0 0",
        ));
        assert_eq!(
            sizes.sizes,
            vec![
                ("disk-db".to_owned(), 1_048_576),
                ("disk-sboms".to_owned(), 0)
            ]
        );
    }

    #[test]
    fn templates_are_upstreams() {
        assert!(GRYPE_TEMPLATE.starts_with("{{- $eol := dict }}\n"));
        assert!(GRYPE_TEMPLATE.ends_with("{{ toJson $m }}\n{{- end }}\n"));
        assert!(SYFT_TEMPLATE.starts_with("{{- range .artifacts }}\n"));
        let line = non_root()
            .packages(HEX)
            .unwrap()
            .into_iter()
            .find(|l| l.contains("base64 -d"))
            .unwrap();
        let expected = base64::engine::general_purpose::STANDARD.encode(SYFT_TEMPLATE);
        assert_eq!(
            line,
            format!(
                "echo '{expected}' | base64 -d | tee '/data/coolify/image-scans/tmp-{RUN}/tpl/syft.tmpl' >/dev/null"
            )
        );
    }
}
