//! `cbuild new` and `cbuild init`: a project laid out the way cbuild expects, building at once.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::plan;
use crate::toolchain::find_program;

pub struct Options {
    pub library: bool,
    pub git: bool,
}

/// What `new` created, for the summary.
pub struct Created {
    pub name: String,
    pub files: Vec<PathBuf>,
    pub git: bool,
}

pub fn new(path: &Path, options: &Options) -> Result<Created> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("the project path has no valid name")?
        .to_string();
    plan::check_name(&name, "project")?;
    ensure!(!path.exists(), "{} already exists", path.display());

    let files = write_template(path, &name, options.library)?;
    let git = options.git && init_git(path);
    Ok(Created { name, files, git })
}

/// Writes the template files of a program or library named `name` into `path`,
/// skipping files that already exist. Returns the files written or changed.
pub fn write_template(path: &Path, name: &str, library: bool) -> Result<Vec<PathBuf>> {
    let files = if library {
        library_files(name)
    } else {
        program_files(name)
    };
    let mut written = Vec::new();
    for (relative, contents) in files {
        let file = path.join(&relative);
        if file.exists() {
            continue;
        }
        fs::create_dir_all(file.parent().expect("files live in folders"))?;
        fs::write(&file, contents).with_context(|| format!("cannot write {}", file.display()))?;
        written.push(relative);
    }
    if ensure_gitignore(path)? {
        written.push(PathBuf::from(".gitignore"));
    }
    Ok(written)
}

/// Entries cbuild needs in .gitignore.
const IGNORED: &[&str] = &["/target/", "/compile_commands.json"];

/// Adds cbuild's entries to `path/.gitignore`, keeping everything already there.
/// Returns true if the file changed.
pub fn ensure_gitignore(path: &Path) -> Result<bool> {
    let file = path.join(".gitignore");
    let mut text = fs::read_to_string(&file).unwrap_or_default();
    // `target`, `/target` and `target/` all count as the same entry.
    let normalize = |line: &str| line.trim().trim_matches('/').to_string();
    let present: Vec<String> = text.lines().map(normalize).collect();
    let missing: Vec<&str> = IGNORED
        .iter()
        .filter(|entry| !present.contains(&normalize(entry)))
        .copied()
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str("# cbuild outputs\n");
    for entry in missing {
        text.push_str(entry);
        text.push('\n');
    }
    fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))?;
    Ok(true)
}

/// Starts a git repository, unless git is missing or `path` is already inside one.
pub fn init_git(path: &Path) -> bool {
    let Some(git) = find_program("git".as_ref()) else {
        return false;
    };
    let inside_repo = Command::new(&git)
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .is_ok_and(|output| output.status.success());
    !inside_repo
        && Command::new(&git)
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .is_ok_and(|status| status.success())
}

/// `name` as a C identifier: `my-lib` -> `my_lib`.
fn identifier(name: &str) -> String {
    let ident = name.replace('-', "_");
    if ident.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{ident}")
    } else {
        ident
    }
}

const DEPENDENCIES: &str = "\
# System libraries the targets can list in `deps`. Install them first, then:
#   zlib = { source = \"system\", pkg_config = \"zlib\" }   # flags from pkg-config
#   pthread = { source = \"system\", link = \"pthread\" }    # links -lpthread
[dependencies]
m = { source = \"system\", link = \"m\" }   # math library (sqrt, pow, ...)
";

fn program_files(name: &str) -> Vec<(PathBuf, String)> {
    let manifest = format!(
        "\
# Build plan for {name}. Run it with `cbuild run`, or `cbuild build --release`.
# New .c files under src/ are picked up automatically.
std = \"gnu17\"

[[targets]]
name = \"{name}\"
kind = \"executable\"
sources = [\"src/**/*.c\"]
include_private = [\"src\"]
deps = [\"m\"]

{DEPENDENCIES}"
    );
    let main = format!(
        "\
#include <stdio.h>

int main(int argc, char **argv) {{
    (void)argc;
    (void)argv;
    printf(\"Hello from {name}!\\n\");
    return 0;
}}
"
    );
    vec![
        (PathBuf::from(plan::MANIFEST), manifest),
        (PathBuf::from("src/main.c"), main),
    ]
}

fn library_files(name: &str) -> Vec<(PathBuf, String)> {
    let ident = identifier(name);
    let guard = format!("{}_H", ident.to_uppercase());
    let demo = format!("{name}-demo");
    let manifest = format!(
        "\
# Build plan for {name}.
#   cbuild build    builds the library (target/debug/lib{name}.a) and the demo
#   cbuild run      runs the demo, examples/demo.c
# Public headers go in include/, the implementation in src/.
std = \"gnu17\"

[[targets]]
name = \"{name}\"
kind = \"static_library\"
sources = [\"src/**/*.c\"]
include_public = [\"include\"]   # given to every target that depends on {name}
include_private = [\"src\"]

[[targets]]
name = \"{demo}\"
kind = \"executable\"
sources = [\"examples/demo.c\"]
deps = [\"{name}\", \"m\"]

{DEPENDENCIES}"
    );
    let header = format!(
        "\
#ifndef {guard}
#define {guard}

/* Returns the sum of a and b. */
int {ident}_add(int a, int b);

#endif /* {guard} */
"
    );
    let source = format!(
        "\
#include \"{name}.h\"

int {ident}_add(int a, int b) {{
    return a + b;
}}
"
    );
    let example = format!(
        "\
#include <stdio.h>
#include <{name}.h>

int main(void) {{
    printf(\"2 + 3 = %d\\n\", {ident}_add(2, 3));
    return 0;
}}
"
    );
    vec![
        (PathBuf::from(plan::MANIFEST), manifest),
        (PathBuf::from(format!("include/{name}.h")), header),
        (PathBuf::from(format!("src/{name}.c")), source),
        (PathBuf::from("examples/demo.c"), example),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_valid_plans() {
        for files in [program_files("hello"), library_files("my-lib")] {
            let manifest = &files
                .iter()
                .find(|(path, _)| path == Path::new(plan::MANIFEST))
                .unwrap()
                .1;
            let parsed: plan::Plan = toml::from_str(manifest).unwrap();
            assert!(!parsed.targets.is_empty());
        }
    }

    #[test]
    fn gitignore_keeps_existing_entries() {
        let dir = std::env::temp_dir().join(format!("cbuild-gitignore-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".gitignore"), "*.o\ntarget\n").unwrap();

        assert!(ensure_gitignore(&dir).unwrap());
        let text = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(
            text,
            "*.o\ntarget\n\n# cbuild outputs\n/compile_commands.json\n"
        );
        assert!(!ensure_gitignore(&dir).unwrap(), "nothing left to add");
    }

    #[test]
    fn names_become_c_identifiers() {
        assert_eq!(identifier("my-lib"), "my_lib");
        assert_eq!(identifier("3d"), "_3d");
    }
}
