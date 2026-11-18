//! Whether every sentence a person can be shown is a message of the template, so that a language file can translate it. The
//! window's sources are read whole: any literal that reads as words is a message unless it is a name. The owner's sources are
//! read where they hand the window a sentence (a refusal, an error, a reason), because most of what they hold is not for a
//! person. Comments, macros that only log or format and test code are not read.
use std::path::{Path, PathBuf};

/// One string literal: its text, and the code written before it with comments and other literals blanked out.
struct Literal {
    text: String,
    before: String,
    in_skipped_macro: bool,
}

/// Macros whose literals no person sees through the window: logs, panics, assertions and formatting written to a stream.
const SKIPPED_MACROS: &[&str] = &[
    "dev_trace",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "eprintln",
    "println",
    "eprint",
    "print",
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "write",
    "writeln",
    "format",
    "format_args",
    "concat",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "matches",
    "vec",
];

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('u') => {
                let code: String = chars.by_ref().skip(1).take_while(|&c| c != '}').collect();
                if let Some(c) = u32::from_str_radix(&code, 16).ok().and_then(char::from_u32) {
                    out.push(c);
                }
            }
            Some('\n') => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The literals of `source`, in order. A small lexer: line and nested block comments, plain, byte and raw strings, character
/// literals told apart from lifetimes, and macro calls by their bracket depth.
fn literals(source: &str) -> Vec<Literal> {
    let bytes: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    let mut code = String::new();
    // Open macro calls: (name, depth of brackets inside it).
    let mut macros: Vec<(String, usize)> = Vec::new();
    let mut i = 0;
    let at = |i: usize| bytes.get(i).copied().unwrap_or('\0');
    while i < bytes.len() {
        let c = bytes[i];
        if c == '/' && at(i + 1) == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            while i < bytes.len() {
                if bytes[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            code.push(' ');
            continue;
        }
        // A raw string: r"…", r#"…"#, br"…".
        let raw_start = if c == 'r' && (at(i + 1) == '"' || at(i + 1) == '#') {
            Some(i + 1)
        } else if c == 'b' && at(i + 1) == 'r' && (at(i + 2) == '"' || at(i + 2) == '#') {
            Some(i + 2)
        } else {
            None
        };
        let word_before = i > 0 && (bytes[i - 1].is_alphanumeric() || bytes[i - 1] == '_');
        if let Some(mut j) = raw_start.filter(|_| !word_before) {
            let mut hashes = 0;
            while at(j) == '#' {
                hashes += 1;
                j += 1;
            }
            if at(j) == '"' {
                let start = j + 1;
                let mut end = start;
                while end < bytes.len() {
                    if bytes[end] == '"' && (0..hashes).all(|k| at(end + 1 + k) == '#') {
                        break;
                    }
                    end += 1;
                }
                record(
                    &mut found,
                    &code,
                    &macros,
                    bytes[start..end.min(bytes.len())].iter().collect(),
                );
                code.push_str("\"\"");
                i = end + 1 + hashes;
                continue;
            }
        }
        if c == '"' || (c == 'b' && at(i + 1) == '"' && !word_before) {
            let start = if c == '"' { i + 1 } else { i + 2 };
            let mut end = start;
            while end < bytes.len() && bytes[end] != '"' {
                if bytes[end] == '\\' {
                    end += 1;
                }
                end += 1;
            }
            let raw: String = bytes[start..end.min(bytes.len())].iter().collect();
            record(&mut found, &code, &macros, unescape(&raw));
            code.push_str("\"\"");
            i = end + 1;
            continue;
        }
        if c == '\'' {
            // A character literal is '\…' or one character and a closing quote; anything else is a lifetime.
            let end = if at(i + 1) == '\\' {
                (i + 3..bytes.len()).find(|&k| bytes[k] == '\'')
            } else if at(i + 2) == '\'' {
                Some(i + 2)
            } else {
                None
            };
            if let Some(end) = end {
                code.push_str("' '");
                i = end + 1;
                continue;
            }
        }
        if c == '!' && matches!(at(i + 1), '(' | '[' | '{') {
            let name: String = code
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            macros.push((name, 0));
            code.push('!');
            i += 1;
            continue;
        }
        if matches!(c, '(' | '[' | '{') {
            if let Some((_, depth)) = macros.last_mut() {
                *depth += 1;
            }
        } else if matches!(c, ')' | ']' | '}')
            && let Some((_, depth)) = macros.last_mut()
        {
            *depth -= 1;
            if *depth == 0 {
                macros.pop();
            }
        }
        code.push(c);
        i += 1;
    }
    found
}

fn record(found: &mut Vec<Literal>, code: &str, macros: &[(String, usize)], text: String) {
    found.push(Literal {
        text,
        before: code[code.char_indices().rev().nth(159).map_or(0, |(at, _)| at)..]
            .trim_end()
            .to_owned(),
        in_skipped_macro: macros
            .iter()
            .any(|(name, _)| SKIPPED_MACROS.contains(&name.as_str())),
    });
}

/// Production code only: every item compiled only for tests or for the development builds (`#[cfg(test)]`, and a `cfg`
/// naming the `demo` or `headless` feature without `not`) taken out wherever it stands (a test module, a test-only
/// `use`, a helper, the extension preview), and everything around and after it kept. Cutting each file at its first test
/// item dropped every line below an early test-only `use` or the first inline test module: most of `view.rs` was never read.
fn production(path: &Path) -> String {
    without_test_items(&std::fs::read_to_string(path).expect("a source file reads"))
}

fn without_test_items(source: &str) -> String {
    const MARK: &str = "#[cfg(";
    let development = |attribute: &str| {
        attribute == "#[cfg(test)]"
            || ["feature = \"demo\"", "feature = \"headless\""]
                .iter()
                .any(|feature| {
                    attribute.contains(feature) && !attribute.contains(&format!("not({feature})"))
                })
    };
    let mut kept = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find(MARK) {
        let Some(close) = rest[at..].find(']') else {
            break;
        };
        let attribute = &rest[at..=at + close];
        if development(attribute) {
            kept.push_str(&rest[..at]);
            rest = &rest[at + attribute.len()..];
            rest = &rest[item_end(rest)..];
        } else {
            kept.push_str(&rest[..at + attribute.len()]);
            rest = &rest[at + attribute.len()..];
        }
    }
    kept.push_str(rest);
    kept
}

/// Where the item at the start of `text` ends: after the `;` that closes it, or the brace that closes its body (and a `;`
/// right after, as a grouped `use` has). Strings, characters and comments are stepped over, so a brace written in one does
/// not count; a lifetime (`'a`) is not a character.
fn item_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let (mut i, mut depth) = (0, 0_usize);
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |end| i + end);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| i + 2 + end + 2);
                continue;
            }
            b'r' if (i == 0
                || !ident(bytes[i - 1])
                || (bytes[i - 1] == b'b' && (i < 2 || !ident(bytes[i - 2]))))
                && matches!(bytes.get(i + 1), Some(b'"' | b'#')) =>
            {
                let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                if bytes.get(i + 1 + hashes) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    let body = i + 2 + hashes;
                    i = text[body..]
                        .find(&close)
                        .map_or(bytes.len(), |end| body + end + close.len());
                    continue;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' => {
                if bytes.get(i + 1) == Some(&b'\\') {
                    // The backslash and the character it escapes (which may be the quote), then up to the closing quote.
                    i = text
                        .get(i + 3..)
                        .and_then(|after| after.find('\''))
                        .map_or(bytes.len(), |end| i + 3 + end);
                } else if let Some(c) = text[i + 1..].chars().next()
                    && bytes.get(i + 1 + c.len_utf8()) == Some(&b'\'')
                {
                    i += 1 + c.len_utf8();
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    // The closing brace, and a `;` after it (whitespace between them goes too, harmlessly).
                    let after = text[i + 1..].trim_start();
                    return text.len() - after.len() + usize::from(after.starts_with(';'));
                }
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// Reads as words for a person: begins with a capital letter and a letter, or with a word of one letter ("A device…",
/// "I…"), and is not an identifier written in capitals (`PEERSUS_PAGE`).
fn words(text: &str) -> bool {
    let identifier = text.contains('_')
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let mut chars = text.chars();
    !identifier
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && match chars.next() {
            Some(' ') => chars.next().is_some_and(char::is_alphabetic),
            next => next.is_some_and(char::is_alphabetic),
        }
}

fn sources(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory)
        .expect("a source directory reads")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Files of the window that draw nothing a translation could change: the credits and licences (names and legal text kept as
/// written), the made-up demo, key names, fonts, the desktop's own bus names, and tests and probes.
const WINDOW_FILES_WITHOUT_WORDS: &[&str] = &[
    "about.rs",
    "demo.rs",
    "demo_owner.rs",
    "font.rs",
    "system_theme.rs",
    "outside_chords.rs",
    "i18n.rs",
    "coverage.rs",
    "tests.rs",
    "layout_regression.rs",
    "headless.rs",
    "probe.rs",
    "main.rs",
];

/// Literals of the window that are names or formats, not words: the product, platforms, units, a repository, desktop entry
/// lines, an environment variable, a made-up device of the preview.
const WINDOW_NAMES: &[&str] = &[
    "Peersus",
    "Android",
    "Linux",
    "Windows",
    "FreeBSD",
    "KB",
    "MB",
    "GB",
    "TB",
    "DarkPhilosophy/Peersus",
    "StartupWMClass={id}",
    "Icon={id}",
    "PEERSUS_HEALED",
    "Peersus \u{b7} {name}",
    "Alex's laptop",
    // The name English gives itself.
    "English",
];

/// The owner's files whose sentences no person reads in the window, and why. Every other file of the owner is read, each with
/// its own modules, so a new file that hands the window a sentence is checked without being listed (a list of the files to
/// read had left the store's refusals unread). An entry is a file and its modules (`x.rs` is `x.rs` and `x/**`), or a module
/// path without `.rs` for that module alone.
const OWNER_FILES_WITHOUT_WORDS: &[&str] = &[
    // The command line answers in English in a terminal; the window does not show it.
    "cli.rs",
    // Its one sentence is the outside-store warning the command line prints; the window draws its own (`agreement_overlay`).
    "plugin_agreements.rs",
    // A fixed tag the window recognises, not a sentence it shows.
    "lib.rs",
    // Answers to the extension that called, in the contract's words: an extension shows them, if at all, as its own.
    "clipboard_handles.rs",
    "desktop_clipboard.rs",
    "page_offers.rs",
    "platform_calls.rs",
    "status_service.rs",
    "platform_services/file_http",
    "platform_services/location",
    // The private copy of a chosen file.
    "platform_services/snapshot",
    // The application's own development programs.
    "bin",
];

/// Whether the owner file at `relative` (to `peersus-app/src`, with `/`) is one of [`OWNER_FILES_WITHOUT_WORDS`].
fn without_words(relative: &str) -> bool {
    OWNER_FILES_WITHOUT_WORDS.iter().any(|skip| {
        let module = skip.strip_suffix(".rs").unwrap_or(skip);
        relative == *skip
            || relative == format!("{module}.rs")
            || relative.starts_with(&format!("{module}/"))
    })
}

/// Where the owner gives a sentence away: as a refusal, an error, a reason, an answer chosen by a match, or a named constant.
fn handed_on(before: &str) -> bool {
    const ENDINGS: &[&str] = &[
        "Err(",
        "ok_or(",
        ".error(",
        "Refusal(",
        "Failed(",
        "reason:",
        "Some(",
        "=>",
        "=> {",
        "&str =",
        "&'static str =",
    ];
    ENDINGS.iter().any(|ending| before.ends_with(ending))
        || (before.ends_with('|')
            && before
                .rsplit_once("map_err(")
                .is_some_and(|(_, after)| !after.contains(')')))
        || (before.ends_with(',')
            && before
                .rsplit_once("plugin_error(")
                .is_some_and(|(_, after)| !after.contains(')')))
}

#[test]
fn every_sentence_a_person_can_be_shown_is_a_message_of_the_template() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut missing = Vec::new();
    let mut scanned = 0;
    let mut window = Vec::new();
    sources(&crates.join("peersus-ui/src"), &mut window);
    for path in &window {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if WINDOW_FILES_WITHOUT_WORDS.contains(&name)
            || path.components().any(|part| part.as_os_str() == "headless")
        {
            continue;
        }
        for literal in literals(&production(path)) {
            if literal.in_skipped_macro
                || !words(&literal.text)
                || WINDOW_NAMES.contains(&literal.text.as_str())
                || literal.before.ends_with("#[")
                || literal.before.ends_with("expect(")
                // The name of an environment variable the window reads.
                || literal.before.ends_with("var(")
                || literal.before.ends_with("var_os(")
            {
                continue;
            }
            scanned += 1;
            if !super::has(&literal.text) {
                missing.push(format!("window {name}: {}", literal.text));
            }
        }
    }
    let root = crates.join("peersus-app/src");
    let mut owner = Vec::new();
    sources(&root, &mut owner);
    owner.retain(|path| {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        !without_words(&relative)
            && !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem == "tests" || stem.ends_with("_tests"))
    });
    for path in &owner {
        let name = path.strip_prefix(&root).unwrap_or(path).display();
        for literal in literals(&production(path)) {
            if literal.in_skipped_macro
                || !words(&literal.text)
                || !literal.text.contains(' ')
                || literal.text.contains('{')
                || !handed_on(&literal.before)
            {
                continue;
            }
            scanned += 1;
            if !super::has(&literal.text) {
                missing.push(format!("owner {name}: {}", literal.text));
            }
        }
    }
    assert!(
        scanned > 400,
        "the scan found too few sentences to be reading the sources: {scanned}"
    );
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "add these to the template and every language: {missing:#?}"
    );
}

#[test]
fn the_lexer_reads_what_a_person_sees_and_skips_the_rest() {
    let source = r##"
        // "A comment"
        /* "A block" /* "nested" */ */
        let a = Err("A refusal");
        tracing::warn!(reason = "Not shown", "A log line");
        let b = r#"A raw "quoted" text"#;
        let c = '"'; let d: &'static str = "A constant";
        let e = "A line \
                 continued";
    "##;
    let read: Vec<(String, bool)> = literals(source)
        .into_iter()
        .map(|literal| (literal.text, literal.in_skipped_macro))
        .collect();
    assert_eq!(
        read,
        [
            ("A refusal".to_owned(), false),
            ("Not shown".to_owned(), true),
            ("A log line".to_owned(), true),
            ("A raw \"quoted\" text".to_owned(), false),
            ("A constant".to_owned(), false),
            ("A line continued".to_owned(), false),
        ]
    );
    assert!(handed_on("let a = Err("));
    assert!(handed_on(".map_err(|_|"));
    assert!(handed_on("self.plugin_error(&plugin,"));
    assert!(handed_on("const UNREACHABLE: Refusal = Refusal("));
    assert!(!handed_on("let x = ("));
}

#[test]
fn every_test_item_is_left_out_and_the_code_after_each_is_read() {
    let source = r##"
        #[cfg(test)]
        use crate::helpers::{one, two};
        const FIRST: &str = "A sentence after a test-only use";
        #[cfg(test)]
        mod inline_tests {
            fn braces() { let a = "}"; let b = '}'; let c = r#"}}"#; let d = '\''; /* } */ }
            // }
            fn life<'a>(x: &'a str) -> &'a str { x }
            const HIDDEN: &str = "A test sentence";
        }
        const SECOND: &str = "A sentence after a test module";
        #[cfg(test)]
        mod tests;
        const THIRD: &str = "A sentence at the end";
        #[cfg(all(feature = "demo", not(target_os = "android")))]
        fn preview() { let folder = "A sentence of the demo only"; }
        #[cfg(not(feature = "demo"))]
        const FOURTH: &str = "A sentence of the product build";
    "##;
    let read: Vec<String> = literals(&without_test_items(source))
        .into_iter()
        .map(|literal| literal.text)
        .filter(|text| words(text))
        .collect();
    assert_eq!(
        read,
        [
            "A sentence after a test-only use",
            "A sentence after a test module",
            "A sentence at the end",
            "A sentence of the product build",
        ]
    );
}
