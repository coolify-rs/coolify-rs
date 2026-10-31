//! Renders notifications for `compat/diff/notifications_diff.py`: reads the cases (argument: a
//! JSON file) and prints what each channel would send.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use coolify_jobs::notifications::{self, CommitDetails, Ctx, Mail, Notification, git_commit_link};
use serde_json::{Value, json};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: notifications_render <cases.json>");
    let text = std::fs::read_to_string(path).expect("read the cases file");
    let input: Value = serde_json::from_str(&text).expect("parse the cases file");
    let c = &input["ctx"];
    let s = |k: &str| c[k].as_str().unwrap_or_default().to_owned();
    let ctx = Ctx {
        base_url: s("base_url"),
        app_url: s("app_url"),
        app_name: s("app_name"),
        version: s("version"),
        cloud: false,
        dev: false,
    };
    let mut out = serde_json::Map::new();
    for case in input["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name").to_owned();
        let result = if let Some(n) = case.get("notification") {
            let n: Notification =
                serde_json::from_value(n.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let details = case["commit_details"].as_bool().unwrap_or(false);
            notifications::render(&ctx, &n, details, 0)
        } else if let Some(m) = case.get("mail") {
            let m: Mail =
                serde_json::from_value(m.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let r = notifications::render_mail(&ctx, &m);
            json!({"subject": r.subject, "html": r.html})
        } else {
            let k = &case["commit"];
            let text = |key: &str| k[key].as_str();
            let d = CommitDetails::resolve(
                text("commit"),
                text("commit_message"),
                text("git_branch"),
                k["is_preview"].as_bool().unwrap_or(false),
                |commit| {
                    git_commit_link(
                        text("html_url"),
                        text("git_repository"),
                        text("git_branch"),
                        commit,
                    )
                },
            );
            serde_json::to_value(d).expect("serialize")
        };
        out.insert(name, result);
    }
    println!("{}", Value::Object(out));
}
