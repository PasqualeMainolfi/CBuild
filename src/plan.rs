//! Build plan IR: the contract between whoever designs the build (heuristics today,
//! the AI build engineer later) and the deterministic engine that executes it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "cbuild-plan/1";
pub const MANIFEST: &str = "cbuild.toml";

const STANDARDS: &[&str] = &[
    "c89", "c99", "c11", "c17", "c23", "gnu89", "gnu99", "gnu11", "gnu17", "gnu23",
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Always "cbuild-plan/1".
    #[serde(default = "default_schema")]
    pub schema: String,
    /// How the plan is executed; only "native" is available for now.
    #[serde(default)]
    pub strategy: Strategy,
    /// C standard: c89, c99, c11, c17, c23 or their gnu variants (default gnu17).
    #[serde(default = "default_std")]
    pub std: String,
    /// Programs and libraries to build.
    #[serde(default)]
    pub targets: Vec<Target>,
    /// System libraries the targets can list in their `deps`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    #[default]
    Native,
    Cmake,
    Meson,
    Autotools,
    Make,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Executable,
    StaticLibrary,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Letters, digits, `_` and `-`; executables get this file name.
    pub name: String,
    pub kind: TargetKind,
    /// Glob patterns of .c files relative to the project root, e.g. "src/**/*.c".
    pub sources: Vec<String>,
    /// Glob patterns removed from `sources`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Include directories used only to build this target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_private: Vec<String>,
    /// Include directories also propagated to targets that depend on this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_public: Vec<String>,
    /// Preprocessor macros: NAME -> value ("" for a bare `-DNAME`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defines: BTreeMap<String, String>,
    /// Names of other targets (libraries) or entries of `dependencies`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub source: DependencySource,
    /// Library linked as `-l<link>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Package queried with pkg-config for compile and link flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg_config: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependencySource {
    System,
}

fn default_schema() -> String {
    SCHEMA.to_string()
}

fn default_std() -> String {
    "gnu17".to_string()
}

impl Plan {
    pub fn load(path: &Path) -> Result<Plan> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

/// A validated plan with every glob expanded, ready for the engine.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub std: String,
    pub targets: Vec<ResolvedTarget>,
    pub dependencies: BTreeMap<String, Dependency>,
    /// Indices into `targets`, each target after the targets it depends on.
    pub order: Vec<usize>,
}

#[derive(Debug)]
pub struct ResolvedTarget {
    pub name: String,
    pub kind: TargetKind,
    /// Source files relative to the project root, sorted.
    pub sources: Vec<PathBuf>,
    pub include_private: Vec<PathBuf>,
    pub include_public: Vec<PathBuf>,
    pub defines: BTreeMap<String, String>,
    /// Indices into `Project::targets` of the libraries this target uses directly.
    pub target_deps: Vec<usize>,
    /// Names of entries in `Project::dependencies` this target uses directly.
    pub system_deps: Vec<String>,
}

impl Project {
    /// Libraries reachable from target `i`, each before the libraries it depends on
    /// (the order static linking needs).
    pub fn link_closure(&self, i: usize) -> Vec<usize> {
        fn walk(project: &Project, i: usize, seen: &mut [bool], post: &mut Vec<usize>) {
            for &j in &project.targets[i].target_deps {
                if !seen[j] {
                    seen[j] = true;
                    walk(project, j, seen, post);
                    post.push(j);
                }
            }
        }
        let mut seen = vec![false; self.targets.len()];
        let mut post = Vec::new();
        walk(self, i, &mut seen, &mut post);
        post.reverse();
        post
    }

    /// Include directories for compiling target `i`: its own, then the public ones of
    /// every library it links.
    pub fn includes(&self, i: usize) -> Vec<PathBuf> {
        let target = &self.targets[i];
        let own = target.include_private.iter().chain(&target.include_public);
        let closure = self.link_closure(i);
        let inherited = closure
            .iter()
            .flat_map(|&j| &self.targets[j].include_public);
        let mut includes = Vec::new();
        for dir in own.chain(inherited) {
            if !includes.contains(dir) {
                includes.push(dir.clone());
            }
        }
        includes
    }

    /// System dependencies of target `i` and of every library it links, without repeats.
    pub fn system_deps(&self, i: usize) -> Vec<&str> {
        let mut names = Vec::new();
        for t in std::iter::once(i).chain(self.link_closure(i)) {
            for name in &self.targets[t].system_deps {
                if !names.contains(&name.as_str()) {
                    names.push(name.as_str());
                }
            }
        }
        names
    }
}

/// Validates `plan` against the files under `root`. Every ambiguity is an error:
/// the engine never guesses.
pub fn resolve(plan: &Plan, root: &Path) -> Result<Project> {
    ensure!(
        plan.schema == SCHEMA,
        "unsupported plan schema `{}` (expected `{SCHEMA}`)",
        plan.schema
    );
    if plan.strategy != Strategy::Native {
        bail!(
            "strategy `{}` is not supported yet: this version only runs `native` builds",
            format!("{:?}", plan.strategy).to_lowercase()
        );
    }
    ensure!(
        STANDARDS.contains(&plan.std.as_str()),
        "unknown C standard `{}` (expected one of: {})",
        plan.std,
        STANDARDS.join(", ")
    );
    ensure!(!plan.targets.is_empty(), "the plan has no targets");

    for (name, dep) in &plan.dependencies {
        check_name(name, "dependency")?;
        ensure!(
            dep.link.is_some() != dep.pkg_config.is_some(),
            "dependency `{name}`: set exactly one of `link` or `pkg_config`"
        );
    }

    let mut index = HashMap::new();
    for (i, target) in plan.targets.iter().enumerate() {
        check_name(&target.name, "target")?;
        ensure!(
            !plan.dependencies.contains_key(&target.name),
            "`{}` is both a target and a dependency",
            target.name
        );
        ensure!(
            index.insert(target.name.as_str(), i).is_none(),
            "duplicate target `{}`",
            target.name
        );
    }

    let mut owners: HashMap<PathBuf, &str> = HashMap::new();
    let mut targets = Vec::new();
    for target in &plan.targets {
        let sources = expand_sources(root, target)?;
        for source in &sources {
            if let Some(other) = owners.insert(source.clone(), &target.name) {
                bail!(
                    "`{}` belongs to both `{other}` and `{}`: each source file must belong to exactly one target",
                    source.display(),
                    target.name
                );
            }
        }

        let mut target_deps = Vec::new();
        let mut system_deps = Vec::new();
        for dep in &target.deps {
            if let Some(&j) = index.get(dep.as_str()) {
                ensure!(
                    plan.targets[j].kind == TargetKind::StaticLibrary,
                    "target `{}` depends on `{dep}`, which is an executable: only libraries can be dependencies",
                    target.name
                );
                target_deps.push(j);
            } else if plan.dependencies.contains_key(dep) {
                system_deps.push(dep.clone());
            } else {
                bail!(
                    "target `{}` depends on `{dep}`, which is neither a target nor a declared dependency",
                    target.name
                );
            }
        }

        targets.push(ResolvedTarget {
            name: target.name.clone(),
            kind: target.kind,
            sources,
            include_private: include_dirs(root, &target.include_private, &target.name)?,
            include_public: include_dirs(root, &target.include_public, &target.name)?,
            defines: target.defines.clone(),
            target_deps,
            system_deps,
        });
    }

    let order = build_order(&targets)?;
    Ok(Project {
        root: root.to_path_buf(),
        std: plan.std.clone(),
        targets,
        dependencies: plan.dependencies.clone(),
        order,
    })
}

fn build_order(targets: &[ResolvedTarget]) -> Result<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Visiting,
        Done,
    }

    fn visit(
        i: usize,
        targets: &[ResolvedTarget],
        state: &mut [State],
        stack: &mut Vec<usize>,
        order: &mut Vec<usize>,
    ) -> Result<()> {
        match state[i] {
            State::Done => return Ok(()),
            State::Visiting => {
                let start = stack.iter().position(|&j| j == i).unwrap_or(0);
                let cycle: Vec<&str> = stack[start..]
                    .iter()
                    .chain([&i])
                    .map(|&j| targets[j].name.as_str())
                    .collect();
                bail!("dependency cycle between targets: {}", cycle.join(" -> "));
            }
            State::New => {}
        }
        state[i] = State::Visiting;
        stack.push(i);
        for &j in &targets[i].target_deps {
            visit(j, targets, state, stack, order)?;
        }
        stack.pop();
        state[i] = State::Done;
        order.push(i);
        Ok(())
    }

    let mut state = vec![State::New; targets.len()];
    let mut order = Vec::new();
    for i in 0..targets.len() {
        visit(i, targets, &mut state, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
}

fn expand_sources(root: &Path, target: &Target) -> Result<Vec<PathBuf>> {
    let name = &target.name;
    ensure!(!target.sources.is_empty(), "target `{name}` has no sources");

    let mut excluded = BTreeSet::new();
    for pattern in &target.exclude {
        excluded.extend(glob_files(root, pattern).with_context(|| format!("target `{name}`"))?);
    }
    let mut files = BTreeSet::new();
    for pattern in &target.sources {
        let matched = glob_files(root, pattern).with_context(|| format!("target `{name}`"))?;
        ensure!(
            !matched.is_empty(),
            "target `{name}`: `{pattern}` matches no files"
        );
        files.extend(matched);
    }

    let files: Vec<PathBuf> = files.difference(&excluded).cloned().collect();
    ensure!(
        !files.is_empty(),
        "target `{name}`: every source file is excluded"
    );
    for file in &files {
        ensure!(
            file.extension().is_some_and(|ext| ext == "c"),
            "target `{name}`: `{}` is not a C source file (.c)",
            file.display()
        );
    }
    Ok(files)
}

/// Files under `root` matching `pattern`, relative to `root` and sorted.
pub fn glob_files(root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    check_relative(pattern)?;
    let root_str = root
        .to_str()
        .context("the project path is not valid UTF-8")?;
    let full = format!("{}/{pattern}", glob::Pattern::escape(root_str));
    // Wildcards never match hidden entries such as .git.
    let options = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..Default::default()
    };
    let mut files = Vec::new();
    for entry in
        glob::glob_with(&full, options).with_context(|| format!("invalid pattern `{pattern}`"))?
    {
        let path = entry?;
        if path.is_file() {
            files.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

fn include_dirs(root: &Path, dirs: &[String], target: &str) -> Result<Vec<PathBuf>> {
    dirs.iter()
        .map(|dir| {
            check_relative(dir).with_context(|| format!("target `{target}`"))?;
            ensure!(
                root.join(dir).is_dir(),
                "target `{target}`: include directory `{dir}` does not exist"
            );
            Ok(PathBuf::from(dir))
        })
        .collect()
}

/// Plans may come from untrusted sources (a model, a shared cache): every path
/// must stay inside the project.
fn check_relative(path: &str) -> Result<()> {
    let inside = !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    ensure!(
        inside,
        "`{path}` must be a relative path inside the project"
    );
    Ok(())
}

pub fn check_name(name: &str, what: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ensure!(
        valid,
        "invalid {what} name `{name}`: use letters, digits, `_` and `-`"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_outside_the_project() {
        assert!(check_relative("src/*.c").is_ok());
        assert!(check_relative("../other/*.c").is_err());
        assert!(check_relative("/etc/passwd").is_err());
        assert!(check_relative("src/../../x.c").is_err());
    }

    #[test]
    fn parses_a_minimal_plan_with_defaults() {
        let plan: Plan = toml::from_str(
            r#"
            [[targets]]
            name = "hello"
            kind = "executable"
            sources = ["main.c"]
            "#,
        )
        .unwrap();
        assert_eq!(plan.schema, SCHEMA);
        assert_eq!(plan.strategy, Strategy::Native);
        assert_eq!(plan.std, "gnu17");
    }

    #[test]
    fn rejects_unknown_fields() {
        let result: Result<Plan, _> = toml::from_str(
            r#"
            [[targets]]
            name = "hello"
            kind = "executable"
            sources = ["main.c"]
            flags = ["-O3"]
            "#,
        );
        assert!(result.is_err());
    }
}
