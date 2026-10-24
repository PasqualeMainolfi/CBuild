//! The tools the AI build engineer acts through. Shared by the MCP server and the
//! internal agent loops: whatever the model, every action is validated here.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::thread;

use anyhow::{Context, Result, anyhow, ensure};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::diagnostics;
use crate::engine::{self, BuildFailure, BuildOptions, Capture};
use crate::plan::{self, Plan};
use crate::toolchain::{Profile, Toolchain};

pub const MAX_BUILD_ATTEMPTS: usize = 15;
const MAX_LISTED_FILES: usize = 400;
const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 400;
const MAX_GREP_MATCHES: usize = 100;
const MAX_FILE_BYTES: u64 = 1 << 20;
const MAX_OUTPUT_LINES: usize = 60;

/// File names that look like secrets: never shown to the model.
const SECRET_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keystore"];
const SECRET_PREFIXES: &[&str] = &["id_rsa", "id_ed25519", "id_ecdsa", "credentials", "secrets"];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListFiles {
    /// Glob pattern relative to the project root, e.g. "src/**/*.c". Default: every file.
    pattern: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFile {
    /// Path relative to the project root.
    path: String,
    /// First line to read, 1-based (default 1).
    offset: Option<usize>,
    /// Number of lines to read (default 200, at most 400).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Grep {
    /// Exact, case-sensitive text to find.
    text: String,
    /// Glob pattern restricting the searched files, e.g. "**/*.h". Default: every file.
    glob: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TryBuild {
    plan: Plan,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Finish {
    /// Exactly a plan that try_build reported as successful.
    plan: Plan,
    /// Short explanation of the main decisions, for the user.
    notes: String,
}

/// The tools' MCP descriptions, also usable as function-calling definitions.
pub fn definitions() -> Vec<Value> {
    vec![
        tool::<ListFiles>(
            "list_files",
            "List the project's files, as paths relative to the project root. Hidden files and \
             cbuild's own outputs are skipped. Long listings are cut: use a narrower pattern.",
        ),
        tool::<ReadFile>(
            "read_file",
            "Read a text file of the project, with line numbers. Use offset and limit for long files.",
        ),
        tool::<Grep>(
            "grep",
            "Find the lines of project files containing an exact text.",
        ),
        tool::<TryBuild>(
            "try_build",
            "Build the project with a plan and report the result: success, the plan's validation \
             error, or the classified compiler/linker diagnostics with the raw output. Each call \
             uses one of the limited build attempts.",
        ),
        tool::<Finish>(
            "finish",
            "Submit the final plan, which must be exactly a plan that try_build reported as \
             successful. Ends the session.",
        ),
    ]
}

fn tool<T: JsonSchema>(name: &str, description: &str) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema_of::<T>() })
}

/// Self-contained JSON Schema of `T`: no `$ref`, which small models handle poorly.
pub fn schema_of<T: JsonSchema>() -> Value {
    let mut schema = SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
    }
    schema
}

pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

/// A verified plan submitted with `finish`.
#[derive(Serialize, Deserialize)]
pub struct Finished {
    pub plan: Plan,
    pub notes: String,
}

pub struct Toolbox {
    root: PathBuf,
    toolchain: Toolchain,
    attempts: usize,
    /// Plans that built successfully, as JSON, for `finish` to check against.
    verified: Vec<Value>,
    pub finished: Option<Finished>,
}

impl Toolbox {
    pub fn new(root: &Path) -> Result<Toolbox> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open {}", root.display()))?;
        Ok(Toolbox {
            root,
            toolchain: Toolchain::detect()?,
            attempts: 0,
            verified: Vec::new(),
            finished: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn call(&mut self, name: &str, input: &Value) -> ToolResult {
        let result = match name {
            "list_files" => parse(input).and_then(|input| self.list_files(input)),
            "read_file" => parse(input).and_then(|input| self.read_file(input)),
            "grep" => parse(input).and_then(|input| self.grep(input)),
            "try_build" => parse(input).map(|input| self.try_build(input)),
            "finish" => parse(input).and_then(|input| self.finish(input)),
            _ => Err(anyhow!("unknown tool `{name}`")),
        };
        match result {
            Ok(text) => ToolResult {
                text,
                is_error: false,
            },
            Err(err) => ToolResult {
                text: format!("{err:#}"),
                is_error: true,
            },
        }
    }

    fn list_files(&self, input: ListFiles) -> Result<String> {
        let pattern = input.pattern.unwrap_or_else(|| "**/*".to_string());
        let files: Vec<PathBuf> = plan::glob_files(&self.root, &pattern)?
            .into_iter()
            .filter(|file| self.visible(file))
            .collect();
        if files.is_empty() {
            return Ok(format!("no files match `{pattern}`"));
        }
        let mut text = String::new();
        for file in files.iter().take(MAX_LISTED_FILES) {
            writeln!(text, "{}", file.display())?;
        }
        if files.len() > MAX_LISTED_FILES {
            writeln!(
                text,
                "... {} more files: use a narrower pattern",
                files.len() - MAX_LISTED_FILES
            )?;
        }
        Ok(text)
    }

    fn read_file(&self, input: ReadFile) -> Result<String> {
        let full = self.checked_path(&input.path)?;
        let meta = fs::metadata(&full)?;
        ensure!(meta.is_file(), "`{}` is not a file", input.path);
        ensure!(
            meta.len() <= MAX_FILE_BYTES,
            "`{}` is too large ({} bytes)",
            input.path,
            meta.len()
        );
        let bytes = fs::read(&full)?;
        ensure!(!bytes.contains(&0), "`{}` is a binary file", input.path);

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let start = input.offset.unwrap_or(1).max(1);
        if start > lines.len() {
            return Ok(format!("`{}` has {} lines", input.path, lines.len()));
        }
        let limit = input
            .limit
            .unwrap_or(DEFAULT_READ_LINES)
            .clamp(1, MAX_READ_LINES);
        let end = (start - 1 + limit).min(lines.len());
        let mut out = String::new();
        for (n, line) in lines[start - 1..end].iter().enumerate() {
            writeln!(out, "{:>5}| {line}", start + n)?;
        }
        if end < lines.len() {
            writeln!(
                out,
                "... {} more lines (continue with offset {})",
                lines.len() - end,
                end + 1
            )?;
        }
        Ok(out)
    }

    fn grep(&self, input: Grep) -> Result<String> {
        ensure!(!input.text.is_empty(), "the search text is empty");
        let pattern = input.glob.unwrap_or_else(|| "**/*".to_string());
        let mut matches = Vec::new();
        let mut total = 0;
        for file in plan::glob_files(&self.root, &pattern)? {
            if !self.visible(&file) || is_secret(&file) {
                continue;
            }
            let full = self.root.join(&file);
            if fs::metadata(&full).map_or(true, |meta| meta.len() > MAX_FILE_BYTES) {
                continue;
            }
            let Ok(bytes) = fs::read(&full) else { continue };
            if bytes.contains(&0) {
                continue;
            }
            for (n, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                if line.contains(&input.text) {
                    total += 1;
                    if matches.len() < MAX_GREP_MATCHES {
                        let line: String = line.trim().chars().take(200).collect();
                        matches.push(format!("{}:{}: {line}", file.display(), n + 1));
                    }
                }
            }
        }
        if matches.is_empty() {
            return Ok(format!("`{}` not found", input.text));
        }
        let mut text = matches.join("\n");
        if total > matches.len() {
            write!(
                text,
                "\n... {} more matches: narrow the glob",
                total - matches.len()
            )?;
        }
        Ok(text)
    }

    fn try_build(&mut self, input: TryBuild) -> String {
        let result = if self.attempts >= MAX_BUILD_ATTEMPTS {
            json!({
                "status": "limit_reached",
                "error": "no build attempts left: call finish with a plan that already built, or stop and explain the problem",
            })
        } else {
            self.attempts += 1;
            let mut result = self.build(&input.plan);
            result["attempts_left"] = json!(MAX_BUILD_ATTEMPTS - self.attempts);
            result
        };
        serde_json::to_string_pretty(&result).expect("JSON values serialize")
    }

    fn build(&mut self, plan: &Plan) -> Value {
        let report = build_report(plan, &self.root, &self.toolchain);
        if report["status"] == "success" {
            self.verified
                .push(serde_json::to_value(plan).expect("plans serialize"));
        }
        report
    }

    fn finish(&mut self, input: Finish) -> Result<String> {
        let plan = serde_json::to_value(&input.plan)?;
        ensure!(
            self.verified.contains(&plan),
            "this plan has not built successfully: call try_build with exactly this plan first"
        );
        self.finished = Some(Finished {
            plan: input.plan,
            notes: input.notes,
        });
        Ok("Plan accepted. The session is complete.".to_string())
    }

    /// Hidden files and cbuild's own outputs are invisible to the model.
    fn visible(&self, path: &Path) -> bool {
        let hidden = path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
        let ours = path.starts_with("target") && engine::owns_target_dir(&self.root.join("target"));
        !hidden && !ours
    }

    fn checked_path(&self, path: &str) -> Result<PathBuf> {
        let relative = Path::new(path);
        let inside = !path.is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        ensure!(
            inside,
            "`{path}` must be a relative path inside the project"
        );
        ensure!(
            self.visible(relative),
            "`{path}` is hidden or a cbuild output, so it cannot be read"
        );
        ensure!(
            !is_secret(relative),
            "`{path}` looks like a secret, so it cannot be read"
        );
        // Symlinks could lead outside the project.
        let full = self
            .root
            .join(relative)
            .canonicalize()
            .with_context(|| format!("`{path}` does not exist"))?;
        ensure!(
            full.starts_with(&self.root),
            "`{path}` points outside the project"
        );
        Ok(full)
    }
}

/// Builds `plan` in cbuild's scratch directory and describes the outcome as the model sees it.
pub fn build_report(plan: &Plan, root: &Path, toolchain: &Toolchain) -> Value {
    let project = match plan::resolve(plan, root) {
        Ok(project) => project,
        Err(err) => return json!({ "status": "invalid_plan", "error": format!("{err:#}") }),
    };
    let target_dir = root.join("target");
    let options = BuildOptions {
        profile: Profile::Dev,
        jobs: thread::available_parallelism().map_or(1, |n| n.get()),
        out_dir: target_dir.join("cbuild-agent/build"),
        color: false,
    };
    let mut capture = Capture::default();
    let built = engine::prepare_target_dir(&target_dir)
        .and_then(|()| engine::build(&project, toolchain, &options, &mut capture));
    match built {
        Ok(_) => {
            let targets: Vec<&str> = project.targets.iter().map(|t| t.name.as_str()).collect();
            json!({ "status": "success", "targets": targets, "warnings": truncate(&capture.output) })
        }
        Err(err) => failure_report(&err),
    }
}

/// A failed build as the model sees it: classified diagnostics plus the start of the raw output.
pub fn failure_report(err: &anyhow::Error) -> Value {
    let Some(failure) = err.downcast_ref::<BuildFailure>() else {
        return json!({ "status": "failed", "error": format!("{err:#}") });
    };
    let diagnostics: Vec<Value> = diagnostics::classify(&failure.output)
        .iter()
        .map(|d| json!({ "kind": d.kind(), "subject": d.subject(), "hint": diagnostics::hint(d) }))
        .collect();
    json!({
        "status": "failed",
        "error": failure.message,
        "diagnostics": diagnostics,
        "output": truncate(&failure.output),
    })
}

fn parse<T: DeserializeOwned>(input: &Value) -> Result<T> {
    serde_json::from_value(input.clone()).context("invalid tool input")
}

fn is_secret(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    SECRET_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || SECRET_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// The first lines of a long tool output.
fn truncate(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_OUTPUT_LINES {
        return text.to_string();
    }
    format!(
        "{}\n... {} more lines",
        lines[..MAX_OUTPUT_LINES].join("\n"),
        lines.len() - MAX_OUTPUT_LINES
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolbox(name: &str, files: &[(&str, &str)]) -> Toolbox {
        let dir = std::env::temp_dir().join(format!("cbuild-tools-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, contents) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        Toolbox::new(&dir).unwrap()
    }

    #[test]
    fn schemas_are_self_contained() {
        for tool in definitions() {
            let schema = tool["inputSchema"].to_string();
            assert!(!schema.contains("$ref"), "{}: {schema}", tool["name"]);
        }
    }

    #[test]
    fn never_reads_hidden_files_secrets_or_outside_paths() {
        let mut tools = toolbox(
            "secrets",
            &[
                (".env", "TOKEN=x"),
                ("certs/server.key", "k"),
                ("main.c", "int main(void){return 0;}"),
            ],
        );
        for path in [".env", "certs/server.key", "../etc/passwd", "/etc/passwd"] {
            let result = tools.call("read_file", &json!({ "path": path }));
            assert!(result.is_error, "{path} should not be readable");
        }
        let listing = tools.call("list_files", &json!({})).text;
        assert!(
            !listing.contains(".env") && listing.contains("main.c"),
            "{listing}"
        );
    }

    #[test]
    fn finish_requires_a_verified_plan() {
        let mut tools = toolbox("finish", &[("hello.c", "int main(void) { return 0; }\n")]);
        let plan =
            json!({ "targets": [{ "name": "hello", "kind": "executable", "sources": ["*.c"] }] });

        let early = tools.call("finish", &json!({ "plan": plan, "notes": "" }));
        assert!(early.is_error);

        let build = tools.call("try_build", &json!({ "plan": plan }));
        assert!(build.text.contains("\"success\""), "{}", build.text);
        let done = tools.call(
            "finish",
            &json!({ "plan": plan, "notes": "single program" }),
        );
        assert!(!done.is_error, "{}", done.text);
        assert!(tools.finished.is_some());
    }

    #[test]
    fn build_failures_are_classified() {
        let mut tools = toolbox(
            "failure",
            &[(
                "main.c",
                "#include <nowhere.h>\nint main(void) { return 0; }\n",
            )],
        );
        let plan =
            json!({ "targets": [{ "name": "main", "kind": "executable", "sources": ["*.c"] }] });
        let result: Value =
            serde_json::from_str(&tools.call("try_build", &json!({ "plan": plan })).text).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["diagnostics"][0]["kind"], "missing_header");
        assert_eq!(result["diagnostics"][0]["subject"], "nowhere.h");
    }
}
