//! Minimal MCP server over stdio (JSON-RPC 2.0, one message per line) exposing the tools.
//! Any MCP client can drive cbuild this way, including agent CLIs running on the
//! user's own subscription.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::tools::{self, Finished, ToolResult, Toolbox};
use crate::plan::MANIFEST;

const PROTOCOL_VERSION: &str = "2025-06-18";
pub const RESULT_FILE: &str = "result.json";
const TRACE_FILE: &str = "trace.jsonl";

/// Serves until stdin closes. With a `session` directory the verified plan goes to
/// `result.json` there and every tool call is traced; without one it becomes cbuild.toml.
pub fn serve(root: &Path, session: Option<&Path>) -> Result<()> {
    let mut toolbox = Toolbox::new(root)?;
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut toolbox, session, &message)?,
            Err(err) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {err}") },
            })),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle(toolbox: &mut Toolbox, session: Option<&Path>, message: &Value) -> Result<Option<Value>> {
    // Notifications carry no id and get no reply.
    let Some(id) = message.get("id").cloned() else {
        return Ok(None);
    };
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "cbuild", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "cbuild designs and verifies build plans for C projects: explore with \
                list_files, read_file and grep, verify a plan with try_build, then submit it with finish.",
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::definitions() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let mut outcome = toolbox.call(name, &arguments);
            if let Some(finished) = toolbox.finished.take()
                && let Err(err) = save(toolbox.root(), session, &finished)
            {
                outcome = ToolResult {
                    text: format!("{err:#}"),
                    is_error: true,
                };
            }
            if let Some(session) = session {
                trace(session, name, &arguments, &outcome)?;
            }
            json!({ "content": [{ "type": "text", "text": outcome.text }], "isError": outcome.is_error })
        }
        method => {
            return Ok(Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") },
            })));
        }
    };
    Ok(Some(
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    ))
}

fn save(root: &Path, session: Option<&Path>, finished: &Finished) -> Result<()> {
    if let Some(session) = session {
        fs::write(
            session.join(RESULT_FILE),
            serde_json::to_string_pretty(finished)?,
        )?;
        return Ok(());
    }
    let manifest = root.join(MANIFEST);
    if manifest.exists() {
        bail!("{MANIFEST} already exists: delete it to save a new plan");
    }
    fs::write(
        manifest,
        super::manifest_text(finished, "MCP client", "designed")?,
    )?;
    Ok(())
}

/// One line per tool call: the raw material for evaluation and training data.
fn trace(session: &Path, name: &str, arguments: &Value, outcome: &ToolResult) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(session.join(TRACE_FILE))?;
    let entry = json!({ "tool": name, "input": arguments, "output": outcome.text, "is_error": outcome.is_error });
    writeln!(file, "{entry}")?;
    Ok(())
}
