//! Static summary of a Laravel migration file: which tables it touches and how.

use serde::{Deserialize, Serialize};

use super::lexer::{Tok, tokenize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableOp {
    Create,
    Alter,
    Drop,
    Rename { to: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnChange {
    /// Builder method, e.g. `string`, `boolean`, `dropColumn`, `renameColumn`, `foreignId`.
    pub method: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableChange {
    pub table: String,
    pub op: TableOp,
    /// Changes found inside the closure, in source order (both `up` and `down`).
    pub columns: Vec<ColumnChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationSummary {
    pub tables: Vec<TableChange>,
    /// Raw SQL (`DB::statement`, `DB::unprepared`) cannot be analysed statically.
    pub has_raw_sql: bool,
    /// Data changes (`DB::table(...)->update/insert/delete`, model writes).
    pub touches_data: bool,
    /// Encryption calls (`encrypt`, `Crypt::`) usually mean a secret re-encoding.
    pub touches_encryption: bool,
    /// Destructive operations: dropped tables or columns.
    pub destructive: bool,
}

impl MigrationSummary {
    pub fn table_names(&self) -> Vec<String> {
        let mut t: Vec<String> = self.tables.iter().map(|t| t.table.clone()).collect();
        t.sort();
        t.dedup();
        t
    }
}

/// Builder methods that do not define a column.
const NON_COLUMN_METHODS: &[&str] = &[
    "index",
    "unique",
    "primary",
    "foreign",
    "dropIndex",
    "dropUnique",
    "dropForeign",
    "dropPrimary",
    "references",
    "on",
    "onDelete",
    "onUpdate",
    "nullable",
    "default",
    "change",
    "after",
    "comment",
    "constrained",
    "cascadeOnDelete",
    "nullOnDelete",
    "useCurrent",
    "unsigned",
    "first",
    "storedAs",
    "virtualAs",
];

/// Summarizes the forward (`up()`) direction of a migration. The `down()`
/// rollback usually mirrors it with drops, which must not count as
/// destructive. Files without an `up()` method are analysed whole.
pub fn summarize_migration(src: &str) -> MigrationSummary {
    let all = tokenize(src);
    let toks = up_body(&all).unwrap_or(&all);
    let mut s = MigrationSummary::default();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].tok {
            Tok::Ident(id) if id == "Schema" && is_punct(&toks, i + 1, "::") => {
                if let (Some(Tok::Ident(method)), Some(Tok::Str(table))) = (
                    toks.get(i + 2).map(|t| &t.tok),
                    toks.get(i + 4).map(|t| &t.tok),
                ) {
                    let op = match method.as_str() {
                        "create" => Some(TableOp::Create),
                        "table" => Some(TableOp::Alter),
                        "drop" | "dropIfExists" => Some(TableOp::Drop),
                        "rename" => match toks.get(i + 6).map(|t| &t.tok) {
                            Some(Tok::Str(to)) => Some(TableOp::Rename { to: to.clone() }),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(op) = op {
                        if op == TableOp::Drop {
                            s.destructive = true;
                        }
                        let (columns, next) = if matches!(op, TableOp::Create | TableOp::Alter) {
                            closure_columns(&toks, i + 5)
                        } else {
                            (Vec::new(), i + 5)
                        };
                        if columns
                            .iter()
                            .any(|c| c.method == "dropColumn" || c.method == "dropColumns")
                        {
                            s.destructive = true;
                        }
                        s.tables.push(TableChange {
                            table: table.clone(),
                            op,
                            columns,
                        });
                        i = next;
                        continue;
                    }
                }
            }
            Tok::Ident(id) if id == "DB" && is_punct(&toks, i + 1, "::") => {
                match toks.get(i + 2).map(|t| &t.tok) {
                    Some(Tok::Ident(m)) if m == "statement" || m == "unprepared" => {
                        s.has_raw_sql = true
                    }
                    Some(Tok::Ident(m)) if m == "table" => s.touches_data = true,
                    _ => {}
                }
            }
            Tok::Ident(id) if id == "Crypt" || id == "encrypt" || id == "decrypt" => {
                s.touches_encryption = true
            }
            Tok::Ident(id) if id == "encryptString" || id == "decryptString" => {
                s.touches_encryption = true
            }
            _ => {}
        }
        i += 1;
    }
    s
}

/// Tokens of the `function up(...)` body, if present.
fn up_body(toks: &[super::lexer::Token]) -> Option<&[super::lexer::Token]> {
    let start = toks.windows(2).position(|w| {
        w[0].tok == Tok::Ident("function".into()) && w[1].tok == Tok::Ident("up".into())
    })?;
    let open = (start..toks.len()).find(|&i| toks[i].tok == Tok::Punct("{"))?;
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(open) {
        match t.tok {
            Tok::Punct("{") => depth += 1,
            Tok::Punct("}") => {
                depth -= 1;
                if depth == 0 {
                    return Some(&toks[open + 1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_punct(toks: &[super::lexer::Token], i: usize, p: &str) -> bool {
    matches!(toks.get(i).map(|t| &t.tok), Some(Tok::Punct(x)) if *x == p)
}

/// Scans the `function (Blueprint $table) { ... }` closure following a Schema call.
fn closure_columns(toks: &[super::lexer::Token], from: usize) -> (Vec<ColumnChange>, usize) {
    let mut i = from;
    while i < toks.len() && toks[i].tok != Tok::Punct("{") {
        if toks[i].tok == Tok::Punct(";") {
            return (Vec::new(), i);
        }
        i += 1;
    }
    let mut depth = 0i32;
    let mut out = Vec::new();
    while i < toks.len() {
        match &toks[i].tok {
            Tok::Punct("{") => depth += 1,
            Tok::Punct("}") => {
                depth -= 1;
                if depth == 0 {
                    return (out, i + 1);
                }
            }
            Tok::Var(v) if v == "table" && is_punct(toks, i + 1, "->") => {
                if let Some(Tok::Ident(method)) = toks.get(i + 2).map(|t| &t.tok)
                    && !NON_COLUMN_METHODS.contains(&method.as_str())
                {
                    let mut columns = Vec::new();
                    let mut j = i + 4;
                    // Collect string args up to the closing paren (handles arrays).
                    let mut paren = 1i32;
                    while j < toks.len() && paren > 0 {
                        match &toks[j].tok {
                            Tok::Punct("(") => paren += 1,
                            Tok::Punct(")") => paren -= 1,
                            Tok::Str(s) if paren == 1 || method.starts_with("drop") => {
                                columns.push(s.clone())
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    // Only the first string is the column for typed builders
                    // (`string('name', 64)` has no second string, `enum('x', [...])` does).
                    if !method.starts_with("drop") && !method.starts_with("rename") {
                        columns.truncate(1);
                    }
                    out.push(ColumnChange {
                        method: method.clone(),
                        columns,
                    });
                }
            }
            _ => {}
        }
        i += 1;
    }
    (out, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_alter_and_drop() {
        let src = r#"<?php
return new class extends Migration {
    public function up(): void {
        Schema::table('projects', function (Blueprint $table) {
            $table->json('canvas_position')->nullable()->after('icon_s3_storage_id');
            $table->index(['team_id']);
        });
        DB::table('projects')->update(['x' => 1]);
    }
    public function down(): void {
        Schema::table('projects', function (Blueprint $table) {
            $table->dropColumn('canvas_position');
        });
    }
};"#;
        let s = summarize_migration(src);
        assert_eq!(s.table_names(), vec!["projects"]);
        // Only up(): the dropColumn in down() is a rollback, not a destructive change.
        assert_eq!(s.tables.len(), 1);
        assert_eq!(s.tables[0].op, TableOp::Alter);
        assert_eq!(
            s.tables[0].columns,
            vec![ColumnChange {
                method: "json".into(),
                columns: vec!["canvas_position".into()]
            }]
        );
        assert!(s.touches_data);
        assert!(!s.destructive);
    }

    #[test]
    fn create_and_raw_sql() {
        let src = r#"<?php
Schema::create('things', function (Blueprint $table) {
    $table->id();
    $table->string('name', 64);
    $table->enum('kind', ['a', 'b']);
    $table->dropColumn(['x', 'y']);
});
DB::statement('ALTER TABLE things SET (fillfactor = 70)');
$v = Crypt::encryptString('x');
"#;
        let s = summarize_migration(src);
        let cols: Vec<_> = s.tables[0]
            .columns
            .iter()
            .map(|c| (c.method.as_str(), c.columns.clone()))
            .collect();
        assert_eq!(cols[0], ("id", vec![]));
        assert_eq!(cols[1], ("string", vec!["name".to_string()]));
        assert_eq!(cols[2], ("enum", vec!["kind".to_string()]));
        assert_eq!(
            cols[3],
            ("dropColumn", vec!["x".to_string(), "y".to_string()])
        );
        assert!(s.has_raw_sql);
        assert!(s.touches_encryption);
    }
}
