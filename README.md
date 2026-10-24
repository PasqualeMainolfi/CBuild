# cbuild

> [!WARNING]
> **cbuild is under active development.** It is not ready for production use yet: commands, the `cbuild.toml` format and the behavior described here may still change between versions, and there is no stable release. Try it, report problems, and keep your existing build files until cbuild covers your project. See [Status](#status) and [Limitations](#limitations).

**Build and run C projects without writing build files.**

```sh
cbuild run hello.c          # one file: compile and run
cbuild build --release      # a whole project, even one that only has a CMakeLists.txt
```

No `CMakeLists.txt`, no `Makefile`, no compiler flags. Simple projects are understood from their sources. For complex ones, an AI build engineer reads the project once, writes a build plan and checks it with a real build. From then on every build is fast, deterministic and works offline, with no AI involved.

## Status

**In development, version 0.1.0, not yet released.**

| Area | State |
|---|---|
| Building programs and static libraries from a plan | works |
| Heuristic plans for simple projects | works |
| AI build engineer (through Claude Code or Codex) | works, being measured on real projects |
| Export to CMake and Make | works |
| Platforms | tested on macOS with Clang; Linux expected to work but not tested yet; Windows not supported |
| Format of `cbuild.toml` | may still change |

On a corpus of 20 real C projects, from `kilo` to `redis` (see [Corpus](#corpus)), cbuild builds 18 with no hand-written build file, 17 of them with every expected program and library. What is missing (htop, redis, and quickjs's `qjs` program) needs files generated during the build, such as a `config.h` from `configure`, which cbuild cannot produce yet.

---

## Contents

- [Status](#status)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Common workflows](#common-workflows)
- [How cbuild finds the plan](#how-cbuild-finds-the-plan)
- [Commands](#commands)
- [The build plan (`cbuild.toml`)](#the-build-plan-cbuildtoml)
- [The AI build engineer](#the-ai-build-engineer)
- [Error messages](#error-messages)
- [Compiler, profiles and outputs](#compiler-profiles-and-outputs)
- [Exporting to CMake or Make](#exporting-to-cmake-or-make)
- [Using cbuild from other AI tools (MCP)](#using-cbuild-from-other-ai-tools-mcp)
- [Limitations](#limitations)
- [Development](#development)

---

## Installation

You need:

| Requirement | Why | Required? |
|---|---|---|
| [Rust](https://rustup.rs) | to build cbuild itself | yes |
| A C compiler: Clang or GCC | to compile your code | yes |
| `pkg-config` | only for libraries declared with `pkg_config` | optional |
| [Claude Code](https://claude.com/claude-code) or [Codex](https://github.com/openai/codex), logged in | only for the AI build engineer (see [AI providers](#ai-providers)) | optional |

On macOS, `xcode-select --install` gives you a compiler. On Debian/Ubuntu, use `sudo apt install build-essential`.

**From a release** (no Rust needed): download the archive for your system from the GitHub releases page, check it and put `cbuild` in your `PATH`:

| System | Archive |
|---|---|
| macOS, Apple Silicon | `cbuild-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `cbuild-vX.Y.Z-x86_64-apple-darwin.tar.gz` |
| Linux, x86_64 (any distribution) | `cbuild-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` |
| Linux, ARM64 (any distribution) | `cbuild-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` |

```sh
shasum -a 256 -c cbuild-vX.Y.Z-<system>.tar.gz.sha256
tar -xzf cbuild-vX.Y.Z-<system>.tar.gz
mv cbuild-vX.Y.Z-<system>/cbuild ~/.local/bin/
```

The macOS binaries are not signed. If macOS blocks a binary downloaded with a browser, run `xattr -d com.apple.quarantine ~/.local/bin/cbuild`.

**From source:**

```sh
git clone https://github.com/PasqualeMainolfi/CBuild.git
cd CBuild
cargo install --path .
```

This installs the `cbuild` command in `~/.cargo/bin`.

### Updating

```sh
cbuild update --check                       # is there a newer version?
cbuild update                               # install it
cbuild update --from ~/src/cbuild           # from a local checkout
cbuild update --from https://example.com/cbuild.git
```

`cbuild update` installs the newest version with `cargo install`, so Rust must be installed. It looks for updates in:

1. the `--from` option;
2. the `CBUILD_UPDATE_SOURCE` environment variable;
3. the `repository` field of cbuild's `Cargo.toml`.

A source can be a git repository, where releases are tags like `v1.2.0`, or a local checkout of cbuild, where the version is read from its `Cargo.toml`. cbuild only installs a version newer than the one running.

---

## Quick start

### A new project

```sh
cbuild new hello            # a program
cd hello
cbuild run
```

```
     Created program `hello` in hello
             cbuild.toml
             src/main.c
             .gitignore
             (git repository)
   Compiling hello (gnu17, Apple clang 21.0.0, dev)
     Running target/debug/hello
Hello from hello!
```

For a library, use `cbuild new mylib --lib`. It creates the library (`include/mylib.h`, `src/mylib.c`) and a demo program (`examples/demo.c`) that uses it, so `cbuild run` works at once:

```
mylib/
├── cbuild.toml          # library target + demo target, with comments
├── include/mylib.h      # public header: what users of the library include
├── src/mylib.c          # implementation
├── examples/demo.c      # a program using the library
└── .gitignore           # target/ and compile_commands.json
```

Add `.c` files under `src/` and they are built automatically. The generated `cbuild.toml` has comments showing where to declare system libraries. A git repository is started unless you pass `--no-git`, or the folder is already inside one.

### An existing project

```sh
cd my-existing-code
cbuild init
```

- **The folder has C sources:** cbuild infers a plan and **checks it with a real build** before saving it as `cbuild.toml`. If that plan does not build, it offers the AI build engineer. It never saves a plan that does not build.
- **The folder has no C sources:** cbuild creates the same starter files as `cbuild new` (`--lib` for a library). Files that already exist are never overwritten.

Either way, `/target/` and `/compile_commands.json` are added to `.gitignore`, keeping what is already there. A git repository is started unless `--no-git` is passed or the folder is already in one.

### A single file

```sh
cbuild run hello.c
```

```
   Compiling hello (gnu17, Apple clang 21.0.0, dev)
    Finished dev in 0.13s
     Running ~/.cbuild/cache/single/hello-…/debug/hello
Hello, world!
```

Nothing is written next to your file. Outputs go to a cache in your home directory.

### A simple project

Put your `.c` and `.h` files in `src/` (or directly in the project folder) with exactly one `main()`:

```
myapp/
└── src/
    ├── main.c
    ├── greet.c
    └── greet.h
```

```sh
cd myapp
cbuild run
```

cbuild finds the sources and the `main()`, adds the include folders, links the math library and builds `target/debug/myapp`.

### A complex project

Say a project has a library, a program, test programs, Windows-only files and a `CMakeLists.txt` that sets macros and links threads. If Claude Code or Codex is installed:

```sh
cbuild build --release
```

```
note: cbuild's heuristics cannot plan this project: ...
No build plan yet. Let the AI build engineer design it? [Y/n] y
       Agent list_files **/*
       Agent read_file CMakeLists.txt
       Agent try_build
       Build success
       Agent finish
    Verified new plan built successfully, in 17s
     Created cbuild.toml
   Compiling wxcore (gnu17, Apple clang 21.0.0, release)
   Compiling wx (gnu17, Apple clang 21.0.0, release)
    Finished release in 0.11s
```

The plan is saved in `cbuild.toml`. The next `cbuild build --release` reads it directly, with no AI. Commit `cbuild.toml` so everyone gets the same build.

---

## Common workflows

Each block is a complete sequence of commands, from start to finished build. Pick the one that matches your situation.

| I want to… | Workflow |
|---|---|
| start a new program or library | [1](#1-new-program) · [2](#2-new-library) |
| run a single `.c` file | [3](#3-single-file) |
| build code I already have | [4](#4-existing-simple-code) · [5](#5-existing-project-with-its-own-build-files) |
| keep building after the project changed | [6](#6-the-project-changed) |
| build in CI | [7](#7-continuous-integration) |
| get a `CMakeLists.txt` or a `Makefile` | [8](#8-export-and-build-with-cmake) · [9](#9-export-and-build-with-make) · [10](#10-from-any-project-to-cmake-in-one-go) |

### 1. New program

```sh
cbuild new hello
cd hello
cbuild run                      # build (debug) and run
# ... edit or add .c files under src/ ...
cbuild run -- --some-arg        # arguments after -- go to your program
cbuild build --release          # optimized: target/release/hello
```

### 2. New library

```sh
cbuild new mylib --lib
cd mylib
cbuild build                    # target/debug/libmylib.a and the demo program
cbuild run                      # runs examples/demo.c, linked with the library
cbuild build --release          # target/release/libmylib.a
```

Programs in other projects use `include/mylib.h` and link `libmylib.a`.

### 3. Single file

```sh
cbuild run hello.c              # build and run, nothing written next to the file
cbuild run hello.c -- a b c     # with arguments
cbuild build --release hello.c  # optimized build only
```

### 4. Existing simple code

Code with one program (or one library) in `src/` or in the project folder:

```sh
cd my-code
cbuild init                     # writes cbuild.toml after checking it with a real build
cbuild run
git add cbuild.toml .gitignore && git commit -m "Build with cbuild"
```

Skipping `cbuild init` also works: `cbuild run` infers the same plan on every build. `init` saves it, so you can read it, edit it and commit it.

### 5. Existing project with its own build files

A project with a `CMakeLists.txt`, `Makefile`, `configure` or `meson.build`, several programs, or platform-specific files. Claude Code or Codex must be installed:

```sh
cd some-project
cbuild build --release          # first time only: asks, the AI writes cbuild.toml, then it builds
cbuild build --release          # every next time: reads cbuild.toml, no AI, works offline
git add cbuild.toml && git commit -m "Add cbuild plan"
```

To only create the plan, without building: `cbuild init` (or `cbuild plan --ai`).

### 6. The project changed

```sh
cbuild build                    # fails: a new file or library is missing from the plan
cbuild fix                      # the AI repairs the plan; shows the change
cbuild build
```

```sh
cbuild build                    # still works, but a new program was added to the project
cbuild sync                     # the AI proposes the new targets; you confirm
cbuild run --bin new-program
```

Both keep the previous plan in `target/cbuild-agent/previous-cbuild.toml`.

### 7. Continuous integration

With `cbuild.toml` committed, CI needs no AI and no network:

```sh
cbuild build --release --no-ai
```

To let the AI design a missing plan without a terminal (Claude Code or Codex must be logged in on the machine):

```sh
cbuild build --release --yes
```

### 8. Export and build with CMake

```sh
cbuild export cmake                                  # CMakeLists.txt in the project root
cmake -S . -B cmake-build -DCMAKE_BUILD_TYPE=Release
cmake --build cmake-build                            # programs and libraries in cmake-build/
```

If the project already has its own `CMakeLists.txt`, cbuild will not replace it. Export next to it:

```sh
cbuild export cmake --output cbuild-export/CMakeLists.txt
cmake -S cbuild-export -B cmake-build
cmake --build cmake-build
```

### 9. Export and build with Make

```sh
cbuild export make              # Makefile in the project root
make                            # debug build in build/debug/
make RELEASE=1                  # optimized build in build/release/
make clean
```

If the project already has its own `Makefile`:

```sh
cbuild export make --output cbuild.mk
make -f cbuild.mk
make -f cbuild.mk RELEASE=1
```

### 10. From any project to CMake in one go

cbuild can produce a clean `CMakeLists.txt` even for a project that never had one, or one whose build files are hard to follow:

```sh
cd some-project
cbuild build                    # cbuild finds or designs the plan and checks it builds
cbuild export cmake --output cbuild-export/CMakeLists.txt
cmake -S cbuild-export -B cmake-build && cmake --build cmake-build
```

After changing the project, run `cbuild sync` (or `cbuild fix`) and export again: the exported files list sources explicitly, so they do not pick up new files by themselves.

## How cbuild finds the plan

Every build follows a **plan**: which targets to build, from which sources, with which include folders, macros and libraries. cbuild looks for it in this order:

1. **`cbuild.toml`** in the current folder or a parent folder. It is used as is.
2. **Heuristics.** If there is no `cbuild.toml`, cbuild infers a plan from the sources in `src/` (or in the project folder):
   - exactly one file defines `main()`: one program;
   - no file defines `main()`: one static library, with its headers made public;
   - tests, examples, benchmarks and fuzzers (`test.c`, `foo_test.c`, `examples/`, …) are left out;
   - a `main()` that is only compiled under a build option (`#ifdef TEST`, `#if 0`) is ignored. A `main()` under a platform check (`#ifdef _WIN32 … #else …`) still counts.
3. **AI build engineer.** If the heuristics cannot plan the project, or their plan does not build because of a missing header or symbol, cbuild offers to let the AI design or repair the plan. It always asks first.

The AI only designs the plan. cbuild's own engine always runs the build, and the AI's plan is only saved after a real build succeeds.

---

## Commands

| Command | What it does |
|---|---|
| `cbuild new PATH [--lib] [--no-git]` | Create a program (or a library with a demo) laid out for cbuild |
| `cbuild init [PATH]` | Set up cbuild in an existing folder (see [An existing project](#an-existing-project)) |
| `cbuild build [FILE]` | Compile the project, or a single `.c` file |
| `cbuild run [FILE] [-- ARGS…]` | Compile, then run the program with `ARGS` |
| `cbuild plan` | Print the plan: `cbuild.toml`, or the one inferred from the sources |
| `cbuild plan --write` | Save the inferred plan as `cbuild.toml` |
| `cbuild plan --ai` | Let the AI build engineer design a plan, without building afterwards |
| `cbuild fix` | Let the AI build engineer repair a plan that no longer builds |
| `cbuild sync` | Let the AI build engineer bring the plan in line with a project that changed |
| `cbuild update` | Update cbuild itself (see [Updating](#updating)) |
| `cbuild export cmake\|make` | Write the plan as a `CMakeLists.txt` or a `Makefile` (see [Exporting](#exporting-to-cmake-or-make)) |
| `cbuild clean` | Delete `target/` |
| `cbuild mcp` | Serve cbuild's planning tools to an AI client (see [MCP](#using-cbuild-from-other-ai-tools-mcp)) |

Options for `build` and `run`:

| Option | Meaning |
|---|---|
| `-r`, `--release` | Optimized build (`-O2 -DNDEBUG`) instead of a debug build |
| `-j N`, `--jobs N` | Compile at most `N` files at once (default: number of CPU cores) |
| `-y`, `--yes` | Let the AI design or repair the plan without asking (for CI) |
| `--no-ai` | Never use the AI: only `cbuild.toml` or the heuristics |
| `--bin NAME` | `run` only: which program to run when there are several |

`plan --ai`, `fix`, `sync` and `init` also accept `-y`/`--yes`; `init` also accepts `--no-ai`.

When the program run by `cbuild run` crashes, cbuild explains the signal, for example *SIGSEGV: invalid memory access, such as a NULL or dangling pointer*. The program's exit code is passed through.

---

## The build plan (`cbuild.toml`)

You rarely write it by hand: `cbuild plan --write` and the AI create it for you. It is plain, declarative TOML: there are no scripts, no shell commands and no compiler flags.

### Example

```toml
std = "gnu17"

[[targets]]
name = "app"
kind = "executable"
sources = ["src/**/*.c"]
exclude = ["src/platform/win32_*.c"]
include_private = ["src"]
defines = { APP_VERSION = "\"1.2.0\"", USE_COLOR = "1" }
deps = ["core", "m", "zlib"]

[[targets]]
name = "core"
kind = "static_library"
sources = ["lib/core/*.c"]
include_public = ["lib/core/include"]

[dependencies]
m = { source = "system", link = "m" }
zlib = { source = "system", pkg_config = "zlib" }
```

### Top-level fields

| Field | Default | Meaning |
|---|---|---|
| `std` | `"gnu17"` | C standard: `c89`, `c99`, `c11`, `c17`, `c23`, or their `gnu` variants |
| `schema` | `"cbuild-plan/1"` | Format version. You can leave it out |
| `strategy` | `"native"` | How the plan is run. Only `native` (cbuild's own engine) exists today |
| `targets` | | List of programs and libraries |
| `dependencies` | | System libraries that targets can use |

### Target fields

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Letters, digits, `_` and `-`. A program gets this file name |
| `kind` | yes | `executable` or `static_library` |
| `sources` | yes | Glob patterns of `.c` files, relative to the project root, e.g. `"src/**/*.c"` |
| `exclude` | | Glob patterns removed from `sources` |
| `include_private` | | Include folders used only to build this target |
| `include_public` | | Include folders also given to every target that depends on this one |
| `defines` | | Macros: `NAME = "value"` becomes `-DNAME=value`. `NAME = ""` becomes `-DNAME` |
| `deps` | | Names of other targets (libraries only) or of entries in `dependencies` |

When a program depends on a library, cbuild links the library and gives the program the library's `include_public` folders and system libraries. This also works through chains of libraries.

### Dependencies

Each entry is a system library, declared in exactly one of two ways:

| Form | Effect |
|---|---|
| `{ source = "system", link = "m" }` | Link with `-lm` |
| `{ source = "system", pkg_config = "zlib" }` | Ask `pkg-config` for the compile and link flags of `zlib` |

The library must already be installed on the machine. cbuild does not download libraries yet.

### Validation

cbuild never guesses. These all stop the build with a clear message:

- an unknown field, such as a misspelled name;
- a path outside the project (`../`, absolute paths);
- a pattern that matches no files, or a non-`.c` source;
- a source file claimed by two targets;
- a dependency that is neither a target nor a declared library, or a target that depends on an executable;
- a dependency cycle between libraries, e.g. `a -> b -> a`.

---

## The AI build engineer

### What it is for

The heuristics only handle simple layouts. Real projects have several programs, platform-specific files, required macros, bundled libraries, and build logic written in CMake, Make or Meson. The AI build engineer reads all of this and translates it into a `cbuild.toml`.

### AI providers

The AI build engineer runs through an agent CLI you have installed and logged in to, on **your own account and subscription**. cbuild never asks for an API key.

| Provider | Install | Status |
|---|---|---|
| Claude Code | [claude.com/claude-code](https://claude.com/claude-code) | ✅ supported |
| Codex | [github.com/openai/codex](https://github.com/openai/codex) | ✅ supported |
| Local models (Ollama, llama.cpp) | | planned: free, offline, code never leaves the machine |
| Anthropic API (API key) | | planned |

cbuild uses the first one installed, Claude Code first. To choose:

```sh
cbuild build --provider codex             # for one command
export CBUILD_AI_PROVIDER=codex           # for every command
```

On the test project used during development (a CMake project with a library, a program, Windows-only files, required macros and threads), both providers designed the same plan on the first build attempt, in about 20 seconds.

Any other agent that speaks MCP can also use cbuild's tools by hand: see [Using cbuild from other AI tools](#using-cbuild-from-other-ai-tools-mcp).

### How it works

1. cbuild starts the provider (Claude Code or Codex) in the background, on **your own account and subscription**.
2. The model can act **only through cbuild's tools**:

   | Tool | What it does |
   |---|---|
   | `list_files` | List the project's files |
   | `read_file` | Read a text file |
   | `grep` | Search for text in the project |
   | `try_build` | Build with a candidate plan and get back the classified errors |
   | `finish` | Submit the final plan, which must be one that `try_build` built successfully |

3. It tries plans until one builds. It has at most 15 build attempts.
4. cbuild checks the plan again, saves it as `cbuild.toml` and shows the model's notes explaining its choices.

When designing a plan from scratch, it usually reads the existing build files (for example `CMakeLists.txt`) and translates them. It leaves out tests, examples, tools and files meant for other platforms unless the main program needs them.

### When it runs

| Situation | What happens |
|---|---|
| No `cbuild.toml`, heuristics fail | `build`/`run` ask, then design a plan and continue the build |
| No `cbuild.toml`, the inferred plan fails with a missing header or symbol | `build`/`run` ask, then repair the plan and continue the build |
| No `cbuild.toml`, the project has its own build files (CMake, Make, configure, Meson) and the inferred plan fails in any way | same: that code compiles with its own build system, so the plan is at fault |
| `cbuild.toml` exists but no longer builds | you see a hint to run `cbuild fix`. A `cbuild.toml` is never changed without your request |
| The error is in your code (syntax, types…) | no AI: you get the compiler error with an explanation |
| No terminal (CI) and no `--yes` | cbuild stops and prints the question, never waiting for input |
| Claude Code not installed | a hint explains how to install it |

`cbuild fix` makes the **smallest change** that makes the plan build again. It prints the changed lines and keeps the previous plan in `target/cbuild-agent/previous-cbuild.toml`:

```
    Verified new plan built successfully, in 20s
note: Added lib/units.c to the wxcore library sources...
     Changes to the plan:
+     "lib/units.c",
     Updated cbuild.toml
```

If a failure comes from your source code rather than from the plan, the AI says where the error is instead of changing the plan.

`cbuild sync` is for a plan that **still builds** but no longer matches the project: a new program, a new source folder, changed build files. The AI compares the plan with the project, verifies its proposal with a real build, shows the changes and **asks before applying them** (`--yes` applies them directly):

```
$ cbuild sync
       Agent read_file CMakeLists.txt
       Agent try_build
       Build success
    Verified new plan built successfully, in 24s
     Changes proposed for the plan:
+ [[targets]]
+ name = "wxcsv"
+ kind = "executable"
+ sources = ["app/wxcsv.c"]
+ deps = ["wxcore"]
Apply these changes? [Y/n]
```

If the plan is already up to date, nothing changes. If it does not build, `sync` repairs it the way `fix` does.

### Privacy and safety

- cbuild asks before any project file is sent to the model. `--yes` skips the question.
- The model can read the project's text files. It can **never** read hidden files or folders (`.env`, `.git`, `.ssh`, …), files that look like secrets (`*.pem`, `*.key`, `id_rsa*`, `credentials*`, …), files outside the project, or symlinks that point outside it.
- The model **cannot change your files** and cannot run commands. The provider's own tools (shell, file editing, web, browser) are switched off, and Codex also runs in its read-only sandbox. The model can only change the plan and ask cbuild to build it. With Codex this was checked by asking the model to read `/etc/hosts`, list the home folder and write a file: every attempt was refused.
- The provider runs in an empty temporary folder **outside the project**, with your user configuration turned off. Agents load instruction files (`CLAUDE.md`, `AGENTS.md`) from their working folder and the folders above it, so a file like that inside the project could otherwise instruct the model. Checked with both providers: a project `CLAUDE.md`/`AGENTS.md` asking the model to write a word in its notes had no effect. The model can still read such files with `read_file`, as data, like any other project file.
- cbuild never reads or stores your credentials. It only starts the `claude` or `codex` command you have already logged in to.
- Building a plan compiles and links the code, but **does not run it**.

### Sessions

Each AI session is kept in `target/cbuild-agent/sessions/<timestamp>/`:

- `mcp.json` (Claude Code) or `codex.log` (Codex): how the provider was connected, and its own messages;
- `trace.jsonl`: every tool call with its input and output, one per line;
- `result.json`: the verified plan and the model's notes.

---

## Error messages

cbuild always shows the compiler's own output first, then adds a plain-language hint for common problems:

| Problem | Example hint |
|---|---|
| Missing header from a known library | `` `raylib.h` comes from the `raylib` library. Install it… then declare it in cbuild.toml: raylib = { source = "system", pkg_config = "raylib" } `` |
| Missing header, unknown | `` `foo.h` was not found. If it is part of your project, add its folder to `include_private`… `` |
| Function used without its `#include` | `` `puts` is used without its declaration: add `#include <stdio.h>` `` |
| Undefined symbol from a system library | `` `sqrt` lives in the system library `m`. Declare it in cbuild.toml… `` |
| Undefined symbol | `` `foo` is declared but never defined: the file defining it is not part of the target… `` |
| Symbol defined twice | `` `counter` is defined in more than one file: define it in a single .c file and declare it `extern`… `` |
| Several `main()` functions | `more than one file defines main(): each program needs its own target` |

Messages from Clang, GCC, GNU ld, LLVM lld and Apple's linker are recognized.

---

## Compiler, profiles and outputs

### Compiler

cbuild uses, in this order: the `CC` environment variable, then `cc`, `clang` or `gcc` from your `PATH`. Static libraries use `ar`, or the `AR` environment variable.

`CFLAGS`, `CPPFLAGS` and similar variables are **ignored on purpose**: the same plan gives the same build on every machine.

### Profiles

| Profile | Option | Flags |
|---|---|---|
| `dev` | default | `-O0 -g` |
| `release` | `--release` | `-O2 -DNDEBUG` |

Both add `-std=<std>`, `-Wall`, `-Werror=implicit-function-declaration` (calling an undeclared function is an error with every compiler, not only with Clang), and `-MMD`, which records header dependencies for future incremental builds.

### Where files go

```
project/
├── cbuild.toml                  # the plan (commit it)
├── compile_commands.json        # for clangd and editors (copied from target/)
└── target/
    ├── CACHEDIR.TAG             # marks the folder as cbuild's
    ├── debug/                   # dev builds: programs, lib<name>.a, obj/
    ├── release/                 # release builds
    └── cbuild-agent/            # AI sessions and trial builds
```

- `compile_commands.json` is written on every build, so clangd and editors understand the project at once. If one already exists and was made by another tool, cbuild leaves it alone and keeps its copy in `target/`.
- cbuild only writes into a `target/` folder it created itself. If your project already has an unrelated `target/` folder, cbuild stops instead of mixing files. `cbuild clean` only deletes a `target/` folder that cbuild created.
- Single files (`cbuild run hello.c`) build into `~/.cbuild/cache/single/`.

Set `NO_COLOR=1` to turn off colors.

---

## Exporting to CMake or Make

cbuild never locks you in. Any plan, written by hand, inferred or designed by the AI, can be turned into a regular build file:

```sh
cbuild export cmake          # writes CMakeLists.txt
cbuild export make           # writes Makefile
```

Use it to hand the project to someone who uses CMake, to open it in an IDE that needs CMake (CLion, Visual Studio), or to package it for a Linux distribution.

| In `cbuild.toml` | In the generated CMake |
|---|---|
| `executable` / `static_library` | `add_executable` / `add_library(... STATIC)` |
| `sources` (globs) | the explicit list of files, as found when exporting |
| `include_public` / `include_private` | `target_include_directories(PUBLIC / PRIVATE)` |
| `defines` | `target_compile_definitions(PRIVATE ...)` |
| `deps` on targets and `link` libraries | `target_link_libraries`, `PUBLIC` for libraries so they reach the programs using them |
| `pkg_config` libraries | `pkg_check_modules(... IMPORTED_TARGET ...)` |
| `std = "gnu17"` | `CMAKE_C_STANDARD 17` with `CMAKE_C_EXTENSIONS ON` |

The Makefile builds the same way, with incremental builds and header dependencies:

```sh
make              # debug build in build/debug/
make RELEASE=1    # optimized build in build/release/
make clean
```

Good to know:

- **Your files are safe.** If a `CMakeLists.txt` or `Makefile` already exists and cbuild did not generate it, `export` stops. Write elsewhere with `--output` (for example `--output cbuild-export/CMakeLists.txt` or `--output cbuild.mk`), or replace it with `--force`. A file cbuild generated is simply regenerated.
- **Sources are listed explicitly**, so the generated file always builds the same thing. After adding files, export again.
- **The generated Makefile respects `CC`, `CFLAGS` and `LDFLAGS`** from the environment, as Makefiles usually do. cbuild itself ignores them.
- **Paths with spaces or with `$ # % : '` cannot go into a Makefile.** cbuild refuses them; the CMake export accepts them.

## Using cbuild from other AI tools (MCP)

`cbuild mcp` serves the same tools (`list_files`, `read_file`, `grep`, `try_build`, `finish`) over the [Model Context Protocol](https://modelcontextprotocol.io). Any MCP client can use cbuild this way, including an interactive Claude Code session:

```sh
claude mcp add cbuild -- cbuild mcp
```

When a client calls `finish` with a verified plan, cbuild saves it as `cbuild.toml`. It never overwrites an existing one.

---

## Limitations

What is missing today, roughly in the order it is planned:

- **No incremental builds.** Every build recompiles every file. Fine for small projects, slow for big ones.
- **Only `native` builds.** cbuild cannot run CMake, Meson, Make or autotools yet. The AI translates them into a native plan instead, which does not work when a project needs generated files such as a `config.h` produced by `configure`.
- **Only executables and static libraries.** Shared libraries come later.
- **No per-platform sections** in `cbuild.toml`. A plan is written for the platform it was made on.
- **No `cbuild test`** and no `cbuild install`.
- **No package downloads.** Libraries must already be installed on the system.
- **AI providers.** Claude Code and Codex only. Local models (Ollama, llama.cpp) and the Anthropic API are planned.
- **macOS only tested.** Linux is expected to work. Windows is not supported.
- **C only.** No C++.

What already works is summarized in [Status](#status).

---

## Development

```sh
cargo build
cargo test                      # unit tests + end-to-end tests with the system compiler
cargo clippy --all-targets
cargo fmt
```

### Corpus

`corpus/corpus.toml` lists 20 real C projects of increasing difficulty (from `kilo` to `redis`), each pinned to a commit. `corpus/run.py` measures how many of them cbuild builds with no help:

```sh
python3 -I corpus/run.py              # heuristics, then the AI where they fail (default provider)
python3 -I corpus/run.py --no-ai      # heuristics only: fast and free
python3 -I corpus/run.py --only lua   # some projects
python3 -I corpus/run.py --provider codex   # measure another AI provider
```

Projects are checked out in `target/corpus/` and compiled, never run. Reports go to `corpus/results/`.

### Releasing

Releases are published by `.github/workflows/release.yml` when a version tag is pushed:

1. Set the new version in `Cargo.toml`, then run `cargo build` so `Cargo.lock` follows.
2. Commit: `git commit -am "Release 0.2.0"`.
3. Tag and push: `git tag -a v0.2.0 -m "cbuild 0.2.0" && git push origin main v0.2.0`.

The workflow then:

- checks that the tag matches the version in `Cargo.toml`;
- runs format, lint and tests on Linux and macOS;
- builds four binaries: static Linux x86_64 and ARM64, macOS ARM64 and Intel;
- publishes a GitHub release with the archives, their SHA-256 checksums and notes generated from the commits.

If any step fails, nothing is published: fix the problem, delete the tag (`git tag -d v0.2.0 && git push origin :v0.2.0`) and tag again. "Run workflow" in the Actions tab does a dry run: it checks and builds without publishing. `cbuild update` finds new releases through the same tags.

### Tests

The end-to-end tests build real C projects with your compiler. They never start a real AI session: the tests that cover AI behavior use fake `claude` and `codex` commands that fail the test if they are ever run.

Source layout:

| Path | Role |
|---|---|
| `src/main.rs` | Command line |
| `src/plan.rs` | Plan format and validation |
| `src/infer.rs` | Heuristic planner |
| `src/scaffold.rs` | `cbuild new` / `cbuild init` templates and `.gitignore` |
| `src/selfupdate.rs` | `cbuild update` |
| `src/export.rs` | `cbuild export`: CMake and Make generators |
| `src/scan.rs` | Light C scanning (finding `main()`) |
| `src/engine.rs` | Compiling, archiving, linking, `compile_commands.json` |
| `src/toolchain.rs` | Compiler detection and flags |
| `src/diagnostics.rs` | Recognizing compiler and linker errors, and the hints |
| `src/ai/tools.rs` | The tools the AI acts through |
| `src/ai/mcp.rs` | MCP server |
| `src/ai/agent.rs` | What both providers share: session, result, progress |
| `src/ai/claude_code.rs` | Running Claude Code |
| `src/ai/codex.rs` | Running Codex |
| `src/ai/prompt.md` | Instructions given to the model |
