//! Change classification.
//!
//! Classification combines three independent kinds of evidence:
//!
//! 1. **paths**: which upstream areas were touched (strong evidence);
//! 2. **content**: what the diff does (route declarations, schema builder
//!    calls, security-relevant APIs), derived from the diff text itself;
//! 3. **message**: the conventional-commit prefix (weak evidence; recorded
//!    but never sufficient on its own to assign or remove a category).
//!
//! Every category carries the evidence that produced it so reviewers (and
//! calibration jobs) can audit why a change was classified the way it was.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::git::FileChange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Documentation,
    Tests,
    Frontend,
    ApiChange,
    DatabaseSchema,
    DomainLogic,
    AuthenticationAuthorization,
    DeploymentEngine,
    ContainerIntegration,
    Configuration,
    ExternalIntegrations,
    BackgroundJobs,
    SecurityFix,
    DependencyUpgrade,
    BuildRelease,
    ServiceTemplates,
    Localization,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Path,
    Content,
    Message,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub categories: BTreeMap<Category, Vec<Evidence>>,
    /// PHP symbols (functions, methods, classes) touched by the diff.
    pub changed_symbols: BTreeSet<String>,
    pub security_signals: Vec<Evidence>,
}

impl Classification {
    pub fn has(&self, c: Category) -> bool {
        self.categories.contains_key(&c)
    }

    pub fn security_sensitive(&self) -> bool {
        self.has(Category::AuthenticationAuthorization)
            || self.has(Category::SecurityFix)
            || !self.security_signals.is_empty()
    }

    fn add(&mut self, c: Category, kind: EvidenceKind, detail: impl Into<String>) {
        self.categories.entry(c).or_default().push(Evidence {
            kind,
            detail: detail.into(),
        });
    }
}

/// Path rules, first match wins per file. Order matters: specific before generic.
const PATH_RULES: &[(&str, Category)] = &[
    ("tests/", Category::Tests),
    ("database/migrations/", Category::DatabaseSchema),
    ("database/schema/", Category::DatabaseSchema),
    ("database/seeders/", Category::DatabaseSchema),
    ("routes/api.php", Category::ApiChange),
    ("app/Http/Controllers/Api/", Category::ApiChange),
    ("openapi.", Category::ApiChange),
    ("app/Policies/", Category::AuthenticationAuthorization),
    (
        "app/Http/Middleware/",
        Category::AuthenticationAuthorization,
    ),
    (
        "app/Actions/Fortify/",
        Category::AuthenticationAuthorization,
    ),
    (
        "app/Providers/AuthServiceProvider.php",
        Category::AuthenticationAuthorization,
    ),
    (
        "app/Providers/FortifyServiceProvider.php",
        Category::AuthenticationAuthorization,
    ),
    ("config/sanctum.php", Category::AuthenticationAuthorization),
    ("config/fortify.php", Category::AuthenticationAuthorization),
    ("app/Services/Auth/", Category::AuthenticationAuthorization),
    (
        "app/Jobs/ApplicationDeploymentJob.php",
        Category::DeploymentEngine,
    ),
    (
        "app/Traits/ExecuteRemoteCommand.php",
        Category::DeploymentEngine,
    ),
    (
        "bootstrap/helpers/applications.php",
        Category::DeploymentEngine,
    ),
    ("app/Actions/Application/", Category::DeploymentEngine),
    (
        "bootstrap/helpers/remoteProcess.php",
        Category::ContainerIntegration,
    ),
    (
        "app/Helpers/SshMultiplexingHelper.php",
        Category::ContainerIntegration,
    ),
    (
        "bootstrap/helpers/docker.php",
        Category::ContainerIntegration,
    ),
    (
        "bootstrap/helpers/proxy.php",
        Category::ContainerIntegration,
    ),
    ("app/Actions/Proxy/", Category::ContainerIntegration),
    ("app/Actions/Database/", Category::ContainerIntegration),
    ("app/Actions/Service/", Category::ContainerIntegration),
    ("app/Actions/Server/", Category::ContainerIntegration),
    ("docker/coolify-terminal/", Category::ContainerIntegration),
    ("app/Jobs/", Category::BackgroundJobs),
    ("app/Console/", Category::BackgroundJobs),
    ("app/Notifications/", Category::ExternalIntegrations),
    (
        "app/Http/Controllers/Webhook/",
        Category::ExternalIntegrations,
    ),
    ("routes/webhooks.php", Category::ExternalIntegrations),
    ("app/Livewire/", Category::Frontend),
    ("resources/", Category::Frontend),
    ("public/", Category::Frontend),
    ("svgs/", Category::Frontend),
    ("routes/web.php", Category::Frontend),
    ("vite.config.js", Category::Frontend),
    ("templates/", Category::ServiceTemplates),
    ("lang/", Category::Localization),
    ("composer.json", Category::DependencyUpgrade),
    ("composer.lock", Category::DependencyUpgrade),
    ("package.json", Category::DependencyUpgrade),
    ("package-lock.json", Category::DependencyUpgrade),
    ("bun.lock", Category::DependencyUpgrade),
    (".github/", Category::BuildRelease),
    ("docker/", Category::BuildRelease),
    ("scripts/", Category::BuildRelease),
    ("docker-compose", Category::BuildRelease),
    ("versions.json", Category::BuildRelease),
    ("config/", Category::Configuration),
    ("app/Models/", Category::DomainLogic),
    ("app/Actions/", Category::DomainLogic),
    ("app/Services/", Category::DomainLogic),
    ("app/Support/", Category::DomainLogic),
    ("app/Rules/", Category::DomainLogic),
    ("app/Enums/", Category::DomainLogic),
    ("app/Traits/", Category::DomainLogic),
    ("bootstrap/helpers/", Category::DomainLogic),
    ("app/", Category::DomainLogic),
];

/// Paths that are security relevant regardless of their category.
const SECURITY_PATHS: &[&str] = &[
    "app/Policies/",
    "app/Http/Middleware/",
    "app/Rules/",
    "app/Support/ValidationPatterns.php",
    "bootstrap/helpers/remoteProcess.php",
    "bootstrap/helpers/sudo.php",
    "bootstrap/helpers/api.php",
    "app/Helpers/SshMultiplexingHelper.php",
    "docker/coolify-terminal/",
    "app/Services/TerminalSessionService.php",
    "app/Services/ContainerFilesystemService.php",
    "app/Http/Controllers/Webhook/",
    "config/sanctum.php",
];

/// Tokens in changed lines that indicate security-relevant behaviour.
const SECURITY_TOKENS: &[&str] = &[
    "escapeshellarg",
    "escapeShellValue",
    "authorize(",
    "Gate::",
    "can_read_sensitive",
    "makeVisible",
    "makeHidden",
    "hash_equals",
    "hash_hmac",
    "Crypt::",
    "encrypt(",
    "decrypt(",
    "isAdminOfTeam",
    "isInstanceAdmin",
    "api.ability",
    "tokenCan",
    "SafeWebhookUrl",
    "validateShellSafePath",
    "sudo ",
    "--privileged",
    "StrictHostKeyChecking",
];

pub fn classify(files: &[FileChange], diff: &str, subject: &str) -> Classification {
    let mut c = Classification::default();

    for f in files {
        let path = f.path.as_str();
        let doc = path.ends_with(".md") || path.starts_with("docs/") || path == "LICENSE";
        if doc {
            c.add(Category::Documentation, EvidenceKind::Path, path);
        } else if let Some((_, cat)) = PATH_RULES.iter().find(|(p, _)| path.starts_with(p)) {
            c.add(*cat, EvidenceKind::Path, path);
        } else {
            c.add(Category::Unclassified, EvidenceKind::Path, path);
        }
        if let Some(p) = SECURITY_PATHS.iter().find(|p| path.starts_with(*p)) {
            c.security_signals.push(Evidence {
                kind: EvidenceKind::Path,
                detail: format!("{path} (security path {p})"),
            });
        }
    }

    // Content evidence from changed lines only (not context lines).
    let mut current_file = String::new();
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            current_file = p.to_string();
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(header) = line.strip_prefix("@@")
            && let Some(sym) = header.split("@@").nth(1).and_then(php_symbol)
        {
            c.changed_symbols.insert(format!("{current_file}::{sym}"));
            continue;
        }
        let Some(body) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else {
            continue;
        };
        if let Some(sym) = php_symbol(body) {
            c.changed_symbols.insert(format!("{current_file}::{sym}"));
        }
        if body.contains("Route::") && current_file.starts_with("routes/api") {
            c.add(Category::ApiChange, EvidenceKind::Content, trimmed(body));
        }
        if body.contains("Schema::") && !current_file.starts_with("tests/") {
            c.add(
                Category::DatabaseSchema,
                EvidenceKind::Content,
                trimmed(body),
            );
        }
        if current_file.starts_with("tests/") || current_file.ends_with(".md") {
            continue;
        }
        if let Some(tok) = SECURITY_TOKENS.iter().find(|t| body.contains(*t)) {
            c.security_signals.push(Evidence {
                kind: EvidenceKind::Content,
                detail: format!("{current_file}: `{tok}` in changed line"),
            });
        }
    }
    dedup_signals(&mut c.security_signals);

    // Message evidence: weak, only adds a security category when other
    // evidence already points at security, and is recorded for calibration.
    let lower = subject.to_ascii_lowercase();
    let msg_type = lower
        .split([':', '('])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if !msg_type.is_empty() && msg_type.len() < 12 {
        let detail = format!("conventional commit type `{msg_type}`");
        match msg_type.as_str() {
            "docs" => c.add(Category::Documentation, EvidenceKind::Message, detail),
            "test" | "tests" => c.add(Category::Tests, EvidenceKind::Message, detail),
            _ => {}
        }
    }
    if (lower.contains("security") || lower.contains("cve") || lower.contains("vulnerab"))
        && !c.security_signals.is_empty()
    {
        c.add(
            Category::SecurityFix,
            EvidenceKind::Message,
            format!("subject mentions security: {subject}"),
        );
    }

    // Message-only categories never stand alone.
    c.categories
        .retain(|_, ev| ev.iter().any(|e| e.kind != EvidenceKind::Message));
    if c.categories.is_empty() {
        c.add(Category::Unclassified, EvidenceKind::Path, "no files");
    }
    c
}

fn trimmed(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() > 160 {
        format!("{}…", t.chars().take(160).collect::<String>())
    } else {
        t.to_string()
    }
}

/// Extracts `function name` / `class Name` from a PHP line.
fn php_symbol(line: &str) -> Option<String> {
    let line = line.trim();
    for kw in ["function ", "class ", "trait ", "interface ", "enum "] {
        if let Some(idx) = line.find(kw) {
            // Require the keyword to start a token.
            if idx > 0 && !line[..idx].ends_with(char::is_whitespace) {
                continue;
            }
            let rest = &line[idx + kw.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                let kind = kw.trim();
                return Some(if kind == "function" {
                    format!("{name}()")
                } else {
                    format!("{kind} {name}")
                });
            }
        }
    }
    None
}

fn dedup_signals(v: &mut Vec<Evidence>) {
    let mut seen = BTreeSet::new();
    v.retain(|e| seen.insert(e.detail.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc(path: &str) -> FileChange {
        FileChange {
            status: "M".into(),
            path: path.into(),
            old_path: None,
            additions: Some(1),
            deletions: Some(0),
        }
    }

    #[test]
    fn multi_category_with_evidence() {
        let files = vec![
            fc("routes/api.php"),
            fc("app/Http/Controllers/Api/ProjectController.php"),
            fc("database/migrations/2026_10_23_182201_add_canvas_position_to_projects_table.php"),
            fc("tests/Feature/Api/ProjectTest.php"),
            fc("README.md"),
        ];
        let diff = "\
+++ b/routes/api.php
@@ -1,3 +1,4 @@ Route::group
+    Route::get('/projects/{uuid}/canvas', [ProjectController::class, 'canvas']);
+++ b/app/Http/Controllers/Api/ProjectController.php
@@ -10,6 +10,9 @@ class ProjectController extends Controller
+    public function canvas(Request $request)
+        $this->authorize('view', $project);
";
        let c = classify(&files, diff, "feat(api): project canvas");
        for cat in [
            Category::ApiChange,
            Category::DatabaseSchema,
            Category::Tests,
            Category::Documentation,
        ] {
            assert!(c.has(cat), "{cat:?}");
        }
        assert!(
            c.categories[&Category::ApiChange]
                .iter()
                .any(|e| e.kind == EvidenceKind::Content)
        );
        assert!(
            c.changed_symbols
                .contains("app/Http/Controllers/Api/ProjectController.php::canvas()")
        );
        assert!(
            c.security_sensitive(),
            "authorize() in a changed line is a security signal"
        );
    }

    #[test]
    fn message_alone_cannot_classify() {
        // A "docs:" commit that actually changes a policy is not documentation-only.
        let c = classify(&[fc("app/Policies/ServerPolicy.php")], "", "docs: typo");
        assert!(!c.has(Category::Documentation));
        assert!(c.has(Category::AuthenticationAuthorization));
        assert!(c.security_sensitive());
    }

    #[test]
    fn security_subject_needs_corroboration() {
        let c = classify(
            &[fc("resources/views/x.blade.php")],
            "+<div>",
            "fix: security headers copy",
        );
        assert!(!c.has(Category::SecurityFix));
    }

    #[test]
    fn extracts_symbols() {
        assert_eq!(
            php_symbol("    public function foo_bar(int $x)").as_deref(),
            Some("foo_bar()")
        );
        assert_eq!(
            php_symbol("final class Thing extends X").as_deref(),
            Some("class Thing")
        );
        assert_eq!(php_symbol("$x = myfunction ();"), None);
    }
}
