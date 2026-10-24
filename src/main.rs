mod ai;
mod diagnostics;
mod engine;
mod export;
mod infer;
mod plan;
mod scaffold;
mod scan;
mod selfupdate;
mod toolchain;
mod ui;

use std::env;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode, ExitStatus};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Args, Parser, Subcommand};

use crate::ai::tools::Finished;
use crate::engine::{BuildFailure, BuildOptions, Executable, Terminal};
use crate::plan::{MANIFEST, Plan, Project};
use crate::toolchain::{Profile, Toolchain};

const AI_HINT: &str = "let the AI build engineer design the plan with `cbuild plan --ai`";
const FIX_HINT: &str = "if the project changed (new files, new libraries), `cbuild fix` lets the AI build engineer update the plan";

#[derive(Parser)]
#[command(
    name = "cbuild",
    version,
    about = "Build and run C projects without writing build files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Agent that runs the AI build engineer (default: the first installed, Claude Code first)
    #[arg(long, global = true, value_enum, env = "CBUILD_AI_PROVIDER")]
    provider: Option<ai::Provider>,
}

#[derive(Subcommand)]
enum Command {
    /// Compile the project, or a single .c file
    Build(BuildArgs),
    /// Compile and run the project, or a single .c file
    Run {
        #[command(flatten)]
        build: BuildArgs,
        /// Executable to run when the project has several
        #[arg(long)]
        bin: Option<String>,
        /// Arguments passed to the program (after `--`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Show the build plan; without cbuild.toml it is inferred from the sources
    Plan {
        /// Save the inferred plan as cbuild.toml
        #[arg(long)]
        write: bool,
        /// Let the AI build engineer design and verify the plan
        #[arg(long)]
        ai: bool,
        /// Do not ask for confirmation before sending project files to the model
        #[arg(long, short)]
        yes: bool,
    },
    /// Let the AI build engineer repair a plan that no longer builds
    Fix {
        /// Do not ask for confirmation before sending project files to the model
        #[arg(long, short)]
        yes: bool,
    },
    /// Create a new project laid out for cbuild
    New {
        /// Folder to create; its name becomes the project name
        path: PathBuf,
        /// Create a library (with a demo program) instead of a program
        #[arg(long)]
        lib: bool,
        /// Do not start a git repository
        #[arg(long)]
        no_git: bool,
    },
    /// Set up cbuild in an existing folder
    ///
    /// With C sources: writes a cbuild.toml verified by a real build. Without: creates a
    /// starter program (or library), never overwriting existing files.
    Init {
        /// Folder to set up (default: current directory)
        path: Option<PathBuf>,
        /// For a folder without sources: create a library instead of a program
        #[arg(long)]
        lib: bool,
        /// Do not start a git repository
        #[arg(long)]
        no_git: bool,
        /// Do not ask for confirmation before sending project files to the model
        #[arg(long, short)]
        yes: bool,
        /// Never use the AI build engineer
        #[arg(long, conflicts_with = "yes")]
        no_ai: bool,
    },
    /// Let the AI build engineer bring the plan in line with a project that changed
    Sync {
        /// Do not ask before sending project files to the model, nor before applying changes
        #[arg(long, short)]
        yes: bool,
    },
    /// Update cbuild itself to the latest version
    Update {
        /// Only check whether a newer version exists
        #[arg(long)]
        check: bool,
        /// Where to look for updates: a git URL with version tags, or a local checkout
        #[arg(long)]
        from: Option<String>,
    },
    /// Write the plan as a CMakeLists.txt or a Makefile
    Export {
        /// Build system to export to
        #[arg(value_enum)]
        format: export::Format,
        /// File to write (default: CMakeLists.txt or Makefile in the project root)
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Overwrite a file that cbuild did not generate
        #[arg(long)]
        force: bool,
    },
    /// Remove build outputs
    Clean,
    /// Serve cbuild's planning tools to an MCP client over stdio
    Mcp {
        /// Project root (default: current directory)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Session directory receiving the verified plan and a trace of tool calls
        #[arg(long)]
        session: Option<PathBuf>,
    },
}

#[derive(Args)]
struct BuildArgs {
    /// A single .c file to build instead of the project in the current directory
    file: Option<PathBuf>,
    /// Build with optimizations
    #[arg(long, short)]
    release: bool,
    /// Number of parallel compiler jobs
    #[arg(long, short)]
    jobs: Option<usize>,
    /// Let the AI build engineer design or repair the plan without asking first
    #[arg(long, short)]
    yes: bool,
    /// Never use the AI build engineer: only cbuild.toml or the heuristics
    #[arg(long, conflicts_with = "yes")]
    no_ai: bool,
}

impl BuildArgs {
    /// Whether a failing plan may be handed to the AI build engineer (after consent).
    fn ai_allowed(&self) -> bool {
        !self.no_ai && self.file.is_none() && ai::provider().is_some()
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    ai::set_preference(cli.provider);
    let result = match cli.command {
        Command::Build(args) => build(&args).map(|_| ExitCode::SUCCESS),
        Command::Run { build, bin, args } => run(&build, bin.as_deref(), &args),
        Command::Plan { ai: true, yes, .. } => plan_with_ai(yes).map(|_| ExitCode::SUCCESS),
        Command::Plan { write, .. } => show_plan(write).map(|_| ExitCode::SUCCESS),
        Command::Fix { yes } => fix(yes).map(|_| ExitCode::SUCCESS),
        Command::New { path, lib, no_git } => new(&path, lib, !no_git).map(|_| ExitCode::SUCCESS),
        Command::Init {
            path,
            lib,
            no_git,
            yes,
            no_ai,
        } => init(path.as_deref(), lib, !no_git, yes, no_ai).map(|_| ExitCode::SUCCESS),
        Command::Sync { yes } => sync(yes).map(|_| ExitCode::SUCCESS),
        Command::Update { check, from } => {
            selfupdate::update(from, check).map(|_| ExitCode::SUCCESS)
        }
        Command::Export {
            format,
            output,
            force,
        } => export_plan(format, output.as_deref(), force).map(|_| ExitCode::SUCCESS),
        Command::Clean => clean().map(|_| ExitCode::SUCCESS),
        Command::Mcp { root, session } => serve_mcp(root, session).map(|_| ExitCode::SUCCESS),
    };
    result.unwrap_or_else(|err| {
        ui::error(format!("{err:#}"));
        ExitCode::FAILURE
    })
}

struct Workspace {
    project: Project,
    /// Where build outputs go: `target/` for projects, a user cache for single files.
    target_dir: PathBuf,
    single_file: bool,
    /// The heuristic plan, when there is no cbuild.toml.
    inferred: Option<infer::Inferred>,
}

fn manifest_root(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|dir| dir.join(MANIFEST).is_file())
}

fn load(args: &BuildArgs) -> Result<Workspace> {
    if let Some(file) = args.file.as_deref() {
        let (root, inferred) = infer::single_file(file)?;
        let project = plan::resolve(&inferred.plan, &root)?;
        let target_dir = single_file_dir(file)?;
        return Ok(Workspace {
            project,
            target_dir,
            single_file: true,
            inferred: None,
        });
    }

    let cwd = env::current_dir()?;
    if let Some(root) = manifest_root(&cwd) {
        let plan = Plan::load(&root.join(MANIFEST))?;
        let project = plan::resolve(&plan, root).inspect_err(|_| ui::hint(FIX_HINT))?;
        return Ok(Workspace {
            project,
            target_dir: root.join("target"),
            single_file: false,
            inferred: None,
        });
    }

    let inferred = match infer::directory(&cwd) {
        Ok(inferred) => inferred,
        Err(err) if args.ai_allowed() => {
            // The heuristics cannot handle this project: the AI designs the plan once,
            // then every later build reads cbuild.toml.
            ui::note(format!(
                "cbuild's heuristics cannot plan this project: {err:#}"
            ));
            design_with_ai(
                &cwd,
                args.yes,
                "No build plan yet. Let the AI build engineer design it?",
            )?;
            let plan = Plan::load(&cwd.join(MANIFEST))?;
            let project = plan::resolve(&plan, &cwd)?;
            return Ok(Workspace {
                project,
                target_dir: cwd.join("target"),
                single_file: false,
                inferred: None,
            });
        }
        Err(err) => {
            if !args.no_ai {
                ui::hint(ai::install_hint());
            }
            return Err(err);
        }
    };
    for warning in &inferred.warnings {
        ui::note(warning);
    }
    ui::status(
        "Inferred",
        format!("plan from the sources, no {MANIFEST} (see `cbuild plan`)"),
    );
    let project = plan::resolve(&inferred.plan, &cwd)?;
    Ok(Workspace {
        project,
        target_dir: cwd.join("target"),
        single_file: false,
        inferred: Some(inferred),
    })
}

/// Single files keep their outputs in a user cache, never next to the file.
fn single_file_dir(file: &Path) -> Result<PathBuf> {
    let file = file.canonicalize()?;
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    Ok(base
        .join(".cbuild/cache/single")
        .join(format!("{stem}-{:016x}", hasher.finish())))
}

fn build(args: &BuildArgs) -> Result<Vec<Executable>> {
    let start = Instant::now();
    let workspace = load(args)?;
    let toolchain = Toolchain::detect()?;
    let profile = if args.release {
        Profile::Release
    } else {
        Profile::Dev
    };
    let jobs = args
        .jobs
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, |n| n.get()));

    engine::prepare_target_dir(&workspace.target_dir)?;
    let out_dir = workspace.target_dir.join(profile.dir());
    let options = BuildOptions {
        profile,
        jobs,
        out_dir: out_dir.clone(),
        color: ui::color_enabled(),
    };
    let executables = match engine::build(&workspace.project, &toolchain, &options, &mut Terminal) {
        Ok(executables) => executables,
        Err(err) => {
            let Some(failure) = err.downcast_ref::<BuildFailure>() else {
                return Err(err);
            };
            diagnostics::report(&failure.output);
            // A project with its own build files compiles with them: when the plan inferred
            // for it fails, the plan is at fault, whatever the error looks like.
            let plan_at_fault = diagnostics::plan_related(&failure.output)
                || workspace
                    .inferred
                    .as_ref()
                    .is_some_and(|i| i.has_build_system);
            if workspace.single_file || !plan_at_fault {
                return Err(err);
            }
            match &workspace.inferred {
                // Nobody wrote this plan: offer the AI right away, then build again.
                Some(inferred) if args.ai_allowed() => {
                    ui::error(&failure.message);
                    let question = "The plan inferred from the sources does not build. Let the AI build engineer fix it?";
                    fix_with_ai(
                        &workspace.project.root,
                        &inferred.plan,
                        &ai::tools::failure_report(&err),
                        args.yes,
                        question,
                    )?;
                    return build(args);
                }
                Some(_) if !args.no_ai => ui::hint(ai::install_hint()),
                Some(_) => {}
                // A cbuild.toml is never changed without being asked.
                None => ui::hint(FIX_HINT),
            }
            return Err(err);
        }
    };
    if !workspace.single_file {
        engine::publish_compile_commands(&workspace.project.root, &workspace.target_dir, &out_dir)?;
    }
    ui::status(
        "Finished",
        format!(
            "{} in {:.2}s",
            profile.name(),
            start.elapsed().as_secs_f64()
        ),
    );
    Ok(executables)
}

fn run(args: &BuildArgs, bin: Option<&str>, program_args: &[String]) -> Result<ExitCode> {
    let executables = build(args)?;
    let names = || {
        executables
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let executable = match bin {
        Some(name) => executables
            .iter()
            .find(|e| e.name == name)
            .with_context(|| format!("no executable named `{name}` (available: {})", names()))?,
        None => match executables.as_slice() {
            [executable] => executable,
            [] => bail!("the project has no executable to run"),
            _ => bail!(
                "the project has several executables, choose one with --bin: {}",
                names()
            ),
        },
    };

    let cwd = env::current_dir()?;
    let shown = executable
        .path
        .strip_prefix(&cwd)
        .unwrap_or(&executable.path);
    ui::status("Running", shown.display());
    let status = process::Command::new(&executable.path)
        .args(program_args)
        .status()
        .with_context(|| format!("cannot start {}", executable.path.display()))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(code as u8);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let reason = match signal {
                6 => " (SIGABRT: the program aborted, often a failed assert())",
                8 => " (SIGFPE: arithmetic error, such as an integer division by zero)",
                11 => " (SIGSEGV: invalid memory access, such as a NULL or dangling pointer)",
                _ => "",
            };
            ui::error(format!(
                "the program was terminated by signal {signal}{reason}"
            ));
            return ExitCode::from((128 + signal) as u8);
        }
    }
    ExitCode::FAILURE
}

fn show_plan(write: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    if let Some(root) = manifest_root(&cwd) {
        let manifest = root.join(MANIFEST);
        ensure!(!write, "{} already exists", manifest.display());
        print!("{}", Plan::load(&manifest)?.to_toml()?);
        return Ok(());
    }

    let inferred = infer::directory(&cwd).inspect_err(|_| ui::hint(AI_HINT))?;
    for warning in &inferred.warnings {
        ui::note(warning);
    }
    plan::resolve(&inferred.plan, &cwd)?;
    let text = inferred_manifest_text(&inferred)?;

    if write {
        fs::write(cwd.join(MANIFEST), text)?;
        ui::status("Created", MANIFEST);
    } else {
        print!("{text}");
    }
    Ok(())
}

/// cbuild.toml contents for a plan inferred by the heuristics, with their reasons.
fn inferred_manifest_text(inferred: &infer::Inferred) -> Result<String> {
    let mut text = String::from("# Plan inferred by cbuild from the sources:\n");
    for note in &inferred.notes {
        text.push_str(&format!("#   {note}\n"));
    }
    text.push('\n');
    text.push_str(&inferred.plan.to_toml()?);
    Ok(text)
}

fn plan_with_ai(yes: bool) -> Result<()> {
    let root = env::current_dir()?;
    ensure!(
        !root.join(MANIFEST).exists(),
        "{MANIFEST} already exists: delete it to design a new plan"
    );
    design_with_ai(&root, yes, "Continue?")
}

/// Runs a planning session and returns the verified plan and who made it, after the
/// user's consent.
fn ask_ai(
    root: &Path,
    yes: bool,
    question: &str,
    goal: ai::Goal,
) -> Result<(Finished, ai::Provider)> {
    let provider = ai::provider().with_context(ai::install_hint)?;
    let toolchain = Toolchain::detect()?;
    ui::note(format!(
        "the AI build engineer runs through {} on your account. The model reads the project's \
         sources and build files through cbuild's tools; hidden files and files that look like \
         secrets are never shared.",
        provider.name()
    ));
    if !yes {
        ensure!(
            confirm(question)?,
            "cancelled: write a {MANIFEST}, or run `cbuild plan --ai` or `cbuild fix` later"
        );
    }

    let started = Instant::now();
    let task = ai::task(root, &toolchain, goal)?;
    let outcome = provider.run(root, &task)?;
    let Some(finished) = outcome.finished else {
        bail!(
            "the AI build engineer could not produce a verified plan:\n{}",
            outcome.summary.trim()
        );
    };
    plan::resolve(&finished.plan, root)?;
    ui::status(
        "Verified",
        format!(
            "new plan built successfully, in {:.0}s",
            started.elapsed().as_secs_f64()
        ),
    );
    for line in finished
        .notes
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        ui::note(line.trim());
    }
    Ok((finished, provider))
}

/// Has the AI build engineer design and verify a plan, saved as cbuild.toml in `root`.
fn design_with_ai(root: &Path, yes: bool, question: &str) -> Result<()> {
    let root = root.canonicalize()?;
    let (finished, provider) = ask_ai(&root, yes, question, ai::Goal::Design)?;
    fs::write(
        root.join(MANIFEST),
        ai::manifest_text(&finished, provider.name(), "designed")?,
    )?;
    ui::status("Created", MANIFEST);
    Ok(())
}

/// Has the AI build engineer repair `current`, which fails as `failure` describes,
/// and saves the result as cbuild.toml, keeping a copy of the previous one.
fn fix_with_ai(
    root: &Path,
    current: &Plan,
    failure: &serde_json::Value,
    yes: bool,
    question: &str,
) -> Result<()> {
    let root = root.canonicalize()?;
    let old = current.to_toml()?;
    let (finished, provider) = ask_ai(
        &root,
        yes,
        question,
        ai::Goal::Fix {
            plan: &old,
            failure,
        },
    )?;

    ui::status("Changes", "to the plan:");
    ui::diff(&old, &finished.plan.to_toml()?);
    save_updated_plan(&root, &finished, provider)
}

/// Writes an AI-updated plan as cbuild.toml, keeping a copy of the previous one.
fn save_updated_plan(root: &Path, finished: &Finished, provider: ai::Provider) -> Result<()> {
    let manifest = root.join(MANIFEST);
    if manifest.exists() {
        let backup = root.join("target/cbuild-agent/previous-cbuild.toml");
        fs::copy(&manifest, &backup)?;
        ui::note(format!(
            "previous plan saved in {}",
            backup.strip_prefix(root).unwrap_or(&backup).display()
        ));
    }
    fs::write(
        &manifest,
        ai::manifest_text(finished, provider.name(), "updated")?,
    )?;
    ui::status("Updated", MANIFEST);
    Ok(())
}

fn sync(yes: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let root = manifest_root(&cwd)
        .with_context(|| format!("no {MANIFEST} here: run `cbuild init` first"))?
        .canonicalize()?;
    let plan = Plan::load(&root.join(MANIFEST))?;
    let toolchain = Toolchain::detect()?;
    ui::status("Checking", "the current plan");
    let report = ai::tools::build_report(&plan, &root, &toolchain);
    if report["status"] != "success" {
        ui::note(format!(
            "the current plan does not build, so it is repaired first: {}",
            report["error"].as_str().unwrap_or_default()
        ));
        return fix_with_ai(
            &root,
            &plan,
            &report,
            yes,
            "Let the AI build engineer fix the plan?",
        );
    }

    let old = plan.to_toml()?;
    let question = "Let the AI build engineer compare the plan with the project?";
    let (finished, provider) = ask_ai(&root, yes, question, ai::Goal::Sync { plan: &old })?;
    let new = finished.plan.to_toml()?;
    if new == old {
        ui::status("Finished", "the plan is up to date");
        return Ok(());
    }
    ui::status("Changes", "proposed for the plan:");
    ui::diff(&old, &new);
    if !yes {
        ensure!(
            confirm("Apply these changes?")?,
            "the plan was left unchanged"
        );
    }
    save_updated_plan(&root, &finished, provider)
}

fn init(path: Option<&Path>, library: bool, git: bool, yes: bool, no_ai: bool) -> Result<()> {
    let path = path.unwrap_or(Path::new("."));
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    let root = path.canonicalize()?;
    ensure!(
        !root.join(MANIFEST).exists(),
        "{} already exists: this project is already set up for cbuild",
        root.join(MANIFEST).display()
    );

    if infer::has_sources(&root)? {
        if library {
            ui::note("--lib is ignored: the plan comes from the existing sources");
        }
        init_existing(&root, yes, no_ai)?;
        if scaffold::ensure_gitignore(&root)? {
            ui::status("Updated", ".gitignore");
        }
    } else {
        let name = infer::target_name(&root);
        let files = scaffold::write_template(&root, &name, library)?;
        let kind = if library { "library" } else { "program" };
        ui::status("Created", format!("{kind} `{name}`"));
        for file in &files {
            eprintln!("             {}", file.display());
        }
    }
    if git && scaffold::init_git(&root) {
        ui::status("Started", "git repository");
    }
    ui::note("next: cbuild run");
    Ok(())
}

/// Writes a cbuild.toml for existing code: the heuristic plan if it really builds,
/// otherwise one designed or repaired by the AI build engineer (after consent).
fn init_existing(root: &Path, yes: bool, no_ai: bool) -> Result<()> {
    let ai_allowed = !no_ai && ai::provider().is_some();
    let inferred = match infer::directory(root) {
        Ok(inferred) => inferred,
        Err(err) if ai_allowed => {
            ui::note(format!(
                "cbuild's heuristics cannot plan this project: {err:#}"
            ));
            return design_with_ai(root, yes, "Let the AI build engineer design the plan?");
        }
        Err(err) => {
            if !no_ai {
                ui::hint(ai::install_hint());
            }
            return Err(err);
        }
    };
    for warning in &inferred.warnings {
        ui::note(warning);
    }

    let toolchain = Toolchain::detect()?;
    ui::status("Checking", "the plan inferred from the sources");
    let report = ai::tools::build_report(&inferred.plan, root, &toolchain);
    if report["status"] == "success" {
        fs::write(root.join(MANIFEST), inferred_manifest_text(&inferred)?)?;
        ui::status("Created", format!("{MANIFEST}, verified by a real build"));
        return Ok(());
    }
    let error = report["error"].as_str().unwrap_or_default();
    ui::note(format!(
        "the plan inferred from the sources does not build: {error}"
    ));
    if !ai_allowed {
        if !no_ai {
            ui::hint(ai::install_hint());
        }
        bail!("no {MANIFEST} written: a plan that does not build would not help");
    }
    fix_with_ai(
        root,
        &inferred.plan,
        &report,
        yes,
        "Let the AI build engineer fix the plan?",
    )
}

fn fix(yes: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let root = manifest_root(&cwd).unwrap_or(&cwd).canonicalize()?;
    let manifest = root.join(MANIFEST);
    let plan = if manifest.exists() {
        Plan::load(&manifest)?
    } else {
        match infer::directory(&root) {
            Ok(inferred) => inferred.plan,
            Err(err) => {
                ui::note(format!(
                    "cbuild's heuristics cannot plan this project: {err:#}"
                ));
                return design_with_ai(
                    &root,
                    yes,
                    "No build plan yet. Let the AI build engineer design it?",
                );
            }
        }
    };

    let toolchain = Toolchain::detect()?;
    ui::status("Checking", "the current plan");
    let report = ai::tools::build_report(&plan, &root, &toolchain);
    if report["status"] == "success" {
        ui::status("Finished", "the plan builds: nothing to fix");
        return Ok(());
    }
    ui::note(format!(
        "the current plan does not build: {}",
        report["error"].as_str().unwrap_or_default()
    ));
    fix_with_ai(
        &root,
        &plan,
        &report,
        yes,
        "Let the AI build engineer fix the plan?",
    )
}

fn confirm(question: &str) -> Result<bool> {
    ensure!(
        io::stdin().is_terminal(),
        "{question} Confirmation needed: run again with --yes to let the AI build engineer read the project"
    );
    eprint!("{question} [Y/n] ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_lowercase();
    Ok(answer.is_empty() || answer.starts_with('y') || answer.starts_with('s'))
}

fn serve_mcp(root: Option<PathBuf>, session: Option<PathBuf>) -> Result<()> {
    let root = match root {
        Some(root) => root,
        None => env::current_dir()?,
    };
    ai::mcp::serve(&root, session.as_deref())
}

fn new(path: &Path, library: bool, git: bool) -> Result<()> {
    let created = scaffold::new(path, &scaffold::Options { library, git })?;
    let kind = if library { "library" } else { "program" };
    ui::status(
        "Created",
        format!("{kind} `{}` in {}", created.name, path.display()),
    );
    for file in &created.files {
        eprintln!("             {}", file.display());
    }
    if created.git {
        eprintln!("             (git repository)");
    }
    let run = if library {
        "cbuild run   # builds the library and runs the demo"
    } else {
        "cbuild run"
    };
    ui::note(format!("next: cd {} && {run}", path.display()));
    Ok(())
}

fn export_plan(format: export::Format, output: Option<&Path>, force: bool) -> Result<()> {
    let cwd = env::current_dir()?.canonicalize()?;
    let (root, plan, origin) = match manifest_root(&cwd) {
        Some(root) => (
            root.to_path_buf(),
            Plan::load(&root.join(MANIFEST))?,
            MANIFEST.to_string(),
        ),
        None => {
            let inferred = infer::directory(&cwd).inspect_err(|_| ui::hint(AI_HINT))?;
            let origin = "the plan inferred from the sources".to_string();
            (cwd.clone(), inferred.plan, origin)
        }
    };
    let project = plan::resolve(&plan, &root)?;

    let file = match output {
        Some(output) => cwd.join(output),
        None => root.join(format.default_file()),
    };
    let file_name = file
        .file_name()
        .context("the output path has no file name")?
        .to_owned();
    let dir = file.parent().context("the output path has no folder")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let dir = dir.canonicalize()?;
    let inside = dir.strip_prefix(&root).map_err(|_| {
        anyhow!(
            "the exported file must be inside the project ({})",
            root.display()
        )
    })?;
    // From the file's folder back to the project root.
    let back = match inside.components().count() {
        0 => ".".to_string(),
        depth => vec![".."; depth].join("/"),
    };
    let file = dir.join(file_name);
    let shown = file
        .strip_prefix(&cwd)
        .unwrap_or(&file)
        .display()
        .to_string();

    if let Ok(existing) = fs::read_to_string(&file) {
        let suggestion = match format {
            export::Format::Cmake => "cbuild-export/CMakeLists.txt",
            export::Format::Make => "cbuild.mk",
        };
        ensure!(
            force || existing.starts_with(export::HEADER),
            "{shown} exists and was not generated by cbuild: write elsewhere with --output \
             (for example --output {suggestion}), or replace it with --force"
        );
    }
    let text = export::generate(format, &project, &infer::target_name(&root), &origin, &back)?;
    fs::write(&file, text).with_context(|| format!("cannot write {shown}"))?;
    ui::status("Exported", &shown);

    let next = match format {
        export::Format::Cmake => {
            let source = dir
                .strip_prefix(&cwd)
                .map_or(dir.display().to_string(), |d| {
                    if d.as_os_str().is_empty() {
                        ".".to_string()
                    } else {
                        d.display().to_string()
                    }
                });
            format!("cmake -S {source} -B build && cmake --build build")
        }
        export::Format::Make if shown == "Makefile" => "make".to_string(),
        export::Format::Make => format!("make -f {shown}"),
    };
    ui::note(format!("build it with: {next}"));
    Ok(())
}

fn clean() -> Result<()> {
    let cwd = env::current_dir()?;
    let target = manifest_root(&cwd).unwrap_or(&cwd).join("target");
    if !target.exists() {
        return Ok(());
    }
    ensure!(
        engine::owns_target_dir(&target),
        "{} was not created by cbuild, refusing to delete it",
        target.display()
    );
    fs::remove_dir_all(&target).with_context(|| format!("cannot remove {}", target.display()))?;
    ui::status("Removed", target.display());
    Ok(())
}
