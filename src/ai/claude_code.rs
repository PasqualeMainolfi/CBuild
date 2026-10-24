//! Runs the AI build engineer through Claude Code (`claude -p`) on the user's own login.
//! Claude Code's built-in tools are disabled, so the model acts only through cbuild's
//! MCP tools; cbuild never reads or handles the user's credentials.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::agent::{self, Outcome, Session};
use crate::toolchain::find_program;

const TOOL_PREFIX: &str = "mcp__cbuild__";

pub fn run(root: &Path, task: &str) -> Result<Outcome> {
    let claude = find_program("claude".as_ref())
        .context("Claude Code is not installed: see https://claude.com/claude-code")?;
    let session = Session::start(root)?;

    let config = json!({
        "mcpServers": {
            "cbuild": { "command": session.server, "args": session.server_args }
        }
    });
    let config_path = session.dir.join("mcp.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

    // Runs in an empty folder outside the project, without user settings, so neither the
    // repository's CLAUDE.md files nor the user's hooks and plugins shape the session.
    let mut child = Command::new(claude)
        .arg("-p")
        .arg(task)
        .arg("--append-system-prompt")
        .arg(super::system_prompt())
        .arg("--mcp-config")
        .arg(&config_path)
        .args(["--strict-mcp-config", "--setting-sources", "project,local"])
        .args(["--tools", "", "--allowedTools", "mcp__cbuild"])
        .args(["--output-format", "stream-json", "--verbose"])
        .current_dir(&session.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot start Claude Code")?;

    let mut summary = String::new();
    let stdout = child.stdout.take().expect("stdout is piped");
    for line in BufReader::new(stdout).lines() {
        let Ok(event) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        match event["type"].as_str() {
            Some("assistant") => {
                for block in blocks(&event) {
                    match block["type"].as_str() {
                        Some("tool_use") => {
                            let name = block["name"].as_str().unwrap_or_default();
                            if let Some(tool) = name.strip_prefix(TOOL_PREFIX) {
                                agent::show_call(tool, &block["input"]);
                            }
                        }
                        Some("text") => {
                            summary = block["text"].as_str().unwrap_or_default().to_string()
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => blocks(&event)
                .filter(|block| block["type"] == "tool_result")
                .for_each(|block| agent::show_result(&agent::content_text(&block["content"]))),
            Some("result") => {
                if let Some(result) = event["result"].as_str().filter(|r| !r.is_empty()) {
                    summary = result.to_string();
                }
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    session.finish("Claude Code", status, summary)
}

fn blocks(event: &Value) -> impl Iterator<Item = &Value> {
    event["message"]["content"].as_array().into_iter().flatten()
}
