//! Deterministic native build: compiles, archives and links a resolved project.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::plan::{Dependency, Project, TargetKind};
use crate::toolchain::{CompileUnit, Profile, Toolchain, find_program};
use crate::ui;

pub const COMPILE_COMMANDS: &str = "compile_commands.json";

/// Marker inside `target/`, so cbuild never writes into or deletes a directory it did not create.
const TARGET_MARKER: &str = "CACHEDIR.TAG";
const TARGET_OWNER: &str = "created by cbuild";

pub struct BuildOptions {
    pub profile: Profile,
    pub jobs: usize,
    pub out_dir: PathBuf,
    /// Ask the compiler for colored diagnostics.
    pub color: bool,
}

pub struct Executable {
    pub name: String,
    pub path: PathBuf,
}

/// Where build progress and compiler output go.
pub trait Reporter {
    fn status(&mut self, label: &str, message: &str);
    fn output(&mut self, bytes: &[u8]);
}

/// Progress and compiler output on the terminal.
pub struct Terminal;

impl Reporter for Terminal {
    fn status(&mut self, label: &str, message: &str) {
        ui::status(label, message);
    }

    fn output(&mut self, bytes: &[u8]) {
        let _ = io::stderr().write_all(bytes);
    }
}

/// Compiler output collected in memory, progress dropped.
#[derive(Default)]
pub struct Capture {
    pub output: String,
}

impl Reporter for Capture {
    fn status(&mut self, _label: &str, _message: &str) {}

    fn output(&mut self, bytes: &[u8]) {
        self.output.push_str(&String::from_utf8_lossy(bytes));
    }
}

/// A compile, archive or link step that failed, with the tool's output.
#[derive(Debug)]
pub struct BuildFailure {
    pub message: String,
    pub output: String,
}

impl fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BuildFailure {}

struct Job {
    target: usize,
    /// (object file, compile command) per source.
    units: Vec<(PathBuf, Vec<String>)>,
    libraries: Vec<PathBuf>,
    ldflags: Vec<String>,
}

pub fn build(
    project: &Project,
    tc: &Toolchain,
    opts: &BuildOptions,
    reporter: &mut dyn Reporter,
) -> Result<Vec<Executable>> {
    let mut system = SystemDeps::default();
    let mut database = Vec::new();
    let mut jobs = Vec::new();

    for &i in &project.order {
        let target = &project.targets[i];
        let closure = project.link_closure(i);

        let includes = project.includes(i);
        let (cflags, ldflags) = system.flags(project, i)?;

        let obj_dir = opts.out_dir.join("obj").join(&target.name);
        let mut units = Vec::new();
        for source in &target.sources {
            let object = obj_dir.join(source).with_extension("o");
            let command = tc.compile_command(&CompileUnit {
                std: &project.std,
                profile: opts.profile,
                source,
                object: &object,
                depfile: &object.with_extension("d"),
                includes: &includes,
                defines: &target.defines,
                cflags: &cflags,
            });
            database.push(json!({
                "directory": project.root,
                "arguments": command,
                "file": source,
                "output": object,
            }));
            units.push((object, command));
        }

        let libraries = closure
            .iter()
            .map(|&j| library_path(opts, &project.targets[j].name))
            .collect();
        jobs.push(Job {
            target: i,
            units,
            libraries,
            ldflags,
        });
    }

    // Written before compiling, so editors work even while the build is broken.
    fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("cannot create {}", opts.out_dir.display()))?;
    fs::write(
        opts.out_dir.join(COMPILE_COMMANDS),
        serde_json::to_string_pretty(&database)?,
    )?;

    let mut executables = Vec::new();
    for job in &jobs {
        let target = &project.targets[job.target];
        reporter.status(
            "Compiling",
            &format!(
                "{} ({}, {}, {})",
                target.name,
                project.std,
                tc.describe(),
                opts.profile.name()
            ),
        );
        compile(project, tc, job, opts, reporter)?;

        let objects: Vec<PathBuf> = job.units.iter().map(|(object, _)| object.clone()).collect();
        match target.kind {
            TargetKind::StaticLibrary => {
                let output = library_path(opts, &target.name);
                // `ar` adds to an existing archive: start fresh so removed sources disappear.
                if output.exists() {
                    fs::remove_file(&output)?;
                }
                let command = tc.archive_command(&objects, &output)?;
                run_step(
                    &command,
                    &project.root,
                    reporter,
                    format!("could not archive `{}`", target.name),
                )?;
            }
            TargetKind::Executable => {
                let output = opts.out_dir.join(&target.name);
                let command = tc.link_command(&objects, &job.libraries, &job.ldflags, &output);
                run_step(
                    &command,
                    &project.root,
                    reporter,
                    format!("could not link `{}`", target.name),
                )?;
                executables.push(Executable {
                    name: target.name.clone(),
                    path: output,
                });
            }
        }
    }
    Ok(executables)
}

fn compile(
    project: &Project,
    tc: &Toolchain,
    job: &Job,
    opts: &BuildOptions,
    reporter: &mut dyn Reporter,
) -> Result<()> {
    let mut commands = Vec::new();
    for (object, command) in &job.units {
        if let Some(dir) = object.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut command = command.clone();
        if opts.color {
            command.insert(1, tc.color_flag().to_string());
        }
        commands.push(command);
    }

    let mut ok = true;
    let mut errors = String::new();
    for (command, output) in commands
        .iter()
        .zip(run_parallel(&commands, &project.root, opts.jobs))
    {
        let output = output.with_context(|| format!("cannot run the compiler `{}`", command[0]))?;
        reporter.output(&output.stderr);
        if !output.status.success() {
            ok = false;
            errors.push_str(&String::from_utf8_lossy(&output.stderr));
        }
    }
    if !ok {
        let message = format!("could not compile `{}`", project.targets[job.target].name);
        return Err(BuildFailure {
            message,
            output: errors,
        }
        .into());
    }
    Ok(())
}

fn run_step(
    command: &[String],
    dir: &Path,
    reporter: &mut dyn Reporter,
    failure: String,
) -> Result<()> {
    let output = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run `{}`", command[0]))?;
    reporter.output(&output.stderr);
    if !output.status.success() {
        let output = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(BuildFailure {
            message: failure,
            output,
        }
        .into());
    }
    Ok(())
}

fn run_parallel(commands: &[Vec<String>], dir: &Path, jobs: usize) -> Vec<io::Result<Output>> {
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<io::Result<Output>>>> =
        commands.iter().map(|_| Mutex::new(None)).collect();
    thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, commands.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(command) = commands.get(i) else {
                        break;
                    };
                    let output = Command::new(&command[0])
                        .args(&command[1..])
                        .current_dir(dir)
                        .output();
                    *results[i].lock().unwrap() = Some(output);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|slot| slot.into_inner().unwrap().expect("every command ran"))
        .collect()
}

fn library_path(opts: &BuildOptions, name: &str) -> PathBuf {
    opts.out_dir.join(format!("lib{name}.a"))
}

/// Compile and link flags of system dependencies, each looked up once per build.
#[derive(Default)]
struct SystemDeps {
    resolved: HashMap<String, (Vec<String>, Vec<String>)>,
}

impl SystemDeps {
    /// Flags for target `i`: its own system dependencies plus those of every library it links.
    fn flags(&mut self, project: &Project, i: usize) -> Result<(Vec<String>, Vec<String>)> {
        let (mut cflags, mut ldflags) = (Vec::new(), Vec::new());
        for name in project.system_deps(i) {
            if !self.resolved.contains_key(name) {
                let flags = resolve_system(name, &project.dependencies[name])?;
                self.resolved.insert(name.to_string(), flags);
            }
            let (c, l) = &self.resolved[name];
            cflags.extend(c.iter().cloned());
            ldflags.extend(l.iter().cloned());
        }
        Ok((cflags, ldflags))
    }
}

fn resolve_system(name: &str, dep: &Dependency) -> Result<(Vec<String>, Vec<String>)> {
    if let Some(lib) = &dep.link {
        return Ok((Vec::new(), vec![format!("-l{lib}")]));
    }
    let package = dep
        .pkg_config
        .as_deref()
        .expect("validated: link or pkg_config");
    let pkg_config = find_program("pkg-config".as_ref())
        .with_context(|| format!("dependency `{name}` needs pkg-config, which is not installed"))?;
    let query = |flag: &str| -> Result<Vec<String>> {
        let output = Command::new(&pkg_config).args([flag, package]).output()?;
        if !output.status.success() {
            bail!(
                "dependency `{name}`: pkg-config cannot find `{package}`. Is the library installed?\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(String::from)
            .collect())
    };
    Ok((query("--cflags")?, query("--libs")?))
}

/// True if `dir` holds cbuild's marker.
pub fn owns_target_dir(dir: &Path) -> bool {
    fs::read_to_string(dir.join(TARGET_MARKER)).is_ok_and(|text| text.contains(TARGET_OWNER))
}

/// Creates `dir` with cbuild's marker, refusing to reuse a non-empty directory someone else owns.
pub fn prepare_target_dir(dir: &Path) -> Result<()> {
    if owns_target_dir(dir) {
        return Ok(());
    }
    if dir
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_some())
    {
        bail!(
            "{} already exists and was not created by cbuild: move it away so build outputs do not mix with your files",
            dir.display()
        );
    }
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    fs::write(
        dir.join(TARGET_MARKER),
        format!(
            "Signature: 8a477f597d28d172789f06886806bc55\n# This directory was {TARGET_OWNER}.\n"
        ),
    )?;
    Ok(())
}

/// Copies the compilation database to the project root, where clangd and editors look for it.
pub fn publish_compile_commands(root: &Path, target_dir: &Path, out_dir: &Path) -> Result<()> {
    let dest = root.join(COMPILE_COMMANDS);
    if let Ok(existing) = fs::read_to_string(&dest) {
        // Never overwrite a database generated by another tool.
        let ours = serde_json::from_str::<Vec<Value>>(&existing).is_ok_and(|entries| {
            entries.iter().all(|entry| {
                entry["output"]
                    .as_str()
                    .is_some_and(|output| Path::new(output).starts_with(target_dir))
            })
        });
        if !ours {
            ui::note(format!(
                "kept the existing {COMPILE_COMMANDS}; cbuild's copy is in {}",
                out_dir.display()
            ));
            return Ok(());
        }
    }
    fs::copy(out_dir.join(COMPILE_COMMANDS), &dest)
        .with_context(|| format!("cannot write {}", dest.display()))?;
    Ok(())
}
