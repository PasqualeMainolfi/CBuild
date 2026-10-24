//! The AI build engineer: the tools a model acts through, the MCP server exposing
//! them, and the providers that run a model.

pub mod agent;
pub mod claude_code;
pub mod codex;
pub mod mcp;
pub mod tools;

use std::env::consts;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Result;
use serde_json::Value;

use crate::infer;
use crate::toolchain::{Toolchain, find_program};
use agent::Outcome;
use tools::{Finished, MAX_BUILD_ATTEMPTS};

/// An agent CLI that runs the AI build engineer on the user's own account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Provider {
    ClaudeCode,
    Codex,
}

/// Tried in this order when the user expresses no preference.
const PROVIDERS: &[Provider] = &[Provider::ClaudeCode, Provider::Codex];

static PREFERENCE: OnceLock<Option<Provider>> = OnceLock::new();

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "Claude Code",
            Provider::Codex => "Codex",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "claude",
            Provider::Codex => "codex",
        }
    }

    fn install_url(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "https://claude.com/claude-code",
            Provider::Codex => "https://github.com/openai/codex",
        }
    }

    fn installed(self) -> bool {
        find_program(self.program().as_ref()).is_some()
    }

    pub fn run(self, root: &Path, task: &str) -> anyhow::Result<Outcome> {
        match self {
            Provider::ClaudeCode => claude_code::run(root, task),
            Provider::Codex => codex::run(root, task),
        }
    }
}

/// Records the provider the user asked for (`--provider` or `CBUILD_AI_PROVIDER`).
pub fn set_preference(preference: Option<Provider>) {
    let _ = PREFERENCE.set(preference);
}

/// The provider to use: the one asked for if installed, else the first installed one.
pub fn provider() -> Option<Provider> {
    match PREFERENCE.get().copied().flatten() {
        Some(preferred) => preferred.installed().then_some(preferred),
        None => PROVIDERS.iter().copied().find(|p| p.installed()),
    }
}

/// How to get an AI provider when none is usable.
pub fn install_hint() -> String {
    match PREFERENCE.get().copied().flatten() {
        Some(preferred) => format!(
            "the AI build engineer was asked to use {}, which is not installed: see {}",
            preferred.name(),
            preferred.install_url()
        ),
        None => {
            let options: Vec<String> = PROVIDERS
                .iter()
                .map(|p| format!("{} ({})", p.name(), p.install_url()))
                .collect();
            format!(
                "the AI build engineer can design the plan: install {}",
                options.join(" or ")
            )
        }
    }
}

const SYSTEM_PROMPT: &str = include_str!("prompt.md");

pub fn system_prompt() -> String {
    SYSTEM_PROMPT
        .replace("{os}", consts::OS)
        .replace("{arch}", consts::ARCH)
        .replace("{attempts}", &MAX_BUILD_ATTEMPTS.to_string())
}

/// What a planning session is for.
pub enum Goal<'a> {
    /// No plan yet: design one.
    Design,
    /// A plan exists but no longer builds.
    Fix {
        /// The current plan, as TOML.
        plan: &'a str,
        /// `tools::build_report` of the current plan.
        failure: &'a Value,
    },
    /// A plan builds, but the project may have changed since it was written.
    Sync {
        /// The current plan, as TOML.
        plan: &'a str,
    },
}

/// The first message of a planning session: what the model cannot discover with the tools.
pub fn task(root: &Path, toolchain: &Toolchain, goal: Goal) -> Result<String> {
    let mut text = match goal {
        Goal::Sync { .. } => String::from(
            "This C project has a build plan that still builds, but the project may have changed \
             since the plan was written: new programs or libraries, new or removed source folders, \
             changed build files (CMakeLists.txt, Makefile, ...). Compare the plan with the project \
             and its own build files, and update the plan so it builds what the project builds by \
             default. Keep every decision that still holds, and keep leaving out tests and examples \
             unless the plan already includes them. If the plan is already up to date, verify it \
             with try_build and call finish with it unchanged. The cbuild tools operate on the \
             project root.\n\n",
        ),
        Goal::Design => String::from(
            "Design and verify a build plan for this C project. The cbuild tools operate on the project root.\n\n",
        ),
        Goal::Fix { .. } => String::from(
            "This C project has a build plan that no longer builds. Fix the plan with the smallest \
             change that makes it build again, keeping the decisions that still hold. The cbuild \
             tools operate on the project root.\n\n",
        ),
    };
    writeln!(text, "Platform: {}/{}", consts::OS, consts::ARCH)?;
    writeln!(text, "Compiler: {}", toolchain.describe())?;

    let mut entries: Vec<String> = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.') && name != "target")
        .collect();
    entries.sort();
    writeln!(text, "Top-level entries: {}", entries.join(", "))?;

    match goal {
        Goal::Design => match infer::directory(root) {
            Ok(inferred) => writeln!(
                text,
                "cbuild's heuristic planner proposes: {}",
                inferred.notes.join("; ")
            )?,
            Err(err) => writeln!(text, "cbuild's heuristic planner failed: {err:#}")?,
        },
        Goal::Sync { plan } => write!(text, "\nCurrent plan:\n```toml\n{plan}```\n")?,
        Goal::Fix { plan, failure } => {
            write!(text, "\nCurrent plan:\n```toml\n{plan}```\n\n")?;
            write!(
                text,
                "Result of building it:\n```json\n{}\n```\n",
                serde_json::to_string_pretty(failure)?
            )?;
        }
    }
    Ok(text)
}

/// cbuild.toml contents for a verified plan, with its provenance.
/// `action` says what the AI did: "designed" or "updated".
pub fn manifest_text(finished: &Finished, provider: &str, action: &str) -> Result<String> {
    let mut text = format!(
        "# Plan {action} by the cbuild AI build engineer ({provider}) and verified by a real build.\n"
    );
    for line in finished
        .notes
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        writeln!(text, "#   {line}")?;
    }
    text.push('\n');
    text.push_str(&finished.plan.to_toml()?);
    Ok(text)
}
