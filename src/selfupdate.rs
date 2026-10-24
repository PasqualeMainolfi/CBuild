//! `cbuild update`: installs a newer cbuild through `cargo install`, from a git
//! repository with version tags (`v1.2.0`) or from a local checkout.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::toolchain::find_program;
use crate::ui;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const PACKAGE: &str = env!("CARGO_PKG_NAME");

type Version = (u64, u64, u64);

enum Release {
    Git { url: String, tag: String },
    Local { path: PathBuf },
}

struct Available {
    version: Version,
    label: String,
    release: Release,
}

/// Where updates come from: `--from`, then `CBUILD_UPDATE_SOURCE`, then the
/// `repository` field of cbuild's Cargo.toml.
fn source(from: Option<String>) -> Result<String> {
    from.or_else(|| env::var("CBUILD_UPDATE_SOURCE").ok())
        .or_else(|| Some(env!("CARGO_PKG_REPOSITORY").to_string()))
        .filter(|source| !source.is_empty())
        .context(
            "no update source known: pass --from <git URL or local checkout>, set \
             CBUILD_UPDATE_SOURCE, or set `repository` in cbuild's Cargo.toml",
        )
}

/// `1.2.3` or `v1.2.3`; pre-release and build suffixes are ignored.
fn parse_version(text: &str) -> Option<Version> {
    let core = text
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.').map(|part| part.parse().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn latest(source: &str) -> Result<Available> {
    let path = Path::new(source);
    if path.is_dir() {
        latest_local(path)
    } else {
        latest_git(source)
    }
}

fn latest_local(path: &Path) -> Result<Available> {
    let manifest = path.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)
        .with_context(|| format!("{} is not a cbuild checkout: no Cargo.toml", path.display()))?;
    let manifest: toml::Table =
        toml::from_str(&text).with_context(|| format!("invalid {}", manifest.display()))?;
    let package = manifest.get("package").and_then(|p| p.as_table());
    let field = |key: &str| {
        package
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    };
    ensure!(
        field("name") == PACKAGE,
        "{} is not a cbuild checkout",
        path.display()
    );
    let label = field("version").to_string();
    let version = parse_version(&label)
        .with_context(|| format!("invalid version `{label}` in {}", path.display()))?;
    Ok(Available {
        version,
        label,
        release: Release::Local {
            path: path.canonicalize()?,
        },
    })
}

fn latest_git(url: &str) -> Result<Available> {
    let git = find_program("git".as_ref()).context("git is needed to look for updates")?;
    let output = Command::new(git)
        .args(["ls-remote", "--tags", "--refs", url])
        .output()?;
    ensure!(
        output.status.success(),
        "cannot read {url}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let (version, tag) = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once("refs/tags/")
                .map(|(_, tag)| tag.trim().to_string())
        })
        .filter_map(|tag| parse_version(&tag).map(|version| (version, tag)))
        .max()
        .with_context(|| format!("no release tags (like v1.2.0) in {url}"))?;
    Ok(Available {
        version,
        label: tag.trim_start_matches('v').to_string(),
        release: Release::Git {
            url: url.to_string(),
            tag,
        },
    })
}

pub fn update(from: Option<String>, check: bool) -> Result<()> {
    let source = source(from)?;
    let current = parse_version(CURRENT).expect("cbuild's own version is valid");
    ui::status("Checking", format!("for updates in {source}"));
    let available = latest(&source)?;
    if available.version <= current {
        ui::status("Finished", format!("cbuild {CURRENT} is up to date"));
        return Ok(());
    }
    if check {
        ui::status(
            "Available",
            format!(
                "cbuild {} (you have {CURRENT}): run `cbuild update` to install it",
                available.label
            ),
        );
        return Ok(());
    }

    let cargo = find_program("cargo".as_ref())
        .context("cargo is needed to install updates: install Rust from https://rustup.rs")?;
    let mut command = Command::new(cargo);
    command.args(["install", "--locked", "--force"]);
    match &available.release {
        Release::Git { url, tag } => command.args(["--git", url, "--tag", tag, PACKAGE]),
        Release::Local { path } => command.arg("--path").arg(path),
    };
    ui::status(
        "Installing",
        format!("cbuild {} (you have {CURRENT})", available.label),
    );
    let status = command.status().context("cannot run cargo")?;
    ensure!(
        status.success(),
        "cargo install failed: cbuild {CURRENT} is still the installed version"
    );
    ui::status(
        "Updated",
        format!("cbuild {CURRENT} -> {}", available.label),
    );

    // cargo installs into its own bin folder: say so if the cbuild that ran lives elsewhere.
    if let (Ok(exe), Some(bin)) = (env::current_exe(), cargo_bin_dir())
        && !exe.starts_with(&bin)
    {
        ui::note(format!(
            "the new cbuild is in {}; the one you ran is {}",
            bin.display(),
            exe.display()
        ));
    }
    Ok(())
}

fn cargo_bin_dir() -> Option<PathBuf> {
    env::var_os("CARGO_INSTALL_ROOT")
        .or_else(|| env::var_os("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .map(|root| root.join("bin"))
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_release_versions() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("v2.0.0-rc.1"), Some((2, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("v1.2"), None);
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
    }
}
