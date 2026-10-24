//! Lightweight lexical analysis of C sources. Not a parser: just enough to answer
//! questions like "does this file define main()?" without a compiler.

/// Whether and how a file defines `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Main {
    None,
    /// Compiled in every build, possibly depending on the platform.
    Always,
    /// Only compiled when a build option is set (`#ifdef TEST`, `#if 0`, ...):
    /// a test or tool entry point, not the program.
    Optional,
}

/// Macros that select a platform: code under them is not optional, just platform-specific.
const PLATFORM_MACROS: &[&str] = &[
    "_WIN32",
    "_WIN64",
    "WIN32",
    "_MSC_VER",
    "__MINGW",
    "__CYGWIN__",
    "__unix",
    "__linux",
    "__APPLE__",
    "__MACH__",
    "__FreeBSD__",
    "__OpenBSD__",
    "__NetBSD__",
    "__ANDROID__",
    "__EMSCRIPTEN__",
    "_POSIX",
];

/// Marker bytes left in the stripped code where conditional directives were.
const IF: u8 = 1;
const ELSE: u8 = 2;
const ENDIF: u8 = 3;

/// Code with comments, string/char literals and preprocessor lines blanked out
/// (newlines kept, conditional directives replaced by markers), plus, per byte,
/// whether it sits in an optional `#if` block.
struct Stripped {
    code: Vec<u8>,
    optional: Vec<bool>,
}

fn strip(src: &[u8]) -> Stripped {
    let mut code = Vec::with_capacity(src.len());
    let mut optional = Vec::with_capacity(src.len());
    // One entry per open #if: true if its condition is about the platform.
    let mut conditions: Vec<bool> = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    let mut push = |code: &mut Vec<u8>, conditions: &[bool], c: u8| {
        code.push(c);
        optional.push(conditions.iter().any(|platform| !platform));
    };
    while i < src.len() {
        let c = src[i];
        match c {
            b'\n' => {
                push(&mut code, &conditions, b'\n');
                line_start = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                push(&mut code, &conditions, b' ');
                i += 1;
                continue;
            }
            b'#' if line_start => {
                // Preprocessor directive, including backslash-continued lines.
                let start = i;
                while i < src.len() && src[i] != b'\n' {
                    if src[i] == b'\\' {
                        i += 1;
                        if src.get(i) == Some(&b'\r') {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                let line = String::from_utf8_lossy(&src[start + 1..i]);
                let directive = line.trim_start();
                let platform = PLATFORM_MACROS.iter().any(|m| directive.contains(m));
                if directive.starts_with("if") {
                    conditions.push(platform);
                    push(&mut code, &conditions, IF);
                } else if directive.starts_with("endif") {
                    push(&mut code, &conditions, ENDIF);
                    conditions.pop();
                } else if directive.starts_with("el") {
                    if directive.starts_with("elif")
                        && let Some(top) = conditions.last_mut()
                    {
                        *top = *top || platform;
                    }
                    push(&mut code, &conditions, ELSE);
                }
                continue;
            }
            b'/' if src.get(i + 1) == Some(&b'/') => {
                while i < src.len() && src[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < src.len() && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                    if src[i] == b'\n' {
                        push(&mut code, &conditions, b'\n');
                    }
                    i += 1;
                }
                i += 2;
                push(&mut code, &conditions, b' ');
                continue;
            }
            b'"' | b'\'' => {
                i += 1;
                while i < src.len() && src[i] != c && src[i] != b'\n' {
                    if src[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                push(&mut code, &conditions, b' ');
                line_start = false;
                continue;
            }
            _ => {}
        }
        line_start = false;
        push(&mut code, &conditions, c);
        i += 1;
    }
    Stripped { code, optional }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// True if `rest` (the text right after an identifier) is a parameter list
/// followed by a function body.
fn is_function_body(rest: &[u8]) -> bool {
    let mut i = rest.iter().take_while(|c| c.is_ascii_whitespace()).count();
    if rest.get(i) != Some(&b'(') {
        return false;
    }
    let mut depth = 0;
    while i < rest.len() {
        match rest[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let after = &rest[(i + 1).min(rest.len())..];
    after.iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'{')
}

/// Whether the source defines a `main` function at file scope, and under which conditions.
pub fn main_definition(src: &[u8]) -> Main {
    let Stripped { code, optional } = strip(src);
    let mut found = Main::None;
    let mut depth = 0i32;
    // Per open #if: brace depth at the #if, and at the end of its first branch.
    // Alternative branches (`#if A { #else { #endif`) each start from the depth at
    // the #if, so their braces are not counted twice.
    let mut branches: Vec<(i32, Option<i32>)> = Vec::new();
    let mut prev_word: Option<&[u8]> = None;
    let mut i = 0;
    while i < code.len() {
        let c = code[i];
        if is_ident_start(c) {
            let start = i;
            while i < code.len() && is_ident(code[i]) {
                i += 1;
            }
            let word = &code[start..i];
            // A definition is preceded by its return type: `int main(...) {`.
            let after_type = prev_word.is_some_and(|w| w != b"return");
            if depth == 0 && word == b"main" && after_type && is_function_body(&code[i..]) {
                if !optional[start] {
                    return Main::Always;
                }
                found = Main::Optional;
            }
            prev_word = Some(word);
            continue;
        }
        match c {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            IF => branches.push((depth, None)),
            ELSE => {
                if let Some((at_if, first_end)) = branches.last_mut() {
                    first_end.get_or_insert(depth);
                    depth = *at_if;
                }
            }
            ENDIF => {
                if let Some((_, Some(first_end))) = branches.pop() {
                    depth = first_end;
                }
            }
            _ => {}
        }
        if !c.is_ascii_whitespace() {
            prev_word = None;
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{Main, main_definition};

    #[test]
    fn finds_common_main_definitions() {
        assert_eq!(
            main_definition(b"int main(void) { return 0; }"),
            Main::Always
        );
        assert_eq!(
            main_definition(b"int\nmain(int argc, char **argv)\n{\n}"),
            Main::Always
        );
        assert_eq!(
            main_definition(b"#include <stdio.h>\nstatic int x;\nint main() { puts(\"hi\"); }"),
            Main::Always
        );
    }

    #[test]
    fn ignores_things_that_are_not_definitions() {
        for src in [
            &b"int main(void);"[..],
            b"// int main(void) {}\n",
            b"/* int main(void) {} */",
            b"const char *s = \"int main(void) {}\";",
            b"#define main SDL_main\nint helper(void) { return 1; }",
            b"int f(void) { return main(); }",
            b"int f(int main) { return main; }",
        ] {
            assert_eq!(
                main_definition(src),
                Main::None,
                "{}",
                String::from_utf8_lossy(src)
            );
        }
    }

    #[test]
    fn alternative_branches_do_not_unbalance_braces() {
        let src = b"#ifdef X\nstatic void f(int a) {\n#else\nstatic void f(void) {\n#endif\n}\nint main(void) { return 0; }\n";
        assert_eq!(main_definition(src), Main::Always);
    }

    #[test]
    fn main_behind_a_build_option_is_optional() {
        let test_main = b"int f(void) { return 1; }\n#ifdef TESTS_MAIN\nint main(void) { return f(); }\n#endif\n";
        assert_eq!(main_definition(test_main), Main::Optional);
        assert_eq!(
            main_definition(b"#if 0\nint main(void) { return 0; }\n#endif\n"),
            Main::Optional
        );
    }

    #[test]
    fn main_behind_a_platform_check_is_the_program() {
        let src = b"#ifdef _WIN32\nint wmain(void) { return 0; }\n#else\nint main(void) { return 0; }\n#endif\n";
        assert_eq!(main_definition(src), Main::Always);
        let nested = b"#if defined(__linux__)\n#  ifdef DEBUG\n#  endif\nint main(void) { return 0; }\n#endif\n";
        assert_eq!(main_definition(nested), Main::Always);
    }
}
