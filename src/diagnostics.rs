//! Classifies raw compiler and linker output and explains it in plain language.
//! The raw output is always shown first; these hints come on top of it.

use crate::ui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    MissingHeader(String),
    UndeclaredFunction(String),
    UndefinedSymbol(String),
    DuplicateSymbol(String),
}

/// Well-known library headers and the pkg-config package that provides them.
const HEADER_PACKAGES: &[(&str, &str)] = &[
    ("raylib.h", "raylib"),
    ("SDL2/SDL.h", "sdl2"),
    ("SDL3/SDL.h", "sdl3"),
    ("GLFW/glfw3.h", "glfw3"),
    ("curl/curl.h", "libcurl"),
    ("zlib.h", "zlib"),
    ("png.h", "libpng"),
    ("openssl/ssl.h", "openssl"),
    ("sqlite3.h", "sqlite3"),
    ("ncurses.h", "ncurses"),
    ("uv.h", "libuv"),
    ("jansson.h", "jansson"),
];

/// Standard header declaring common library functions.
const FUNCTION_HEADERS: &[(&str, &[&str])] = &[
    (
        "stdio.h",
        &[
            "printf", "puts", "putchar", "fprintf", "sprintf", "snprintf", "scanf", "fopen",
            "fclose", "fgets", "fread", "fwrite", "getchar", "perror",
        ],
    ),
    (
        "stdlib.h",
        &[
            "malloc", "calloc", "realloc", "free", "exit", "atoi", "atof", "strtol", "rand",
            "srand", "qsort", "abs", "getenv",
        ],
    ),
    (
        "string.h",
        &[
            "strlen", "strcmp", "strncmp", "strcpy", "strncpy", "strcat", "strchr", "strstr",
            "strdup", "memcpy", "memmove", "memset", "memcmp",
        ],
    ),
    (
        "math.h",
        &[
            "sqrt", "pow", "sin", "cos", "tan", "atan2", "floor", "ceil", "fabs", "fmod", "log",
            "log10", "exp", "round",
        ],
    ),
    (
        "ctype.h",
        &[
            "isdigit", "isalpha", "isalnum", "isspace", "isupper", "islower", "toupper", "tolower",
        ],
    ),
    (
        "time.h",
        &["time", "clock", "difftime", "strftime", "localtime"],
    ),
    (
        "unistd.h",
        &["sleep", "usleep", "read", "write", "close", "getpid"],
    ),
];

/// Symbols living in system libraries that are not linked by default everywhere.
const SYMBOL_LIBRARIES: &[(&str, &[&str])] = &[
    (
        "m",
        &[
            "sqrt", "sqrtf", "pow", "powf", "sin", "sinf", "cos", "cosf", "tan", "tanf", "asin",
            "acos", "atan", "atan2", "floor", "ceil", "fabs", "fmod", "log", "log2", "log10",
            "exp", "exp2", "round", "lround", "trunc", "hypot", "cbrt", "fmin", "fmax",
        ],
    ),
    ("dl", &["dlopen", "dlsym", "dlclose", "dlerror"]),
];

impl Diagnostic {
    /// Stable machine-readable name of the problem.
    pub fn kind(&self) -> &'static str {
        match self {
            Diagnostic::MissingHeader(_) => "missing_header",
            Diagnostic::UndeclaredFunction(_) => "undeclared_function",
            Diagnostic::UndefinedSymbol(_) => "undefined_symbol",
            Diagnostic::DuplicateSymbol(_) => "duplicate_symbol",
        }
    }

    /// The header or symbol the problem is about.
    pub fn subject(&self) -> &str {
        match self {
            Diagnostic::MissingHeader(name)
            | Diagnostic::UndeclaredFunction(name)
            | Diagnostic::UndefinedSymbol(name)
            | Diagnostic::DuplicateSymbol(name) => name,
        }
    }
}

pub fn classify(output: &str) -> Vec<Diagnostic> {
    let text = strip_ansi(output);
    let mut found = Vec::new();
    // ld64 (macOS) lists undefined symbols on the lines after this header.
    let mut in_undefined_list = false;
    for line in text.lines() {
        let diagnostic = if let Some(header) = missing_header(line) {
            Some(Diagnostic::MissingHeader(header))
        } else if let Some(name) = name_after(
            line,
            &[
                "call to undeclared function ",
                "implicit declaration of function ",
            ],
        ) {
            Some(Diagnostic::UndeclaredFunction(name))
        } else if let Some(name) =
            name_after(line, &["undefined reference to ", "undefined symbol: "])
        {
            Some(Diagnostic::UndefinedSymbol(name))
        } else if let Some(name) =
            name_after(line, &["multiple definition of ", "duplicate symbol: "])
        {
            Some(Diagnostic::DuplicateSymbol(name))
        } else if let Some(name) = name_after(line, &["duplicate symbol '"]) {
            Some(Diagnostic::DuplicateSymbol(unmangle(name)))
        } else if in_undefined_list && line.trim_start().starts_with('"') {
            name_after(line, &["\""]).map(|name| Diagnostic::UndefinedSymbol(unmangle(name)))
        } else {
            None
        };
        if line.starts_with("Undefined symbols for architecture") {
            in_undefined_list = true;
        } else if !line.starts_with(' ') {
            in_undefined_list = false;
        }
        if let Some(diagnostic) = diagnostic.filter(|d| !found.contains(d)) {
            found.push(diagnostic);
        }
    }
    found
}

pub fn hint(diagnostic: &Diagnostic) -> String {
    match diagnostic {
        Diagnostic::MissingHeader(header) => match lookup_pair(HEADER_PACKAGES, header) {
            Some(package) => format!(
                "`{header}` comes from the `{package}` library. Install it with your system package \
                 manager, then declare it in cbuild.toml:\n    [dependencies]\n    {package} = \
                 {{ source = \"system\", pkg_config = \"{package}\" }}\n  and add \"{package}\" to the target's deps"
            ),
            None => format!(
                "`{header}` was not found. If it is part of your project, add its folder to \
                 `include_private`; if it comes from a library, install the library and declare it \
                 as a dependency"
            ),
        },
        Diagnostic::UndeclaredFunction(function) => {
            match lookup_group(FUNCTION_HEADERS, function) {
                Some(header) => format!(
                    "`{function}` is used without its declaration: add `#include <{header}>` at the top of the file"
                ),
                None => format!(
                    "`{function}` is used before it is declared: include the header that declares it, \
                 or declare it above its first use"
                ),
            }
        }
        Diagnostic::UndefinedSymbol(symbol) if symbol == "main" => {
            "the program has no main() function".to_string()
        }
        Diagnostic::UndefinedSymbol(symbol) => match lookup_group(SYMBOL_LIBRARIES, symbol) {
            Some(library) => format!(
                "`{symbol}` lives in the system library `{library}`. Declare it in cbuild.toml:\n    \
                 [dependencies]\n    {library} = {{ source = \"system\", link = \"{library}\" }}\n  \
                 and add \"{library}\" to the target's deps"
            ),
            None => format!(
                "`{symbol}` is declared but never defined: the file defining it is not part of the \
                 target, or it comes from a library that is not linked"
            ),
        },
        Diagnostic::DuplicateSymbol(symbol) if symbol == "main" => {
            "more than one file defines main(): each program needs its own target".to_string()
        }
        Diagnostic::DuplicateSymbol(symbol) => format!(
            "`{symbol}` is defined in more than one file: define it in a single .c file and declare \
             it `extern` in the header, or make each copy `static`"
        ),
    }
}

/// True if `output` shows a problem a different plan can solve (missing include path,
/// source or library), as opposed to an error in the code itself.
pub fn plan_related(output: &str) -> bool {
    classify(output).iter().any(|d| {
        matches!(
            d,
            Diagnostic::MissingHeader(_)
                | Diagnostic::UndefinedSymbol(_)
                | Diagnostic::DuplicateSymbol(_)
        )
    })
}

/// Prints a hint for every recognized problem in `output`.
pub fn report(output: &str) {
    for diagnostic in classify(output) {
        ui::hint(hint(&diagnostic));
    }
}

fn missing_header(line: &str) -> Option<String> {
    let rest = &line[line.find("fatal error: ")? + "fatal error: ".len()..];
    // Clang: 'foo.h' file not found
    if rest.ends_with("file not found") {
        return name_after(rest, &[""]);
    }
    // GCC: foo.h: No such file or directory
    rest.strip_suffix(": No such file or directory")
        .map(str::to_string)
}

fn is_quote(c: char) -> bool {
    matches!(c, '\'' | '`' | '"' | '‘' | '’')
}

/// The identifier following the first of `prefixes` found in `line`, without quotes.
fn name_after(line: &str, prefixes: &[&str]) -> Option<String> {
    let rest = prefixes
        .iter()
        .find_map(|prefix| line.find(prefix).map(|at| &line[at + prefix.len()..]))?;
    let name: String = rest
        .trim_start_matches(is_quote)
        .chars()
        .take_while(|&c| !is_quote(c) && !c.is_whitespace() && c != ';' && c != ',')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// ld64 prints C symbols with a leading underscore.
fn unmangle(name: String) -> String {
    name.strip_prefix('_').map(str::to_string).unwrap_or(name)
}

fn lookup_pair<'a>(table: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn lookup_group<'a>(table: &[(&'a str, &[&str])], item: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(_, items)| items.contains(&item))
        .map(|(group, _)| *group)
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip a CSI sequence: ESC [ params final-byte.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Diagnostic::*;
    use super::*;

    #[test]
    fn missing_headers() {
        assert_eq!(
            classify("src/main.c:1:10: fatal error: 'raylib.h' file not found"),
            [MissingHeader("raylib.h".into())]
        );
        assert_eq!(
            classify("main.c:1:10: fatal error: SDL2/SDL.h: No such file or directory"),
            [MissingHeader("SDL2/SDL.h".into())]
        );
    }

    #[test]
    fn undeclared_functions() {
        assert_eq!(
            classify(
                "a.c:2:5: error: call to undeclared function 'puts'; ISO C99 and later do not support"
            ),
            [UndeclaredFunction("puts".into())]
        );
        assert_eq!(
            classify(
                "a.c:2:5: error: implicit declaration of function ‘puts’ [-Wimplicit-function-declaration]"
            ),
            [UndeclaredFunction("puts".into())]
        );
    }

    #[test]
    fn linker_errors_from_gnu_ld_lld_and_ld64() {
        assert_eq!(
            classify(
                "main.c:(.text+0x15): undefined reference to `sqrt'\nmain.c:(.text+0x25): undefined reference to `sqrt'"
            ),
            [UndefinedSymbol("sqrt".into())]
        );
        assert_eq!(
            classify("ld.lld: error: undefined symbol: foo"),
            [UndefinedSymbol("foo".into())]
        );
        assert_eq!(
            classify(
                "Undefined symbols for architecture arm64:\n  \"_foo\", referenced from:\n      _main in main.o\nld: symbol(s) not found"
            ),
            [UndefinedSymbol("foo".into())]
        );
        assert_eq!(
            classify("duplicate symbol '_main' in:\n    a.o\n    b.o"),
            [DuplicateSymbol("main".into())]
        );
        assert_eq!(
            classify("/usr/bin/ld: b.o: multiple definition of `counter'; a.o: first defined here"),
            [DuplicateSymbol("counter".into())]
        );
    }

    #[test]
    fn colored_output_is_understood() {
        assert_eq!(
            classify(
                "\x1b[1mmain.c:1:10: \x1b[0;1;31mfatal error: \x1b[0m\x1b[1m'zlib.h' file not found\x1b[0m"
            ),
            [MissingHeader("zlib.h".into())]
        );
    }

    #[test]
    fn separates_plan_problems_from_code_errors() {
        assert!(plan_related(
            "main.c:1:10: fatal error: 'zlib.h' file not found"
        ));
        assert!(plan_related("ld.lld: error: undefined symbol: foo"));
        assert!(!plan_related(
            "main.c:3:5: error: expected ';' after expression"
        ));
        assert!(!plan_related(
            "a.c:2:5: error: call to undeclared function 'puts'"
        ));
    }

    #[test]
    fn hints_name_the_fix() {
        assert!(hint(&MissingHeader("raylib.h".into())).contains("pkg_config = \"raylib\""));
        assert!(hint(&UndeclaredFunction("printf".into())).contains("#include <stdio.h>"));
        assert!(hint(&UndefinedSymbol("sqrt".into())).contains("link = \"m\""));
    }
}
