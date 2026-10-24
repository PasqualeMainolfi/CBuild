# cbuild corpus — 2026-10-24-heuristics

- Built: **7/20** (heuristics 7, AI 0), failed 13
- Built with every expected target: **7/20**

| Project | Level | Result | Expected targets | Build attempts | Time | Notes |
|---|---|---|---|---|---|---|
| kilo | easy | ✅ heuristics | all | — | 0s |  |
| linenoise | easy | ✅ heuristics | all | — | 0s |  |
| parson | easy | ✅ heuristics | all | — | 0s |  |
| tiny-aes | easy | ✅ heuristics | all | — | 0s |  |
| log.c | easy | ✅ heuristics | all | — | 0s |  |
| inih | easy | ✅ heuristics | all | — | 0s |  |
| cjson | medium | ✅ heuristics | all | — | 0s |  |
| zlib | medium | ❌ | — | — | 0s | hint: `read` is used without its declaration: add `#include <unistd.h>` at the top of the file / hint: `close` is used without its declaration: add `#include <unistd.h>` at the top of the file / hint: `write` is used without its declaration: add `#include <unistd.h>` at the top of the file / error: could not compile `zlib` |
| lua | medium | ❌ | — | — | 0s | hint: `lua_closethread` is defined in more than one file: define it in a single .c file and declare it `extern` in the header, or make each copy `static` / hint: `luaL_tolstring` is defined in more than one file: define it in a single .c file and declare it `extern` in the header, or make each copy `static` / hint: `lua_arith` is defined in more than one file: define it in a single .c file and dec |
| libyaml | medium | ❌ | — | — | 0s |    23 \|     *patch = YAML_VERSION_PATCH; /       \|              ^~~~~~~~~~~~~~~~~~ / 4 errors generated. / error: could not compile `libyaml` |
| lz4 | medium | ❌ | — | — | 0s | error: no C source files found (*.c). To build a single file run `cbuild run <file.c>` |
| wren | medium | ❌ | — | — | 0s | 1 error generated. / hint: `wren_common.h` was not found. If it is part of your project, add its folder to `include_private`; if it comes from a library, install the library and declare it as a dependency / hint: `wren.h` was not found. If it is part of your project, add its folder to `include_private`; if it comes from a library, install the library and declare it as a dependency / error: could n |
| tree-sitter | medium | ❌ | — | — | 0s | error: no C source files found (*.c). To build a single file run `cbuild run <file.c>` |
| nnn | medium | ❌ | — | — | 0s | hint: `wrefresh$NCURSES60` is declared but never defined: the file defining it is not part of the target, or it comes from a library that is not linked / hint: `write_history` is declared but never defined: the file defining it is not part of the target, or it comes from a library that is not linked / hint: `wtimeout` is declared but never defined: the file defining it is not part of the target, o |
| zstd | hard | ❌ | — | — | 0s | error: no C source files found (*.c). To build a single file run `cbuild run <file.c>` |
| libuv | hard | ❌ | — | — | 0s | hint: `windows.h` was not found. If it is part of your project, add its folder to `include_private`; if it comes from a library, install the library and declare it as a dependency / hint: `direct.h` was not found. If it is part of your project, add its folder to `include_private`; if it comes from a library, install the library and declare it as a dependency / hint: `io.h` was not found. If it is  |
| htop | hard | ❌ | — | — | 0s | error: 2 files define main(): htop.c, pcp-htop.c. / Run one of them with `cbuild run <file.c>`, or declare one target per program in cbuild.toml |
| quickjs | hard | ❌ | — | — | 0s | error: 5 files define main(): ctest.c, qjs.c, qjsc.c, run-test262.c, unicode_gen.c. / Run one of them with `cbuild run <file.c>`, or declare one target per program in cbuild.toml |
| raylib | hard | ❌ | — | — | 1s | hint: `getchar` is used without its declaration: add `#include <stdio.h>` at the top of the file / hint: `TRACELOG` is used before it is declared: include the header that declares it, or declare it above its first use / hint: `emscripten/emscripten.h` was not found. If it is part of your project, add its folder to `include_private`; if it comes from a library, install the library and declare it as |
| redis | hard | ❌ | — | — | 0s | error: 2 files define main(): src/redis-cli.c, src/server.c. / Run one of them with `cbuild run <file.c>`, or declare one target per program in cbuild.toml |
