//! `cargo xtask check-manifest PATH`: the one command scripts and CI call to prove an extension
//! package carries the licence-and-notice agreement the application requires before it may be
//! shown or installed, and that its own translations keep up with its manifest. `PATH` is a
//! package folder (holding exactly one of `plugin.json` or `plugin.toml`) or a manifest file itself.
//!
//! The rules are the application's own: the file is read the way
//! `peersus_app::discovery` reads it (TOML through the same JSON value, `package` split out and
//! checked on its own, unknown fields refused), then `peersus_api::manifest::Manifest::validate`
//! runs. The one rule that is the store's and not the manifest type's — `agreements` must be
//! there, because a manifest without one is still a valid manifest but can never be installed —
//! is stated here once, in the checker everything else calls. So is the rule for the
//! `languages/<tag>.po` files beside the manifest, which the packager carries and the application
//! reads with the same `peersus_api::language::Catalog`.
use peersus_api::{
    language::{Catalog, Language},
    manifest::Manifest,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The two names a package may give its manifest, looked for at the package root and, in the
/// source layout of an extension whose guest lives in `wasm/`, under `wasm/`.
const NAMES: [&str; 2] = ["plugin.json", "plugin.toml"];
/// Largest manifest read, in bytes, matching `peersus_app::discovery::MAX_MANIFEST`.
const MAX_MANIFEST: u64 = 64 * 1024;
fn manifest_path(target: &Path) -> Result<PathBuf, String> {
    if target.is_dir() {
        // A root manifest is the manifest. Only a source layout with none looks under `wasm/`,
        // so a folder that carries both a process and a guest variant still names one answer.
        for base in [target.to_path_buf(), target.join("wasm")] {
            let found: Vec<PathBuf> = NAMES
                .iter()
                .map(|name| base.join(name))
                .filter(|path| fs::symlink_metadata(path).is_ok_and(|m| m.is_file()))
                .collect();
            match found.as_slice() {
                [only] => return Ok(only.clone()),
                [] => continue,
                _ => {
                    return Err(format!(
                        "two manifests: a package needs exactly one of plugin.json or plugin.toml ({})",
                        base.display()
                    ));
                }
            }
        }
        return Err(format!(
            "no manifest: a package needs exactly one of plugin.json or plugin.toml ({})",
            target.display()
        ));
    }
    if target.is_file() {
        Ok(target.to_path_buf())
    } else {
        Err(format!(
            "not a package folder or manifest file: {}",
            target.display()
        ))
    }
}

/// Read one manifest file the way the application does: TOML and JSON become one shape, the
/// `package` block is checked on its own and the rest must be a manifest it knows. The package's
/// description comes with it: it is one of the words the extension's translations carry.
fn read_manifest(path: &Path) -> Result<(Manifest, Option<String>), String> {
    let invalid = |what: &str| format!("{what}: {}", path.display());
    let bytes = fs::read(path)
        .map_err(|error| invalid(&format!("the manifest could not be read ({error})")))?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid("the manifest is too large"));
    }
    let json = if path.file_name().is_some_and(|name| name == "plugin.toml") {
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("the manifest is not valid"))?;
        let value: toml::Value =
            toml::from_str(text).map_err(|_| invalid("the manifest is not valid"))?;
        serde_json::to_vec(&value).map_err(|_| invalid("the manifest is not valid"))?
    } else {
        bytes
    };
    let split =
        peersus_api::package::split(&json).map_err(|_| invalid("the manifest is not valid"))?;
    let manifest = serde_json::from_slice(&split.manifest)
        .map_err(|_| invalid("the manifest is not valid"))?;
    Ok((manifest, split.package.map(|package| package.description)))
}

/// The verdict for `path`: `Ok` with the notes worth reading when the package may be shown and
/// installed, `Err` with the one-sentence reason it cannot. `complete` is an extension's own choice
/// (its recipe passes `--complete-translations`) that a word its translations leave in English, or a
/// translation the application would drop, is a refusal and not a note.
fn check(path: &Path, complete: bool) -> Result<Vec<String>, String> {
    let (manifest, description) = read_manifest(path)?;
    if manifest.agreements.is_none() {
        return Err(format!(
            "no licence-and-notice agreement: the manifest carries no `agreements`, \
             so it cannot be installed ({})",
            path.display()
        ));
    }
    manifest.validate().map_err(|_| {
        format!(
            "the licence-and-notice agreement is not valid: it must hold at least one licence page \
             and one notice page, every page within its limits ({})",
            path.display()
        )
    })?;
    let gaps = check_languages(path, &manifest, description.as_deref())?;
    if complete && let Some(first) = gaps.first() {
        return Err(first.clone());
    }
    Ok(gaps)
}

/// The extension's own translations, `languages/<tag>.po` beside its manifest, when it carries any.
/// Refused: a file that is not `<tag>.po`, cannot be read as a translation, or is in another language
/// than its name says, which the packager and the application would not take as that language.
/// Returned as gaps, one per file: translations the application would drop and words of the manifest
/// the application translates (its contributions and its package's description) that the file leaves
/// in English. The contract allows both (`docs/api/service-messages.md`, Translations): the English is
/// shown. A word changed in the manifest and not in a file is how a translation falls behind, so it is
/// said by name. What a capability lets an extension do is never translated: a person grants on the
/// manifest's own English.
fn check_languages(
    path: &Path,
    manifest: &Manifest,
    description: Option<&str>,
) -> Result<Vec<String>, String> {
    let folder = path.parent().unwrap_or(Path::new(".")).join("languages");
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "the translations could not be read ({error}): {}",
                folder.display()
            ));
        }
    };
    let mut files = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "the translations could not be read ({error}): {}",
                folder.display()
            )
        })?;
    files.sort();
    let words: Vec<&str> = manifest
        .contributions
        .words()
        .chain(description.filter(|text| !text.is_empty()))
        .collect();
    let mut gaps = Vec::new();
    for file in files {
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let said = |what: String| format!("languages/{name} {what} ({})", file.display());
        let tag = name.strip_suffix(".po").ok_or_else(|| {
            said("is not a translation: only <tag>.po files belong there".to_owned())
        })?;
        let language =
            Language::parse(tag).map_err(|_| said("is not named by a language tag".to_owned()))?;
        let text = fs::read_to_string(&file)
            .map_err(|error| said(format!("could not be read ({error})")))?;
        let catalog = Catalog::parse(&text)
            .map_err(|error| said(format!("is not a valid file ({error})")))?;
        if catalog.language() != Some(language) {
            return Err(said(
                "says it is in another language than its name".to_owned(),
            ));
        }
        let problems = catalog.problems(None);
        if !problems.is_empty() {
            gaps.push(said(format!(
                "has {} translation(s) the application would not show: {problems:?}",
                problems.len()
            )));
        }
        let (kept, _) = catalog.without_problems(None);
        let missing: Vec<&str> = words
            .iter()
            .copied()
            .filter(|word| kept.get(None, word).is_none())
            .collect();
        if !missing.is_empty() {
            gaps.push(said(format!(
                "leaves in English {missing:?}: does not translate them"
            )));
        }
    }
    Ok(gaps)
}

pub fn run(args: &[String]) -> Result<(), String> {
    const USE: &str = "use: cargo xtask check-manifest [--complete-translations] <package-folder-or-manifest-file>";
    let (complete, target) = match args {
        [target] => (false, target),
        [flag, target] if flag == "--complete-translations" => (true, target),
        _ => return Err(USE.to_owned()),
    };
    let path = manifest_path(Path::new(target))?;
    for gap in check(&path, complete)? {
        println!("note: {gap}");
    }
    println!(
        "{}: licence-and-notice agreement present and valid{}",
        path.display(),
        if complete {
            "; its translations carry every word"
        } else {
            ""
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest everything else in the module's tests edits: valid once `agreements` is in.
    const BASE: &str =
        r#"{"manifest_version":1,"capabilities":{},"imports":[],"exports":[]AGREEMENTS}"#;
    const GOOD_JSON: &str = r#"{"manifest_version":1,"capabilities":{},"imports":[],"exports":[],
        "agreements":{"pages":[
          {"kind":"licence","title":"Apache-2.0","explains":"You may use and share it.","changes":"Nothing changes on your devices.","spdx":"Apache-2.0","links":[{"label":"Licence text","url":"https://www.apache.org/licenses/LICENSE-2.0"}]},
          {"kind":"notice","title":"What it does","explains":"It says hello.","changes":"Nothing changes on your devices."}]}}"#;
    const GOOD_TOML: &str = r#"manifest_version = 1
capabilities = {}
imports = []
exports = []
[[agreements.pages]]
kind = "licence"
title = "Apache-2.0"
explains = "You may use and share it."
changes = "Nothing changes on your devices."
spdx = "Apache-2.0"
[[agreements.pages.links]]
label = "Licence text"
url = "https://www.apache.org/licenses/LICENSE-2.0"
[[agreements.pages]]
kind = "notice"
title = "What it does"
explains = "It says hello."
changes = "Nothing changes on your devices."
"#;

    fn with_agreements(agreements: &str) -> String {
        BASE.replace("AGREEMENTS", &format!(",{agreements}"))
    }

    /// A unique empty directory per test, removed when it drops.
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "xtask-manifest-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&directory).unwrap();
            Temp(directory)
        }
        fn put(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, text).unwrap();
            path
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_manifest_without_agreements_is_refused() {
        let temp = Temp::new();
        temp.put("plugin.json", &BASE.replace("AGREEMENTS", ""));
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(
            reason.contains("no licence-and-notice agreement"),
            "{reason}"
        );
    }

    #[test]
    fn a_licence_page_alone_is_refused() {
        let temp = Temp::new();
        temp.put("plugin.json", &with_agreements(
            r#""agreements":{"pages":[{"kind":"licence","title":"MIT","explains":"Use it.","changes":"Nothing.","spdx":"MIT","links":[{"label":"L","url":"https://example.org/l"}]}]}"#,
        ));
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(reason.contains("agreement is not valid"), "{reason}");
    }

    #[test]
    fn an_insecure_link_is_refused() {
        let temp = Temp::new();
        temp.put("plugin.json", &with_agreements(
            r#""agreements":{"pages":[
              {"kind":"licence","title":"MIT","explains":"Use it.","changes":"Nothing.","spdx":"MIT","links":[{"label":"L","url":"http://example.org/l"}]},
              {"kind":"notice","title":"N","explains":"Does x.","changes":"Nothing."}]}"#,
        ));
        assert!(run(&[temp.0.to_string_lossy().into_owned()]).is_err());
    }

    #[test]
    fn a_valid_agreement_is_accepted_as_json_and_toml() {
        for (name, text) in [("plugin.json", GOOD_JSON), ("plugin.toml", GOOD_TOML)] {
            let temp = Temp::new();
            let path = temp.put(name, text);
            run(&[temp.0.to_string_lossy().into_owned()])
                .unwrap_or_else(|reason| panic!("{name}: {reason}"));
            run(&[path.to_string_lossy().into_owned()])
                .unwrap_or_else(|reason| panic!("{name} file: {reason}"));
        }
    }

    #[test]
    fn a_folder_with_two_manifests_or_none_is_refused() {
        let temp = Temp::new();
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(reason.contains("no manifest"), "{reason}");
        temp.put("plugin.json", GOOD_JSON);
        temp.put("plugin.toml", GOOD_TOML);
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(reason.contains("two manifests"), "{reason}");
    }

    /// A manifest with one setting (a label and its help) and a package description: three words a translation carries.
    fn with_words() -> String {
        GOOD_JSON.replacen(
            r#""manifest_version":1,"#,
            r#""manifest_version":1,
            "package":{"name":"Hello","version":"1.0.0","author":"A","link":"https://example.org","description":"Says hello."},
            "contributions":{"settings":[{"id":"on","label":"Say hello","help":"Greets the person.","scope":"global","kind":{"type":"toggle","default":false}}]},"#,
            1,
        )
    }

    fn po(language: &str, entries: &[(&str, &str)]) -> String {
        let mut text = format!(
            "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Language: {language}\\n\"\n"
        );
        for (english, translated) in entries {
            text += &format!("\nmsgid \"{english}\"\nmsgstr \"{translated}\"\n");
        }
        text
    }

    fn package_with(language_file: &str, text: &str) -> Temp {
        let temp = Temp::new();
        temp.put("plugin.json", &with_words());
        fs::create_dir(temp.0.join("languages")).unwrap();
        temp.put(&format!("languages/{language_file}"), text);
        temp
    }

    const ALL: [(&str, &str); 3] = [
        ("Say hello", "Salută"),
        ("Greets the person.", "Salută persoana."),
        ("Says hello.", "Salută."),
    ];

    #[test]
    fn translations_that_carry_every_word_pass_and_a_package_without_any_needs_none() {
        let temp = package_with("ro.po", &po("ro", &ALL));
        let folder = temp.0.to_string_lossy().into_owned();
        run(std::slice::from_ref(&folder)).unwrap();
        run(&["--complete-translations".to_owned(), folder]).unwrap();
        let without = Temp::new();
        without.put("plugin.json", &with_words());
        let folder = without.0.to_string_lossy().into_owned();
        run(&["--complete-translations".to_owned(), folder]).unwrap();
    }

    #[test]
    fn a_word_left_in_english_is_a_note_by_contract_and_a_refusal_when_the_extension_asks_for_completeness()
     {
        // The description is a word too: a changed description would otherwise stay English, unsaid.
        let temp = package_with("ro.po", &po("ro", &ALL[..2]));
        let path = manifest_path(&temp.0).unwrap();
        let notes = check(&path, false).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("leaves in English [\"Says hello.\"]"),
            "{notes:?}"
        );
        let reason = check(&path, true).unwrap_err();
        assert!(
            reason.contains("leaves in English [\"Says hello.\"]"),
            "{reason}"
        );
    }

    #[test]
    fn a_file_in_another_language_than_its_name_or_not_a_translation_is_refused() {
        let temp = package_with("ro.po", &po("de", &ALL));
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(reason.contains("another language"), "{reason}");
        let temp = package_with("notes.txt", "anything");
        let reason = run(&[temp.0.to_string_lossy().into_owned()]).unwrap_err();
        assert!(reason.contains("only <tag>.po files"), "{reason}");
    }
}
