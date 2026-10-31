//! `run_pre_deployment_command()` and `run_post_deployment_command()`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{Cmd, Fail, Run, SEPARATOR, Step};

fn names(containers: &[Value]) -> Vec<String> {
    containers
        .iter()
        .map(|c| {
            c.get("Names")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The `sh -c` command in a container: newlines become spaces, quotes are escaped.
fn exec_command(container: &str, command: &str) -> String {
    let flat = command.replace("\r\n", " ").replace(['\r', '\n'], " ");
    format!(
        "docker exec {container} sh -c '{}'",
        flat.replace('\'', "'\\''")
    )
}

/// `validateContainerName()`: `ValidationPatterns::CONTAINER_NAME_PATTERN`.
fn validate_container_name(name: &str) -> Step {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[a-zA-Z0-9][a-zA-Z0-9._-]*\z").unwrap_or_else(|_| unreachable!())
    });
    if !name.is_empty() && !NAME.is_match(name) {
        return Err(Fail::new(
            "Invalid container name: contains forbidden characters.",
        ));
    }
    Ok(())
}

impl Run {
    /// `resolveCommandContainer($containers, $specifiedContainerName, $commandType)`.
    async fn resolve_command_container(
        &self,
        containers: &[Value],
        specified: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let all = names(containers);
        if all.len() <= 1 {
            return all.into_iter().next();
        }
        let available = all.join(", ");
        // `empty($specifiedContainerName)`
        let Some(specified) = specified.filter(|s| !s.is_empty() && *s != "0") else {
            self.info(&format!(
                "{kind} command: Multiple containers found but no container name specified. Available: {available}"
            ))
            .await;
            return None;
        };
        let prefix = format!("{specified}-{}", self.app.uuid);
        if let Some(found) = all.iter().find(|n| n.starts_with(&prefix)) {
            return Some(found.clone());
        }
        self.info(&format!(
            "{kind} command: Container '{specified}' not found. Available: {available}"
        ))
        .await;
        None
    }

    /// `run_pre_deployment_command()`.
    pub(super) async fn run_pre_deployment_command(&mut self) -> Step {
        let Some(command) = self
            .app
            .str("pre_deployment_command")
            .filter(|c| !c.is_empty() && *c != "0")
            .map(str::to_owned)
        else {
            return Ok(());
        };
        let containers = self.current_containers().await;
        if containers.is_empty() {
            self.info("Pre-deployment command: No running containers found. Skipping.")
                .await;
            return Ok(());
        }
        self.info("Executing pre-deployment command (see debug log for output/errors).")
            .await;
        let specified = self
            .app
            .str("pre_deployment_command_container")
            .map(str::to_owned);
        let Some(container) = self
            .resolve_command_container(&containers, specified.as_deref(), "Pre-deployment")
            .await
        else {
            return Err(Fail::new(
                "Pre-deployment command: Could not find a valid container. Is the container name correct?",
            ));
        };
        validate_container_name(&container)?;
        self.exec(vec![Cmd::new(exec_command(&container, &command)).hidden()])
            .await
    }

    /// `run_post_deployment_command()`: `post_deployment()` only logs its exceptions.
    pub(super) async fn run_post_deployment_command(&mut self) -> Step {
        let Some(command) = self
            .app
            .str("post_deployment_command")
            .filter(|c| !c.is_empty() && *c != "0")
            .map(str::to_owned)
        else {
            return Ok(());
        };
        self.info(SEPARATOR).await;
        self.info("Executing post-deployment command (see debug log for output).")
            .await;
        let containers = self.current_containers().await;
        if containers.is_empty() {
            self.info("Post-deployment command: No running containers found. Skipping.")
                .await;
            return Ok(());
        }
        let specified = self
            .app
            .str("post_deployment_command_container")
            .map(str::to_owned);
        let Some(container) = self
            .resolve_command_container(&containers, specified.as_deref(), "Post-deployment")
            .await
        else {
            return Err(Fail::new(
                "Post-deployment command: Could not find a valid container. Is the container name correct?",
            ));
        };
        validate_container_name(&container)?;
        let key = "post-deployment-command-output";
        let result = self
            .exec(vec![
                Cmd::new(exec_command(&container, &command))
                    .hidden()
                    .save(key, true),
            ])
            .await;
        if let Err(Fail::Error { .. }) = result {
            let output = self.saved.get(key).cloned().unwrap_or_default();
            // Nothing is saved for a command without output.
            if !output.is_empty() {
                self.info("Post-deployment command failed.").await;
                self.log(&output, "stderr", false).await;
            }
        }
        Ok(())
    }
}
