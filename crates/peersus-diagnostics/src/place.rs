//! Shared panic hook. Never put a panic message or payload in diagnostics.
use std::panic::PanicHookInfo;

pub(crate) fn of(info: &PanicHookInfo<'_>) -> String {
    info.location().map_or_else(
        || "unknown".to_owned(),
        |place| {
            format!(
                "{}:{}:{}",
                source(place.file()),
                place.line(),
                place.column()
            )
        },
    )
}

fn source(file: &str) -> &str {
    // Workspace-relative names disambiguate our own source; for other code keep just the
    // filename. In particular, never expose the builder's home directory or checkout path.
    if file.starts_with("crates/")
        && file.len() <= 160
        && file
            .split('/')
            .all(|part| !matches!(part, "" | "." | "..") && safe_word(part))
    {
        return file;
    }
    file.rsplit(['/', '\\'])
        .next()
        .filter(|part| part.len() <= 96 && safe_word(part))
        .unwrap_or("unknown")
}

fn safe_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn thread_name() -> &'static str {
    // Thread labels may be assembled at runtime. Only keep the names that are known to be fixed.
    match std::thread::current().name() {
        Some("main") => "main",
        Some("tokio-runtime-worker") => "tokio-runtime-worker",
        Some("diagnostic-writer") => "diagnostic-writer",
        Some("diagnostic-watchdog") => "diagnostic-watchdog",
        Some("deadline") => "deadline",
        _ => "other",
    }
}

pub(crate) fn install(on_panic: impl Fn(&PanicHookInfo<'_>) + Send + Sync + 'static) {
    #[cfg(target_os = "android")]
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic(info);
        // Android already used this hook for native Logcat; retain its previous behaviour.
        // Desktop stderr can be owner.log. Never give the default printer the raw message.
        #[cfg(target_os = "android")]
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::source;

    #[test]
    fn locations_keep_source_names_but_not_machine_paths() {
        assert_eq!(
            source("crates/peersus-app/src/runtime.rs"),
            "crates/peersus-app/src/runtime.rs"
        );
        assert_eq!(source("/home/person/private/project/src/lib.rs"), "lib.rs");
        assert_eq!(source(r"C:\Users\person\src\lib.rs"), "lib.rs");
        assert_eq!(source("crates/../../private/lib.rs"), "lib.rs");
        assert_eq!(source("/private/my file.rs"), "unknown");
    }
}
