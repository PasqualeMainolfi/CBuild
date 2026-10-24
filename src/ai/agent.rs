//! What every agent CLI provider shares: a session folder, cbuild's MCP server as the
//! model's only tools, the verified plan read back at the end, and progress on screen.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::mcp::RESULT_FILE;
use super::tools::Finished;
use crate::engine;
use crate::ui;

/// The names of cbuild's MCP tools, which providers allow and nothing else.
pub const TOOLS: &[&str] = &["list_files", "read_file", "grep", "try_build", "finish"];

pub struct Outcome {
    pub finished: Option<Finished>,
    /// The model's last words: its explanation when it could not finish.
    pub summary: String,
}

/// A planning session: where its records go, an empty folder the agent runs in, and the
/// command that starts cbuild's MCP server for it.
pub struct Session {
    /// `target/cbuild-agent/sessions/<time>/`: trace, result and logs.
    pub dir: PathBuf,
    /// The agent's working folder, empty and outside the project. Agents read instruction
    /// files (CLAUDE.md, AGENTS.md) from their working folder and every folder above it:
    /// inside the project, the repository could instruct the model.
    pub workdir: PathBuf,
    pub server: PathBuf,
    pub server_args: Vec<String>,
}

impl Session {
    pub fn start(root: &Path) -> Result<Session> {
        let target_dir = root.join("target");
        engine::prepare_target_dir(&target_dir)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let dir = target_dir
            .join("cbuild-agent/sessions")
            .join(stamp.to_string());
        fs::create_dir_all(&dir)?;
        let workdir = env::temp_dir().join(format!("cbuild-agent-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&workdir)?;
        let server_args = vec![
            "mcp".to_string(),
            "--root".to_string(),
            root.to_string_lossy().into_owned(),
            "--session".to_string(),
            dir.to_string_lossy().into_owned(),
        ];
        Ok(Session {
            dir,
            workdir,
            server: env::current_exe()?,
            server_args,
        })
    }

    /// The verified plan the MCP server saved, if the model called `finish`.
    pub fn finish(&self, provider: &str, status: ExitStatus, summary: String) -> Result<Outcome> {
        let finished = match fs::read_to_string(self.dir.join(RESULT_FILE)) {
            Ok(text) => {
                Some(serde_json::from_str(&text).context("invalid result from the MCP session")?)
            }
            Err(_) => None,
        };
        if finished.is_none() && !status.success() {
            bail!("{provider} exited with {status}: {summary}");
        }
        Ok(Outcome { finished, summary })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.workdir);
    }
}

/// Shows a tool call as it happens.
pub fn show_call(tool: &str, input: &Value) {
    let detail = match tool {
        "list_files" => input["pattern"].as_str().unwrap_or("**/*"),
        "read_file" => input["path"].as_str().unwrap_or_default(),
        "grep" => input["text"].as_str().unwrap_or_default(),
        _ => "",
    };
    ui::status("Agent", format!("{tool} {detail}").trim_end());
}

/// Reports try_build outcomes; other tool results are file contents, not worth showing.
pub fn show_result(text: &str) {
    let Ok(result) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let error = result["error"].as_str().unwrap_or_default();
    match result["status"].as_str() {
        Some("success") => ui::status("Build", "success"),
        Some("failed") => {
            let first = &result["diagnostics"][0];
            match (first["kind"].as_str(), first["subject"].as_str()) {
                (Some(kind), Some(subject)) => {
                    ui::status("Build", format!("failed: {kind} `{subject}`"))
                }
                _ => ui::status("Build", format!("failed: {error}")),
            }
        }
        Some("invalid_plan") => ui::status("Build", format!("invalid plan: {error}")),
        Some("limit_reached") => ui::status("Build", "no attempts left"),
        _ => {}
    }
}

/// The text of MCP content: a string, or a list of `{type: "text", text}` parts.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn agents_run_outside_the_project() {
        let root = std::env::temp_dir().join(format!("cbuild-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();

        let session = Session::start(&root).unwrap();
        assert!(
            session.dir.starts_with(&root),
            "records stay with the project"
        );
        assert!(
            !session.workdir.starts_with(&root),
            "instruction files in the project must not reach the agent"
        );
        let workdir = session.workdir.clone();
        assert!(workdir.is_dir());
        drop(session);
        assert!(
            !workdir.exists(),
            "the working folder is removed afterwards"
        );
    }
}
