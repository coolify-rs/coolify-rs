//! Migration planning: turns an analysed upstream change into a plan with a
//! risk level, required actions, validation commands and escalation reasons.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::classify::{Category, Classification};
use crate::inventory::RouteDiff;
use crate::mapping::ComponentMatch;
use crate::php::migrations::MigrationSummary;

pub const PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Deterministic regeneration (inventories, schemas).
    A,
    /// Template-driven code generation for well-understood patterns.
    B,
    /// AI-assisted semantic translation, validated by the pipeline.
    C,
    /// Human-led architectural work.
    D,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub layer: Layer,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationChange {
    pub name: String,
    pub summary: MigrationSummary,
    /// Tables in this migration that a Rust component reads or writes.
    pub mapped_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    pub reasons: Vec<String>,
    pub decision_required: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub schema_version: u32,
    pub upstream_from: String,
    pub upstream_to: String,
    pub commits: Vec<String>,
    pub categories: Vec<Category>,
    pub security_sensitive: bool,
    pub risk: Risk,
    pub risk_reasons: Vec<String>,
    pub affected_components: Vec<ComponentMatch>,
    /// Behaviour-relevant upstream files that no mapping entry covers.
    pub unmapped_files: Vec<String>,
    pub route_diff: RouteDiff,
    pub migrations: Vec<MigrationChange>,
    pub actions: Vec<Action>,
    pub validation: Vec<Vec<String>>,
    pub escalation: Option<Escalation>,
    /// Heuristic, NOT calibrated: fraction of behaviour-relevant files that
    /// are covered by mapping entries. See docs/MIGRATION_STRATEGY.md §8.
    pub mapping_coverage: f64,
    pub unresolved_questions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub commits: Vec<String>,
    pub classification: &'a Classification,
    pub files: Vec<String>,
    pub components: Vec<ComponentMatch>,
    pub route_diff: RouteDiff,
    pub migrations: Vec<MigrationChange>,
}

/// Files that cannot change runtime behaviour of the API server.
fn behaviour_irrelevant(path: &str) -> bool {
    // Tool/editor configuration such as .cursor/, .codex/, .github/.
    path.starts_with('.')
        || path.ends_with(".md")
        || path.starts_with("docs/")
        || path.starts_with("tests/")
        || path.starts_with("resources/")
        || path.starts_with("app/Livewire/")
        || path.starts_with("public/")
        || path.starts_with("svgs/")
        || path.starts_with("lang/")
        || path.starts_with(".github/")
        || path.starts_with("templates/")
        || path == "routes/web.php"
}

pub fn build_plan(input: PlanInput<'_>) -> Plan {
    let c = input.classification;
    let mut reasons = Vec::new();
    let mut risk = Risk::Low;
    let bump = |r: Risk, why: String, risk: &mut Risk, reasons: &mut Vec<String>| {
        if r > *risk {
            *risk = r;
        }
        reasons.push(why);
    };

    let mapped: Vec<&ComponentMatch> = input.components.iter().collect();
    let implemented_hit = mapped.iter().any(|m| {
        matches!(
            m.status,
            crate::mapping::Status::Implemented
                | crate::mapping::Status::Verified
                | crate::mapping::Status::Partial
        )
    });

    if c.security_sensitive() {
        bump(
            Risk::High,
            format!("security-sensitive ({} signals)", c.security_signals.len()),
            &mut risk,
            &mut reasons,
        );
    }
    if c.has(Category::AuthenticationAuthorization) {
        bump(
            Risk::High,
            "touches authentication/authorization".into(),
            &mut risk,
            &mut reasons,
        );
    }
    for m in &input.migrations {
        if !m.mapped_tables.is_empty() {
            bump(
                Risk::High,
                format!(
                    "migration {} changes mapped tables {:?}",
                    m.name, m.mapped_tables
                ),
                &mut risk,
                &mut reasons,
            );
        }
        if m.summary.destructive || m.summary.has_raw_sql || m.summary.touches_encryption {
            bump(
                Risk::High,
                format!(
                    "migration {} is destructive/raw-SQL/encryption (destructive={}, raw_sql={}, encryption={})",
                    m.name,
                    m.summary.destructive,
                    m.summary.has_raw_sql,
                    m.summary.touches_encryption
                ),
                &mut risk,
                &mut reasons,
            );
        }
    }
    let route_hits_implemented = !input.route_diff.is_empty() && implemented_hit;
    if route_hits_implemented {
        bump(
            Risk::Medium,
            "API routes changed in an implemented component".into(),
            &mut risk,
            &mut reasons,
        );
    }
    for cat in [
        Category::ApiChange,
        Category::DomainLogic,
        Category::DatabaseSchema,
        Category::BackgroundJobs,
        Category::DependencyUpgrade,
        Category::ExternalIntegrations,
        Category::Configuration,
        Category::DeploymentEngine,
        Category::ContainerIntegration,
    ] {
        if c.has(cat) {
            bump(
                Risk::Medium,
                format!("category {cat:?}"),
                &mut risk,
                &mut reasons,
            );
        }
    }
    // Only deployment/container paths that map to a non-planned component count.
    let engine_paths: Vec<&str> = [Category::DeploymentEngine, Category::ContainerIntegration]
        .iter()
        .filter_map(|cat| c.categories.get(cat))
        .flatten()
        .filter(|e| e.kind == crate::classify::EvidenceKind::Path)
        .map(|e| e.detail.as_str())
        .collect();
    let engine_hit = input.components.iter().any(|m| {
        m.status != crate::mapping::Status::Planned
            && m.reasons.iter().any(|r| {
                r.strip_prefix("path ")
                    .is_some_and(|p| engine_paths.contains(&p))
            })
    });
    if engine_hit {
        bump(
            Risk::High,
            "deployment/container behaviour in an implemented component".into(),
            &mut risk,
            &mut reasons,
        );
    }
    if reasons.is_empty() {
        reasons
            .push("only low-impact categories (docs/tests/frontend/templates/i18n/build)".into());
    }

    // Unmapped behaviour-relevant files.
    let mapped_reason_paths: BTreeSet<&str> = input
        .components
        .iter()
        .flat_map(|m| m.reasons.iter())
        .filter_map(|r| r.strip_prefix("path "))
        .collect();
    let relevant: Vec<&String> = input
        .files
        .iter()
        .filter(|f| !behaviour_irrelevant(f))
        .collect();
    let unmapped: Vec<String> = relevant
        .iter()
        .filter(|f| !mapped_reason_paths.contains(f.as_str()))
        .map(|f| f.to_string())
        .collect();
    let coverage = if relevant.is_empty() {
        1.0
    } else {
        (relevant.len() - unmapped.len()) as f64 / relevant.len() as f64
    };

    // Actions.
    let mut actions = Vec::new();
    if c.has(Category::ApiChange) {
        actions.push(Action {
            id: "regenerate_route_inventory".into(),
            layer: Layer::A,
            description: format!("regenerate {}", crate::inventory::ROUTES_FILE),
        });
    }
    if !input.migrations.is_empty() {
        actions.push(Action {
            id: "regenerate_migration_inventory".into(),
            layer: Layer::A,
            description: format!("regenerate {}", crate::inventory::MIGRATIONS_FILE),
        });
    }
    for m in input
        .migrations
        .iter()
        .filter(|m| !m.mapped_tables.is_empty())
    {
        for t in &m.summary.tables {
            if !m.mapped_tables.contains(&t.table) {
                continue;
            }
            let cols: Vec<String> = t
                .columns
                .iter()
                .flat_map(|cc| {
                    cc.columns
                        .iter()
                        .map(move |col| format!("{}:{}", cc.method, col))
                })
                .collect();
            actions.push(Action {
                id: format!("update_schema_mapping:{}", t.table),
                layer: Layer::B,
                description: format!(
                    "update the Rust column set / serializers for `{}` ({:?}: {})",
                    t.table,
                    t.op,
                    cols.join(", ")
                ),
            });
        }
    }
    for m in &input.components {
        if matches!(m.status, crate::mapping::Status::Planned) {
            continue;
        }
        actions.push(Action {
            id: format!("review_component:{}", m.component),
            layer: if risk == Risk::High {
                Layer::D
            } else {
                Layer::C
            },
            description: format!(
                "port behaviour change into {} ({})",
                m.component,
                m.reasons.join("; ")
            ),
        });
    }

    // Validation commands: always the workspace checks, plus mapped packages.
    let mut validation = vec![
        vec![
            "cargo".into(),
            "fmt".into(),
            "--all".into(),
            "--check".into(),
        ],
        vec![
            "cargo".into(),
            "clippy".into(),
            "--workspace".into(),
            "--all-targets".into(),
            "--".into(),
            "-D".into(),
            "warnings".into(),
        ],
    ];
    let mut packages: BTreeSet<String> = input
        .components
        .iter()
        .flat_map(|m| m.rust_packages.iter().cloned())
        .collect();
    if !actions.is_empty() {
        // Inventories are checked by coolify-api's contract tests.
        packages.insert("coolify-api".into());
        packages.insert("coolify-db".into());
    }
    if packages.is_empty() {
        validation.push(vec!["cargo".into(), "test".into(), "--workspace".into()]);
    } else {
        for p in packages {
            validation.push(vec!["cargo".into(), "test".into(), "-p".into(), p]);
        }
    }

    // Escalation.
    let mut esc = Vec::new();
    if risk == Risk::High {
        esc.push("high risk: requires explicit human approval before integration".into());
    }
    if !unmapped.is_empty() && implemented_hit {
        esc.push(format!(
            "{} behaviour-relevant files are not covered by the mapping registry",
            unmapped.len()
        ));
    }
    let mut questions = Vec::new();
    for m in &input.migrations {
        if m.summary.has_raw_sql {
            questions.push(format!(
                "{}: raw SQL cannot be analysed statically; review manually",
                m.name
            ));
        }
    }
    if c.has(Category::Unclassified) {
        questions.push("some files matched no classification rule; extend PATH_RULES".into());
    }
    let escalation = (!esc.is_empty()).then(|| Escalation {
        decision_required: if risk == Risk::High {
            "approve or reject the generated patch and assign the semantic port of the listed components".into()
        } else {
            "extend the mapping registry for the unmapped files".into()
        },
        reasons: esc,
    });

    let mut categories: Vec<Category> = c.categories.keys().copied().collect();
    categories.sort();
    Plan {
        schema_version: PLAN_SCHEMA_VERSION,
        upstream_from: input.from.to_string(),
        upstream_to: input.to.to_string(),
        commits: input.commits,
        categories,
        security_sensitive: c.security_sensitive(),
        risk,
        risk_reasons: reasons,
        affected_components: input.components,
        unmapped_files: unmapped,
        route_diff: input.route_diff,
        migrations: input.migrations,
        actions,
        validation,
        escalation,
        mapping_coverage: coverage,
        unresolved_questions: questions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify;
    use crate::git::FileChange;
    use crate::php::migrations::summarize_migration;

    fn fc(p: &str) -> FileChange {
        FileChange {
            status: "M".into(),
            path: p.into(),
            old_path: None,
            additions: None,
            deletions: None,
        }
    }

    #[test]
    fn docs_only_is_low_risk() {
        let files = vec![fc("README.md")];
        let c = classify(&files, "", "docs: x");
        let p = build_plan(PlanInput {
            from: "a",
            to: "b",
            commits: vec!["b".into()],
            classification: &c,
            files: vec!["README.md".into()],
            components: vec![],
            route_diff: RouteDiff::default(),
            migrations: vec![],
        });
        assert_eq!(p.risk, Risk::Low);
        assert!(p.escalation.is_none());
        assert!(p.actions.is_empty());
        assert_eq!(p.mapping_coverage, 1.0);
    }

    #[test]
    fn mapped_migration_is_high_and_escalates() {
        let path =
            "database/migrations/2026_10_23_182201_add_canvas_position_to_projects_table.php";
        let files = vec![fc(path)];
        let c = classify(&files, "", "feat: canvas");
        let summary = summarize_migration(
            "<?php Schema::table('projects', function (Blueprint $table) { $table->json('canvas_position')->nullable(); });",
        );
        let p = build_plan(PlanInput {
            from: "a",
            to: "b",
            commits: vec!["b".into()],
            classification: &c,
            files: vec![path.into()],
            components: vec![],
            route_diff: RouteDiff::default(),
            migrations: vec![MigrationChange {
                name: "2026_10_23_182201_add_canvas_position_to_projects_table".into(),
                summary,
                mapped_tables: vec!["projects".into()],
            }],
        });
        assert_eq!(p.risk, Risk::High);
        assert!(p.escalation.is_some());
        assert!(
            p.actions
                .iter()
                .any(|a| a.id == "regenerate_migration_inventory" && a.layer == Layer::A)
        );
        assert!(
            p.actions
                .iter()
                .any(|a| a.id == "update_schema_mapping:projects" && a.layer == Layer::B)
        );
    }
}
