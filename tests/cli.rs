//! End-to-end tests: real C projects built with the system compiler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A throwaway project directory populated with `files`.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir()
        .join(format!(
            "cbuild-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    for (path, contents) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    dir
}

fn cbuild(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .args(args)
        .current_dir(dir)
        // Keep single-file caches out of the real home directory.
        .env("HOME", dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const HELLO: &str = "#include <stdio.h>\nint main(void) { puts(\"Hello, world!\"); return 0; }\n";

#[test]
fn runs_a_single_file_without_any_setup() {
    let dir = project("single", &[("hello.c", HELLO)]);
    let output = cbuild(&dir, &["run", "hello.c"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Hello, world!\n");
    assert!(
        !dir.join("target").exists(),
        "single files must not create target/ next to them"
    );
}

#[test]
fn passes_arguments_and_exit_code_through() {
    let dir = project(
        "args",
        &[(
            "main.c",
            "#include <stdio.h>\nint main(int argc, char **argv) { puts(argv[1]); return argc; }\n",
        )],
    );
    let output = cbuild(&dir, &["run", "main.c", "--", "ciao"]);
    assert_eq!(stdout(&output), "ciao\n");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn infers_a_multi_file_project() {
    let dir = project(
        "multi",
        &[
            (
                "src/main.c",
                "#include <math.h>\n#include \"greet.h\"\nint main(void) { greet(); return (int)sqrt(0.0); }\n",
            ),
            ("src/greet.h", "void greet(void);\n"),
            (
                "src/greet.c",
                "#include <stdio.h>\n#include \"greet.h\"\nvoid greet(void) { puts(\"hi from greet\"); }\n",
            ),
        ],
    );
    let output = cbuild(&dir, &["run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "hi from greet\n");
    assert!(dir.join("target/debug/multi").is_file());
    assert!(dir.join("compile_commands.json").is_file());
}

#[test]
fn builds_libraries_declared_in_the_manifest() {
    let dir = project(
        "lib",
        &[
            (
                "cbuild.toml",
                r#"
                [[targets]]
                name = "app"
                kind = "executable"
                sources = ["src/*.c"]
                deps = ["mathx"]

                [[targets]]
                name = "mathx"
                kind = "static_library"
                sources = ["mathx/*.c"]
                include_public = ["mathx/include"]
                deps = ["m"]

                [dependencies]
                m = { source = "system", link = "m" }
                "#,
            ),
            ("mathx/include/mathx.h", "double hyp(double a, double b);\n"),
            (
                "mathx/hyp.c",
                "#include <math.h>\n#include \"mathx.h\"\ndouble hyp(double a, double b) { return sqrt(a*a + b*b); }\n",
            ),
            (
                "src/main.c",
                "#include <stdio.h>\n#include <mathx.h>\nint main(void) { printf(\"%g\\n\", hyp(3, 4)); return 0; }\n",
            ),
        ],
    );
    let output = cbuild(&dir, &["run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "5\n");
    assert!(dir.join("target/debug/libmathx.a").is_file());
}

#[test]
fn dev_and_release_use_different_flags() {
    let dir = project("profiles", &[("main.c", HELLO)]);
    assert!(cbuild(&dir, &["build"]).status.success());
    let dev = fs::read_to_string(dir.join("target/debug/compile_commands.json")).unwrap();
    assert!(cbuild(&dir, &["build", "--release"]).status.success());
    let release = fs::read_to_string(dir.join("target/release/compile_commands.json")).unwrap();
    assert!(dev.contains("\"-O0\"") && dev.contains("\"-g\""));
    assert!(release.contains("\"-O2\"") && release.contains("\"-DNDEBUG\""));
}

#[test]
fn explains_common_errors() {
    let dir = project(
        "errors",
        &[
            (
                "undeclared.c",
                "int main(void) { puts(\"hi\"); return 0; }\n",
            ),
            (
                "missing.c",
                "#include <raylib.h>\nint main(void) { return 0; }\n",
            ),
            (
                "undefined.c",
                "int foo(void);\nint main(void) { return foo(); }\n",
            ),
        ],
    );
    let output = cbuild(&dir, &["run", "undeclared.c"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("add `#include <stdio.h>`"),
        "{}",
        stderr(&output)
    );

    let output = cbuild(&dir, &["run", "missing.c"]);
    assert!(
        stderr(&output).contains("pkg_config = \"raylib\""),
        "{}",
        stderr(&output)
    );

    let output = cbuild(&dir, &["run", "undefined.c"]);
    assert!(
        stderr(&output).contains("`foo` is declared but never defined"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn infers_a_library_and_skips_tests() {
    let dir = project(
        "mylib",
        &[
            (
                "mylib.c",
                "#include \"mylib.h\"\nint twice(int x) { return 2 * x; }\n#ifdef SELF_TEST\nint main(void) { return twice(0); }\n#endif\n",
            ),
            ("mylib.h", "int twice(int x);\n"),
            (
                "test.c",
                "#include \"mylib.h\"\nint main(void) { return twice(0); }\n",
            ),
        ],
    );
    let output = cbuild(&dir, &["build"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.join("target/debug/libmylib.a").is_file());

    let plan = stdout(&cbuild(&dir, &["plan"]));
    assert!(plan.contains("kind = \"static_library\""), "{plan}");
    assert!(plan.contains("exclude = [\"test.c\"]"), "{plan}");
    assert!(
        plan.contains("ignored main() only compiled under a build option in: mylib.c"),
        "{plan}"
    );
}

#[test]
fn refuses_ambiguous_projects() {
    let dir = project(
        "two-mains",
        &[
            ("src/a.c", "int main(void) { return 0; }\n"),
            ("src/b.c", "int main(void) { return 0; }\n"),
        ],
    );
    let output = cbuild(&dir, &["build"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("2 files define main(): src/a.c, src/b.c"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn rejects_invalid_plans() {
    let cases = [
        (
            "unknown field",
            "[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"*.c\"]\nflags = [\"-O3\"]\n",
            "unknown field",
        ),
        (
            "escaping path",
            "[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"../*.c\"]\n",
            "relative path inside the project",
        ),
        (
            "no match",
            "[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"lib/*.c\"]\n",
            "matches no files",
        ),
        (
            "overlap",
            "[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"*.c\"]\n[[targets]]\nname = \"b\"\nkind = \"executable\"\nsources = [\"main.c\"]\n",
            "belongs to both",
        ),
        (
            "cycle",
            "[[targets]]\nname = \"a\"\nkind = \"static_library\"\nsources = [\"main.c\"]\ndeps = [\"b\"]\n[[targets]]\nname = \"b\"\nkind = \"static_library\"\nsources = [\"x.c\"]\ndeps = [\"a\"]\n",
            "dependency cycle between targets: a -> b -> a",
        ),
        (
            "strategy",
            "strategy = \"cmake\"\n[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"*.c\"]\n",
            "not supported yet",
        ),
    ];
    for (case, manifest, expected) in cases {
        let dir = project(
            "invalid",
            &[
                ("cbuild.toml", manifest),
                ("main.c", HELLO),
                ("x.c", "int x;\n"),
            ],
        );
        let output = cbuild(&dir, &["build"]);
        assert!(!output.status.success(), "{case}: should fail");
        assert!(
            stderr(&output).contains(expected),
            "{case}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn plan_write_freezes_the_inferred_plan() {
    let dir = project("freeze", &[("main.c", HELLO)]);
    assert!(cbuild(&dir, &["plan", "--write"]).status.success());
    let manifest = fs::read_to_string(dir.join("cbuild.toml")).unwrap();
    assert!(manifest.contains("schema = \"cbuild-plan/1\""));
    let output = cbuild(&dir, &["run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !stderr(&output).contains("Inferred"),
        "a frozen plan must not be re-inferred"
    );
}

#[test]
fn never_touches_a_target_directory_it_did_not_create() {
    let dir = project(
        "foreign",
        &[("main.c", HELLO), ("target/notes.txt", "mine")],
    );
    assert!(!cbuild(&dir, &["build"]).status.success());
    assert!(!cbuild(&dir, &["clean"]).status.success());
    assert!(dir.join("target/notes.txt").is_file());
}

#[test]
fn mcp_server_verifies_and_saves_a_plan() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = project("mcp", &[("main.c", HELLO)]);
    let plan = r#"{"targets":[{"name":"hello","kind":"executable","sources":["*.c"]}]}"#;
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"finish","arguments":{{"plan":{plan},"notes":"x"}}}}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"try_build","arguments":{{"plan":{plan}}}}}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"finish","arguments":{{"plan":{plan},"notes":"one program"}}}}}}"#),
    ];

    let mut child = Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .arg("mcp")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all((requests.join("\n") + "\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    let replies: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 5, "notifications get no reply");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 5);
    assert_eq!(
        replies[2]["result"]["isError"], true,
        "finish before a verified build must fail"
    );
    assert!(
        replies[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"success\"")
    );
    assert_eq!(replies[4]["result"]["isError"], false);

    let manifest = fs::read_to_string(dir.join("cbuild.toml")).unwrap();
    assert!(manifest.contains("verified by a real build") && manifest.contains("one program"));
}

/// A project the heuristics cannot plan: sources outside `src/`, two programs.
fn unplannable() -> PathBuf {
    project(
        "unplannable",
        &[
            ("app/main.c", HELLO),
            ("tools/gen.c", HELLO),
            ("CMakeLists.txt", "project(x C)\n"),
        ],
    )
}

/// PATH with the system compiler and, optionally, a fake `claude` that must never run.
fn path_with_claude(dir: &Path, claude: bool) -> String {
    let agents: &[&str] = if claude { &["claude"] } else { &[] };
    path_with_agents(dir, agents)
}

/// PATH with the system compiler and fake agent CLIs that fail the test if they ever run.
fn path_with_agents(dir: &Path, agents: &[&str]) -> String {
    let bin = dir.join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    for agent in agents {
        let fake = bin.join(agent);
        fs::write(
            &fake,
            format!("#!/bin/sh\necho 'fake {agent} must not run' >&2\nexit 99\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    format!("{}:/usr/bin:/bin", bin.display())
}

#[test]
fn uses_codex_when_it_is_the_only_agent_installed() {
    let dir = unplannable();
    let output = cbuild_with_path(&dir, &path_with_agents(&dir, &["codex"]), &["build"]);
    let err = stderr(&output);
    assert!(err.contains("runs through Codex on your account"), "{err}");
    assert!(err.contains("run again with --yes"), "{err}");
    assert!(
        !err.contains("must not run"),
        "no agent starts without consent"
    );
}

#[test]
fn provider_choice_is_respected() {
    let dir = unplannable();
    let both = path_with_agents(&dir, &["claude", "codex"]);

    let default = cbuild_with_path(&dir, &both, &["build"]);
    assert!(
        stderr(&default).contains("runs through Claude Code"),
        "{}",
        stderr(&default)
    );

    let chosen = cbuild_with_path(&dir, &both, &["build", "--provider", "codex"]);
    assert!(
        stderr(&chosen).contains("runs through Codex"),
        "{}",
        stderr(&chosen)
    );

    let from_env = Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .arg("build")
        .current_dir(&dir)
        .env("PATH", &both)
        .env("CBUILD_AI_PROVIDER", "codex")
        .output()
        .unwrap();
    assert!(
        stderr(&from_env).contains("runs through Codex"),
        "{}",
        stderr(&from_env)
    );
}

#[test]
fn a_missing_chosen_provider_is_explained() {
    let dir = unplannable();
    let only_claude = path_with_agents(&dir, &["claude"]);
    let output = cbuild_with_path(&dir, &only_claude, &["build", "--provider", "codex"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(
        err.contains("asked to use Codex, which is not installed"),
        "{err}"
    );
    assert!(!err.contains("must not run"));
}

#[test]
fn build_suggests_installing_claude_code_when_heuristics_fail() {
    let dir = unplannable();
    let output = Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .arg("build")
        .current_dir(&dir)
        .env("PATH", path_with_claude(&dir, false))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("install Claude Code"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn build_asks_before_handing_the_project_to_the_ai() {
    let dir = unplannable();
    let output = Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .arg("build")
        .current_dir(&dir)
        .env("PATH", path_with_claude(&dir, true))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(err.contains("heuristics cannot plan this project"), "{err}");
    assert!(err.contains("run again with --yes"), "{err}");
    assert!(
        !err.contains("fake claude must not run"),
        "the AI must not start without consent"
    );
    assert!(!dir.join("cbuild.toml").exists());
}

fn cbuild_with_path(dir: &Path, path: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// The heuristics pick `src/` only, so the code in `lib/` is missing at link time.
fn inferred_plan_that_fails() -> PathBuf {
    project(
        "inferred-fails",
        &[
            (
                "src/main.c",
                "int helper(void);\nint main(void) { return helper(); }\n",
            ),
            ("lib/helper.c", "int helper(void) { return 0; }\n"),
        ],
    )
}

#[test]
fn offers_the_ai_when_an_inferred_plan_does_not_build() {
    let dir = inferred_plan_that_fails();
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["build"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(
        err.contains("`helper` is declared but never defined"),
        "{err}"
    );
    assert!(
        err.contains("inferred from the sources does not build"),
        "{err}"
    );
    assert!(err.contains("run again with --yes"), "{err}");
    assert!(!err.contains("fake claude must not run"));
}

#[test]
fn no_ai_keeps_the_ai_out() {
    for dir in [unplannable(), inferred_plan_that_fails()] {
        let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["build", "--no-ai"]);
        let err = stderr(&output);
        assert!(!output.status.success());
        assert!(!err.contains("AI build engineer"), "{err}");
    }
}

#[test]
fn blames_the_plan_when_a_project_with_build_files_fails() {
    let dir = project(
        "needs-define",
        &[
            ("Makefile", "CFLAGS += -DVERSION=3\nall: main\n"),
            ("main.c", "int main(void) { return VERSION - 3; }\n"),
        ],
    );
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["build"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(
        err.contains("inferred from the sources does not build"),
        "{err}"
    );
    assert!(!err.contains("fake claude must not run"));
}

#[test]
fn never_offers_the_ai_for_errors_in_the_code() {
    let dir = project("code-error", &[("main.c", "int main(void) { return 0 }\n")]);
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["build"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(!err.contains("AI build engineer"), "{err}");
}

#[test]
fn suggests_fix_for_a_manifest_that_no_longer_builds() {
    let dir = project(
        "stale-manifest",
        &[
            (
                "cbuild.toml",
                "[[targets]]\nname = \"app\"\nkind = \"executable\"\nsources = [\"src/*.c\"]\n",
            ),
            (
                "src/main.c",
                "int helper(void);\nint main(void) { return helper(); }\n",
            ),
            ("src/old.c", "int unused;\n"),
        ],
    );
    fs::remove_file(dir.join("src/old.c")).unwrap();
    fs::write(dir.join("lib.c"), "int helper(void) { return 0; }\n").unwrap();
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["build"]);
    let err = stderr(&output);
    assert!(err.contains("`cbuild fix`"), "{err}");
    assert!(
        !err.contains("fake claude must not run"),
        "a cbuild.toml is never changed unasked"
    );
}

#[test]
fn fix_does_nothing_when_the_plan_builds() {
    let dir = project("fix-ok", &[("main.c", HELLO)]);
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["fix"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("nothing to fix"));
}

#[test]
fn fix_asks_before_handing_the_project_to_the_ai() {
    let dir = inferred_plan_that_fails();
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["fix"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(err.contains("the current plan does not build"), "{err}");
    assert!(err.contains("run again with --yes"), "{err}");
    assert!(!err.contains("fake claude must not run"));
}

#[test]
fn new_creates_a_program_that_runs() {
    let dir = project("new-program", &[]);
    fs::create_dir_all(&dir).unwrap();
    let created = cbuild(&dir, &["new", "hello", "--no-git"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let hello = dir.join("hello");
    assert!(hello.join("cbuild.toml").is_file() && hello.join(".gitignore").is_file());
    assert!(!hello.join(".git").exists(), "--no-git");

    let output = cbuild(&hello, &["run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Hello from hello!\n");
}

#[test]
fn new_lib_creates_a_library_and_a_demo() {
    let dir = project("new-library", &[]);
    fs::create_dir_all(&dir).unwrap();
    assert!(
        cbuild(&dir, &["new", "my-lib", "--lib", "--no-git"])
            .status
            .success()
    );
    let lib = dir.join("my-lib");

    let output = cbuild(&lib, &["run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "2 + 3 = 5\n");
    assert!(lib.join("target/debug/libmy-lib.a").is_file());
}

#[test]
fn new_never_overwrites() {
    let dir = project("new-existing", &[("taken/keep.txt", "mine")]);
    let output = cbuild(&dir, &["new", "taken", "--no-git"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("already exists"));
    assert_eq!(
        fs::read_to_string(dir.join("taken/keep.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn init_an_empty_folder_creates_a_starter_program() {
    let dir = project("init-empty", &[]);
    fs::create_dir_all(&dir).unwrap();
    let output = cbuild(&dir, &["init", "--no-git"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.join("src/main.c").is_file() && dir.join("cbuild.toml").is_file());
    assert_eq!(stdout(&cbuild(&dir, &["run"])), "Hello from init-empty!\n");
}

#[test]
fn init_existing_code_writes_a_verified_plan_and_keeps_files() {
    let dir = project(
        "init-existing",
        &[("src/main.c", HELLO), (".gitignore", "*.o\n")],
    );
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["init", "--no-git"]);
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(err.contains("verified by a real build"), "{err}");
    assert!(!err.contains("fake claude must not run"));
    assert!(
        !dir.join("src/hello.c").exists(),
        "no template over existing code"
    );
    assert_eq!(
        fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "*.o\n\n# cbuild outputs\n/target/\n/compile_commands.json\n"
    );
    assert!(cbuild(&dir, &["run"]).status.success());
}

#[test]
fn init_never_writes_a_plan_that_does_not_build() {
    let dir = inferred_plan_that_fails();
    let output = cbuild(&dir, &["init", "--no-git", "--no-ai"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("does not build"),
        "{}",
        stderr(&output)
    );
    assert!(!dir.join("cbuild.toml").exists());
}

#[test]
fn init_refuses_a_project_already_set_up() {
    let dir = project(
        "init-twice",
        &[
            (
                "cbuild.toml",
                "[[targets]]\nname = \"a\"\nkind = \"executable\"\nsources = [\"*.c\"]\n",
            ),
            ("main.c", HELLO),
        ],
    );
    let output = cbuild(&dir, &["init", "--no-git"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("already set up"));
}

#[test]
fn sync_needs_a_plan_and_asks_before_using_the_ai() {
    let dir = project("sync", &[("main.c", HELLO)]);
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["sync"]);
    assert!(
        stderr(&output).contains("run `cbuild init` first"),
        "{}",
        stderr(&output)
    );

    assert!(cbuild(&dir, &["plan", "--write"]).status.success());
    let output = cbuild_with_path(&dir, &path_with_claude(&dir, true), &["sync"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(err.contains("compare the plan with the project"), "{err}");
    assert!(err.contains("run again with --yes"), "{err}");
    assert!(!err.contains("fake claude must not run"));
}

#[test]
fn update_check_compares_with_a_local_checkout() {
    let newer = project(
        "checkout-new",
        &[(
            "Cargo.toml",
            "[package]\nname = \"cbuild\"\nversion = \"99.0.0\"\n",
        )],
    );
    let output = cbuild(
        &newer,
        &["update", "--check", "--from", newer.to_str().unwrap()],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cbuild 99.0.0 (you have"),
        "{}",
        stderr(&output)
    );

    let older = project(
        "checkout-old",
        &[(
            "Cargo.toml",
            "[package]\nname = \"cbuild\"\nversion = \"0.0.1\"\n",
        )],
    );
    let output = cbuild(
        &older,
        &["update", "--check", "--from", older.to_str().unwrap()],
    );
    assert!(
        stderr(&output).contains("is up to date"),
        "{}",
        stderr(&output)
    );

    let other = project(
        "checkout-other",
        &[(
            "Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"9.0.0\"\n",
        )],
    );
    let output = cbuild(
        &other,
        &["update", "--check", "--from", other.to_str().unwrap()],
    );
    assert!(
        stderr(&output).contains("is not a cbuild checkout"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn update_explains_when_no_source_is_known() {
    let dir = project("update-nosource", &[]);
    fs::create_dir_all(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cbuild"))
        .args(["update", "--check"])
        .current_dir(&dir)
        .env_remove("CBUILD_UPDATE_SOURCE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no update source known"),
        "{}",
        stderr(&output)
    );
}

/// A library, a program using it with a quoted macro, and libm: what exports must keep.
fn exportable_project(name: &str) -> PathBuf {
    project(
        name,
        &[
            (
                "cbuild.toml",
                r#"
                [[targets]]
                name = "app"
                kind = "executable"
                sources = ["src/*.c"]
                defines = { GREETING = "\"hyp\"" }
                deps = ["mathx"]

                [[targets]]
                name = "mathx"
                kind = "static_library"
                sources = ["mathx/*.c"]
                include_public = ["mathx/include"]
                deps = ["m"]

                [dependencies]
                m = { source = "system", link = "m" }
                "#,
            ),
            ("mathx/include/mathx.h", "double hyp(double a, double b);\n"),
            (
                "mathx/hyp.c",
                "#include <math.h>\n#include \"mathx.h\"\ndouble hyp(double a, double b) { return sqrt(a*a + b*b); }\n",
            ),
            (
                "src/main.c",
                "#include <stdio.h>\n#include <mathx.h>\nint main(void) { printf(\"%s %g\\n\", GREETING, hyp(3, 4)); return 0; }\n",
            ),
        ],
    )
}

fn available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn exported_cmake_builds_the_same_program() {
    if !available("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let dir = exportable_project("export-cmake");
    let output = cbuild(&dir, &["export", "cmake"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let build = dir.join("cmake-build");
    let configure = Command::new("cmake")
        .arg("-S")
        .arg(&dir)
        .arg("-B")
        .arg(&build)
        .output()
        .unwrap();
    assert!(configure.status.success(), "{}", stderr(&configure));
    let compile = Command::new("cmake")
        .arg("--build")
        .arg(&build)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}{}",
        stdout(&compile),
        stderr(&compile)
    );
    let run = Command::new(build.join("app")).output().unwrap();
    assert_eq!(stdout(&run), "hyp 5\n");
}

#[test]
fn exported_makefile_builds_the_same_program() {
    if !available("make") {
        eprintln!("skipped: make is not installed");
        return;
    }
    let dir = exportable_project("export-make");
    let output = cbuild(&dir, &["export", "make"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let compile = Command::new("make").current_dir(&dir).output().unwrap();
    assert!(
        compile.status.success(),
        "{}{}",
        stdout(&compile),
        stderr(&compile)
    );
    let run = Command::new(dir.join("build/debug/app")).output().unwrap();
    assert_eq!(stdout(&run), "hyp 5\n");

    let again = Command::new("make").current_dir(&dir).output().unwrap();
    assert!(
        stdout(&again).contains("Nothing to be done"),
        "{}",
        stdout(&again)
    );

    // The release build must really be rebuilt with its own flags.
    let release = Command::new("make")
        .arg("RELEASE=1")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(stdout(&release).contains("-O2"), "{}", stdout(&release));
    assert!(dir.join("build/release/app").is_file());

    fs::create_dir_all(dir.join("build/cmake")).unwrap();
    let clean = Command::new("make")
        .arg("clean")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(clean.status.success());
    assert!(!dir.join("build/debug").exists() && !dir.join("build/release").exists());
    assert!(
        dir.join("build/cmake").exists(),
        "other builds in build/ are kept"
    );
}

#[test]
fn export_never_replaces_build_files_written_by_others() {
    let dir = exportable_project("export-existing");
    fs::write(dir.join("CMakeLists.txt"), "project(mine C)\n").unwrap();

    let output = cbuild(&dir, &["export", "cmake"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--output cbuild-export/CMakeLists.txt"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(dir.join("CMakeLists.txt")).unwrap(),
        "project(mine C)\n"
    );

    let elsewhere = cbuild(
        &dir,
        &[
            "export",
            "cmake",
            "--output",
            "cbuild-export/CMakeLists.txt",
        ],
    );
    assert!(elsewhere.status.success(), "{}", stderr(&elsewhere));
    let generated = fs::read_to_string(dir.join("cbuild-export/CMakeLists.txt")).unwrap();
    assert!(
        generated.contains("set(ROOT \"${CMAKE_CURRENT_SOURCE_DIR}/..\")"),
        "{generated}"
    );
    assert!(
        cbuild(
            &dir,
            &[
                "export",
                "cmake",
                "--output",
                "cbuild-export/CMakeLists.txt"
            ]
        )
        .status
        .success(),
        "regenerating its own file"
    );

    assert!(
        cbuild(&dir, &["export", "cmake", "--force"])
            .status
            .success()
    );
    assert!(
        fs::read_to_string(dir.join("CMakeLists.txt"))
            .unwrap()
            .starts_with("# Generated by cbuild")
    );
}

#[test]
fn clean_removes_its_own_outputs() {
    let dir = project("clean", &[("main.c", HELLO)]);
    assert!(cbuild(&dir, &["build"]).status.success());
    assert!(cbuild(&dir, &["clean"]).status.success());
    assert!(!dir.join("target").exists());
}
