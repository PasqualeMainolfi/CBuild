//! Compiler detection and translation of build intentions into compiler flags.
//! Everything GCC/Clang-specific lives here, so another backend (MSVC) only touches this file.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Dev,
    Release,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Dev => "dev",
            Profile::Release => "release",
        }
    }

    /// Output directory under `target/`.
    pub fn dir(self) -> &'static str {
        match self {
            Profile::Dev => "debug",
            Profile::Release => "release",
        }
    }

    fn flags(self) -> &'static [&'static str] {
        match self {
            Profile::Dev => &["-O0", "-g"],
            Profile::Release => &["-O2", "-DNDEBUG"],
        }
    }
}

/// One translation unit to compile.
pub struct CompileUnit<'a> {
    pub std: &'a str,
    pub profile: Profile,
    pub source: &'a Path,
    pub object: &'a Path,
    pub depfile: &'a Path,
    pub includes: &'a [PathBuf],
    pub defines: &'a BTreeMap<String, String>,
    pub cflags: &'a [String],
}

#[derive(Debug)]
pub struct Toolchain {
    cc: PathBuf,
    name: String,
}

const NO_COMPILER: &str = "no C compiler found (looked for $CC, cc, clang and gcc).\n\
    Install one: on macOS run `xcode-select --install`, on Debian/Ubuntu \
    `sudo apt install build-essential`, on Fedora `sudo dnf install gcc`";

impl Toolchain {
    pub fn detect() -> Result<Toolchain> {
        if let Some(cc) = env::var_os("CC").filter(|cc| !cc.is_empty()) {
            let path = find_program(&cc).with_context(|| {
                format!(
                    "CC is set to `{}`, but no such program was found",
                    cc.to_string_lossy()
                )
            })?;
            return Self::identify(path);
        }
        for name in ["cc", "clang", "gcc"] {
            if let Some(path) = find_program(OsStr::new(name)) {
                return Self::identify(path);
            }
        }
        bail!(NO_COMPILER)
    }

    fn identify(cc: PathBuf) -> Result<Toolchain> {
        let output = Command::new(&cc)
            .arg("--version")
            .output()
            .with_context(|| format!("cannot run `{}`", cc.display()))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text.lines().next().unwrap_or_default().trim();
        let name = if let Some((vendor, rest)) = first.split_once("clang version ") {
            // "Apple clang version 21.0.0 (clang-2100.3.34.2)" -> "Apple clang 21.0.0"
            let version = rest.split_whitespace().next().unwrap_or_default();
            format!("{vendor}clang {version}")
        } else if text.contains("Free Software Foundation") {
            // "gcc (Ubuntu 13.2.0-23ubuntu4) 13.2.0" -> "gcc 13.2.0"
            format!(
                "gcc {}",
                first.split_whitespace().last().unwrap_or_default()
            )
        } else {
            bail!(
                "`{}` is not a supported compiler (GCC or Clang required): {first}",
                cc.display()
            );
        };
        Ok(Toolchain { cc, name })
    }

    /// Human-readable compiler name, e.g. "Apple clang 21.0.0".
    pub fn describe(&self) -> &str {
        &self.name
    }

    pub fn compile_command(&self, unit: &CompileUnit) -> Vec<String> {
        let mut args = vec![path_str(&self.cc), format!("-std={}", unit.std)];
        args.extend(unit.profile.flags().iter().map(|flag| flag.to_string()));
        args.extend([
            "-Wall".into(),
            // Invalid since C99: Clang rejects it, GCC before 14 only warns. Same result everywhere.
            "-Werror=implicit-function-declaration".into(),
            "-MMD".into(),
            "-MF".into(),
            path_str(unit.depfile),
        ]);
        args.extend(
            unit.includes
                .iter()
                .map(|dir| format!("-I{}", dir.display())),
        );
        args.extend(unit.defines.iter().map(|(name, value)| {
            if value.is_empty() {
                format!("-D{name}")
            } else {
                format!("-D{name}={value}")
            }
        }));
        args.extend(unit.cflags.iter().cloned());
        args.extend([
            "-c".into(),
            path_str(unit.source),
            "-o".into(),
            path_str(unit.object),
        ]);
        args
    }

    pub fn link_command(
        &self,
        objects: &[PathBuf],
        libraries: &[PathBuf],
        ldflags: &[String],
        output: &Path,
    ) -> Vec<String> {
        let mut args = vec![path_str(&self.cc)];
        args.extend(objects.iter().chain(libraries).map(|path| path_str(path)));
        args.extend(ldflags.iter().cloned());
        args.extend(["-o".into(), path_str(output)]);
        args
    }

    pub fn archive_command(&self, objects: &[PathBuf], output: &Path) -> Result<Vec<String>> {
        let ar = env::var_os("AR")
            .filter(|ar| !ar.is_empty())
            .unwrap_or_else(|| "ar".into());
        let ar =
            find_program(&ar).context("`ar` not found: it is needed to create static libraries")?;
        let mut args = vec![path_str(&ar), "rcs".into(), path_str(output)];
        args.extend(objects.iter().map(|path| path_str(path)));
        Ok(args)
    }

    /// Keeps colored diagnostics even though compiler output is captured.
    pub fn color_flag(&self) -> &'static str {
        "-fdiagnostics-color=always"
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn find_program(name: &OsStr) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_path_buf());
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
