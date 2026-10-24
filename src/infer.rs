//! Heuristic planner: derives a plan from the sources when there is no cbuild.toml.
//! It only handles unambiguous cases; anything else is an error that says what to declare.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::plan::{self, Dependency, DependencySource, Plan, SCHEMA, Strategy, Target, TargetKind};
use crate::scan::{self, Main};

/// A heuristic plan and the reasons behind it.
pub struct Inferred {
    pub plan: Plan,
    /// Decisions taken, shown by `cbuild plan`.
    pub notes: Vec<String>,
    /// Things the user should know on every build.
    pub warnings: Vec<String>,
    /// The project has its own build files (CMake, Make, ...), so its code is known to
    /// compile: when the inferred plan fails, the plan is at fault.
    pub has_build_system: bool,
}

const EXTERNAL_BUILD_FILES: &[&str] = &["CMakeLists.txt", "meson.build", "configure", "Makefile"];

/// Folder names and file-name words marking tests, examples and the like: not part of the program.
const AUXILIARY_WORDS: &[&str] = &[
    "test",
    "tests",
    "unittest",
    "example",
    "examples",
    "bench",
    "benchmark",
    "benchmarks",
    "fuzz",
    "fuzzer",
    "fuzzing",
    "demo",
    "demos",
];

/// Plan for a single source file; returns the directory containing it as project root.
pub fn single_file(file: &Path) -> Result<(PathBuf, Inferred)> {
    ensure!(
        file.extension().is_some_and(|ext| ext == "c"),
        "`{}` is not a C source file (.c)",
        file.display()
    );
    let file = file
        .canonicalize()
        .with_context(|| format!("cannot find `{}`", file.display()))?;
    let root = file
        .parent()
        .context("the file has no parent directory")?
        .to_path_buf();
    let file_name = file
        .file_name()
        .and_then(|name| name.to_str())
        .context("the file name is not valid UTF-8")?;
    let source = fs::read(&file).with_context(|| format!("cannot read {file_name}"))?;
    match scan::main_definition(&source) {
        Main::Always => {}
        Main::Optional => bail!(
            "`{file_name}` defines main() only inside an #if/#ifdef block, so it is not compiled by default"
        ),
        Main::None => bail!("`{file_name}` has no main() function, so it cannot become a program"),
    }

    let name = target_name(&file);
    let target = target(
        name.clone(),
        TargetKind::Executable,
        file_name.to_string(),
        Vec::new(),
    );
    let inferred = Inferred {
        notes: vec![format!("executable `{name}` from {file_name}")],
        warnings: Vec::new(),
        has_build_system: false,
        plan: plan_with(target),
    };
    Ok((root, inferred))
}

/// Plan for a project directory, made of `src/**/*.c` (or `*.c` without `src/`) minus
/// tests and examples: one program if exactly one file defines main(), a static library if none does.
pub fn directory(root: &Path) -> Result<Inferred> {
    let warnings: Vec<String> = EXTERNAL_BUILD_FILES
        .iter()
        .filter(|file| root.join(file).exists())
        .map(|file| format!("found {file}, but external build systems are not supported yet: building the C sources directly"))
        .collect();

    let pattern = source_pattern(root);
    let (auxiliary, files): (Vec<PathBuf>, Vec<PathBuf>) = plan::glob_files(root, pattern)?
        .into_iter()
        .partition(|file| is_auxiliary(file));
    ensure!(
        !files.is_empty(),
        "no C source files found ({pattern}). To build a single file run `cbuild run <file.c>`"
    );

    let mut mains = Vec::new();
    let mut optional_mains = Vec::new();
    for file in &files {
        let source =
            fs::read(root.join(file)).with_context(|| format!("cannot read {}", file.display()))?;
        match scan::main_definition(&source) {
            Main::Always => mains.push(file.display().to_string()),
            Main::Optional => optional_mains.push(file.display().to_string()),
            Main::None => {}
        }
    }

    let name = target_name(root);
    let mut notes = Vec::new();
    let kind = match mains.as_slice() {
        [] => {
            notes.push(format!("static library `{name}`: no file defines main()"));
            TargetKind::StaticLibrary
        }
        [main] => {
            notes.push(format!("executable `{name}` <- main() in {main}"));
            TargetKind::Executable
        }
        _ => bail!(
            "{} files define main(): {}.\nRun one of them with `cbuild run <file.c>`, or declare one \
             target per program in {}",
            mains.len(),
            mains.join(", "),
            plan::MANIFEST
        ),
    };
    notes.push(format!("sources: {pattern} ({} files)", files.len()));
    if !auxiliary.is_empty() {
        let names: Vec<String> = auxiliary.iter().map(|f| f.display().to_string()).collect();
        notes.push(format!(
            "left out as tests or examples: {}",
            names.join(", ")
        ));
    }
    if !optional_mains.is_empty() {
        notes.push(format!(
            "ignored main() only compiled under a build option in: {}",
            optional_mains.join(", ")
        ));
    }

    let mut includes: Vec<String> = ["src", "include"]
        .into_iter()
        .filter(|dir| root.join(dir).is_dir())
        .map(String::from)
        .collect();
    if kind == TargetKind::StaticLibrary && includes.is_empty() {
        includes.push(".".to_string());
    }
    if !includes.is_empty() {
        notes.push(format!("include directories: {}", includes.join(", ")));
    }

    let mut target = target(name, kind, pattern.to_string(), includes);
    target.exclude = auxiliary.iter().map(|f| f.display().to_string()).collect();
    if kind == TargetKind::Executable {
        notes.push("libm linked for the math functions".to_string());
    } else {
        // Users of a library need its headers: they become public.
        target.include_public = std::mem::take(&mut target.include_private);
    }
    Ok(Inferred {
        plan: plan_with(target),
        notes,
        has_build_system: !warnings.is_empty(),
        warnings,
    })
}

/// Where the heuristics look for sources: `src/` if it exists, else the project folder.
fn source_pattern(root: &Path) -> &'static str {
    if root.join("src").is_dir() {
        "src/**/*.c"
    } else {
        "*.c"
    }
}

/// True if `root` holds C sources anywhere (hidden folders aside).
pub fn has_sources(root: &Path) -> Result<bool> {
    Ok(!plan::glob_files(root, "**/*.c")?.is_empty())
}

/// True for files that look like tests, examples, benchmarks or fuzzers: in such a folder,
/// or with such a word in the name (`test.c`, `linenoise-test.c`, `fuzz_parse.c`).
fn is_auxiliary(path: &Path) -> bool {
    let is_word = |word: &str| AUXILIARY_WORDS.contains(&word.to_lowercase().as_str());
    let in_auxiliary_dir = path.parent().is_some_and(|dir| {
        dir.components()
            .any(|c| is_word(&c.as_os_str().to_string_lossy()))
    });
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    in_auxiliary_dir
        || stem
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(is_word)
}

fn target(name: String, kind: TargetKind, sources: String, include_private: Vec<String>) -> Target {
    // Executables link libm by default: math functions are common and `-lm` is harmless.
    let deps = if kind == TargetKind::Executable {
        vec!["m".to_string()]
    } else {
        Vec::new()
    };
    Target {
        name,
        kind,
        sources: vec![sources],
        exclude: Vec::new(),
        include_private,
        include_public: Vec::new(),
        defines: BTreeMap::new(),
        deps,
    }
}

fn plan_with(target: Target) -> Plan {
    let mut dependencies = BTreeMap::new();
    if target.deps.iter().any(|dep| dep == "m") {
        let libm = Dependency {
            source: DependencySource::System,
            link: Some("m".into()),
            pkg_config: None,
        };
        dependencies.insert("m".to_string(), libm);
    }
    Plan {
        schema: SCHEMA.to_string(),
        strategy: Strategy::Native,
        std: "gnu17".to_string(),
        targets: vec![target],
        dependencies,
    }
}

/// A valid target name from a file or directory name.
pub fn target_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.trim_matches('_').is_empty() {
        "app".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::is_auxiliary;
    use std::path::Path;

    #[test]
    fn recognizes_tests_and_examples() {
        for path in [
            "test.c",
            "tests.c",
            "linenoise-test.c",
            "example.c",
            "src/tests/a.c",
            "fuzz_parse.c",
            "bench.c",
        ] {
            assert!(is_auxiliary(Path::new(path)), "{path}");
        }
        for path in [
            "main.c",
            "src/parser.c",
            "contest.c",
            "latest.c",
            "testing.c",
        ] {
            assert!(!is_auxiliary(Path::new(path)), "{path}");
        }
    }
}
