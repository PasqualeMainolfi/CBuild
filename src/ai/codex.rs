//! Runs the AI build engineer through the Codex CLI (`codex exec`) on the user's own login.
//!
//! Codex's own tools are switched off and its sandbox is read-only, so the model acts only
//! through cbuild's MCP tools. Checked against codex-cli 0.164.0: shell commands, file
//! access from code mode (`fs`, `Deno`, `fetch`) and writes with `apply_patch` are all
//! refused. Code mode itself must stay on: Codex calls MCP tools through it.
//! cbuild never reads or handles the user's credentials.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::Value;

use super::agent::{self, Outcome, Session};
use crate::toolchain::find_program;

/// Codex features that would give the model other ways to act. An unknown name is a
/// fatal error in Codex, so a feature renamed in a later version fails loudly.
const LOG_FILE: &str = "codex.log";

const DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "multi_agent",
    "image_generation",
    "goals",
    "view_image",
    "apps",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "hooks",
    "sleep_tool",
    "tool_suggest",
    "skill_search",
];

pub fn run(root: &Path, task: &str) -> Result<Outcome> {
    let codex = find_program("codex".as_ref())
        .context("Codex is not installed: see https://github.com/openai/codex")?;
    let session = Session::start(root)?;

    let mut command = Command::new(codex);
    command
        .arg("exec")
        .args(["--json", "--skip-git-repo-check", "--ephemeral"])
        // Neither the user's configuration nor the repository's rules or AGENTS.md files
        // shape the session: it runs in an empty folder outside the project.
        .args(["--ignore-user-config", "--ignore-rules"])
        .args(["--sandbox", "read-only", "--cd"])
        .arg(&session.workdir);
    for feature in DISABLED_FEATURES {
        command.args(["--disable", feature]);
    }
    for setting in config(&session) {
        command.arg("--config").arg(setting);
    }
    // `codex exec` has no separate system prompt: the instructions lead the task.
    let mut child = command
        .arg(format!("{}\n\n{task}", super::system_prompt()))
        .current_dir(&session.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Codex's own diagnostics, kept for when it fails without saying why.
        .stderr(File::create(session.dir.join(LOG_FILE))?)
        .spawn()
        .context("cannot start Codex")?;

    let mut summary = String::new();
    let stdout = child.stdout.take().expect("stdout is piped");
    for line in BufReader::new(stdout).lines() {
        let Ok(event) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let item = &event["item"];
        match (event["type"].as_str(), item["type"].as_str()) {
            (Some("item.started"), Some("mcp_tool_call")) if item["server"] == "cbuild" => {
                agent::show_call(
                    item["tool"].as_str().unwrap_or_default(),
                    &item["arguments"],
                );
            }
            (Some("item.completed"), Some("mcp_tool_call")) if item["server"] == "cbuild" => {
                agent::show_result(&agent::content_text(&item["result"]["content"]));
            }
            (Some("item.completed"), Some("agent_message")) => {
                summary = item["text"].as_str().unwrap_or_default().to_string();
            }
            (Some("item.completed"), Some("error")) | (Some("turn.failed"), _) => {
                let message = item["message"]
                    .as_str()
                    .or(event["error"]["message"].as_str());
                if let Some(message) = message {
                    summary = message.to_string();
                }
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    if summary.is_empty() && !status.success() {
        let log = fs::read_to_string(session.dir.join(LOG_FILE)).unwrap_or_default();
        let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
        summary = lines[lines.len().saturating_sub(5)..].join("\n");
    }
    session.finish("Codex", status, summary)
}

/// `--config` settings that add cbuild's MCP server, its tools approved and no others.
fn config(session: &Session) -> Vec<String> {
    let server_args: Vec<String> = session.server_args.iter().map(|a| toml_string(a)).collect();
    let tools: Vec<String> = agent::TOOLS.iter().map(|t| toml_string(t)).collect();
    vec![
        "web_search=\"disabled\"".to_string(),
        format!(
            "mcp_servers.cbuild.command={}",
            toml_string(&session.server.to_string_lossy())
        ),
        format!("mcp_servers.cbuild.args=[{}]", server_args.join(",")),
        // Without it every MCP call fails: `codex exec` cannot ask for approval.
        "mcp_servers.cbuild.default_tools_approval_mode=\"approve\"".to_string(),
        format!("mcp_servers.cbuild.enabled_tools=[{}]", tools.join(",")),
    ]
}

/// A TOML basic string. JSON string syntax is valid TOML for every character it escapes.
fn toml_string(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn config_is_valid_toml_with_paths_that_need_escaping() {
        let session = Session {
            dir: PathBuf::from("/tmp/s"),
            workdir: PathBuf::from("/tmp/w"),
            server: PathBuf::from("/opt/my tools/cbuild\"x"),
            server_args: vec!["mcp".into(), "--root".into(), "C:\\proj".into()],
        };
        for setting in config(&session) {
            let parsed: toml::Table =
                toml::from_str(&setting).unwrap_or_else(|e| panic!("{setting}: {e}"));
            assert!(!parsed.is_empty());
        }
        let args: toml::Table = toml::from_str(&config(&session)[2]).unwrap();
        let args = &args["mcp_servers"]["cbuild"]["args"];
        assert_eq!(args[2].as_str(), Some("C:\\proj"));
    }
}
