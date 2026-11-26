//! `cargo xtask languages-offer [DIRECTORY]` writes the offer of one release's language files, and
//! `cargo xtask check-languages [DIRECTORY]` proves it is current and every file fits the template. `DIRECTORY` is the
//! release's directory of a copy of the languages branch, `languages/<release>/` by default (the release is the
//! workspace's version).
//!
//! The offer (`languages.json`) is what the application reads to know which languages there are, the name each gives
//! itself and how many of the template's messages it translates; it counts the way the window does when it installs a
//! file, against the template, after dropping every translation that does not fit (see `peersus_api::language`).
use peersus_api::language::{Catalog, MAX_FILE_BYTES};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The template every language file translates: the application's messages, in English.
const TEMPLATE: &str = "crates/peersus-ui/languages/messages.pot";
/// The offer's name, beside the files (`peersus_app::languages::OFFER_FILE`).
const OFFER_FILE: &str = "languages.json";

fn release() -> Result<String, String> {
    let manifest: toml::Table = fs::read_to_string("Cargo.toml")
        .map_err(|error| format!("Cargo.toml could not be read ({error})"))?
        .parse()
        .map_err(|error| format!("Cargo.toml could not be parsed ({error})"))?;
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Cargo.toml names no workspace version".to_owned())
}

fn read(path: &Path) -> Result<Catalog, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{}: larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8", path.display()))?;
    Catalog::parse(&text).map_err(|error| format!("{}: {error:?}", path.display()))
}

/// The offer of `directory`, and every problem found on the way (an empty list is a clean release).
fn offer(directory: &Path, release: &str) -> Result<(serde_json::Value, Vec<String>), String> {
    let template = read(Path::new(TEMPLATE))?;
    let messages = template.counts().0;
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "po"))
        .collect();
    files.sort();
    let mut problems = Vec::new();
    let mut languages = Vec::new();
    for path in files {
        let catalog = read(&path)?;
        let Some(language) = catalog.language() else {
            problems.push(format!("{}: its header has no Language", path.display()));
            continue;
        };
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if stem != language.tag() {
            problems.push(format!(
                "{}: the file says it is {}; its name must be {}.po",
                path.display(),
                language.tag(),
                language.tag()
            ));
            continue;
        }
        let Some(name) = catalog.name().map(str::to_owned) else {
            problems.push(format!(
                "{}: its header has no X-Language-Name (the name the language gives itself)",
                path.display()
            ));
            continue;
        };
        for problem in catalog.problems(Some(&template)) {
            problems.push(format!(
                "{}: {:?}: {}",
                path.display(),
                problem.english,
                problem.what
            ));
        }
        let machine = catalog.machine_translated();
        let (fitting, _) = catalog.without_problems(Some(&template));
        // What downloading it costs, said to a person before they choose it; the application refuses a file of another size.
        let bytes = std::fs::metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .len();
        languages.push(serde_json::json!({
            "language": language.tag(),
            "name": name,
            "translated": fitting.counts().1,
            "bytes": bytes,
            "machine": machine,
        }));
    }
    Ok((
        serde_json::json!({ "release": release, "messages": messages, "languages": languages }),
        problems,
    ))
}

fn directory_of(args: &[String], release: &str) -> Result<PathBuf, String> {
    match args {
        [] => Ok(Path::new("languages").join(release)),
        [directory] => Ok(PathBuf::from(directory)),
        _ => {
            Err("takes at most one argument, the release's directory of language files".to_owned())
        }
    }
}

fn encode(offer: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(offer)
        .map(|text| text + "\n")
        .map_err(|error| error.to_string())
}

/// `languages-offer`: writes the offer. Problems are printed: the files are offered for what they get right.
pub fn write(args: &[String]) -> Result<(), String> {
    let release = release()?;
    let directory = directory_of(args, &release)?;
    let (offer, problems) = offer(&directory, &release)?;
    for problem in &problems {
        eprintln!("warning: {problem}");
    }
    let path = directory.join(OFFER_FILE);
    fs::write(&path, encode(&offer)?).map_err(|error| format!("{}: {error}", path.display()))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

/// `check-languages`: fails when a file does not fit the template or the offer is not the one its files make.
pub fn check(args: &[String]) -> Result<(), String> {
    let release = release()?;
    let directory = directory_of(args, &release)?;
    let (offer, problems) = offer(&directory, &release)?;
    let path = directory.join(OFFER_FILE);
    let held = fs::read_to_string(&path).unwrap_or_default();
    let mut failures = problems;
    if held != encode(&offer)? {
        failures.push(format!(
            "{} is not the offer of its files; run cargo xtask languages-offer",
            path.display()
        ));
    }
    if failures.is_empty() {
        eprintln!(
            "{}: every language file fits and the offer is current",
            directory.display()
        );
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
