//! An extension's own translations of the words its manifest shows: `languages/<tag>.po` in its package, gettext files of
//! the same format as the application's (see [`peersus_api::language`]), each translating the English texts of its own
//! manifest. The words the extension shows at run time it translates itself (`peersus.language`).
//!
//! What a package brings is untrusted and bounded: at most [`MAX_LANGUAGES`] files of at most a language file's size,
//! plain files only, each read as the language its name and its header both say, and only the translations whose
//! placeholders fit their English text kept. A file that fails any of that is left out; the package still loads and shows
//! its English.

use peersus_api::language::{Catalog, Language, MAX_FILE_BYTES};
use std::{fs, io::Read, path::Path, sync::Arc};

/// The folder of a package that holds its languages.
pub const DIRECTORY: &str = "languages";
/// The most languages one package's files may bring.
pub const MAX_LANGUAGES: usize = 64;
/// The most entries of that folder looked at, so a folder of many other things costs no more than this.
const MAX_ENTRIES: usize = 256;

/// The languages the package in `package` brings, sorted by tag.
pub fn load(package: &Path) -> Arc<[Catalog]> {
    let folder = package.join(DIRECTORY);
    // A link could point anywhere outside the package.
    if !fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir()) {
        return Arc::default();
    }
    let Ok(entries) = fs::read_dir(&folder) else {
        return Arc::default();
    };
    let mut held: Vec<Catalog> = Vec::new();
    for entry in entries.flatten().take(MAX_ENTRIES) {
        if held.len() == MAX_LANGUAGES {
            break;
        }
        let path = entry.path();
        let Some(tag) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".po"))
            .and_then(|tag| Language::parse(tag).ok())
        else {
            continue;
        };
        if held.iter().any(|catalog| catalog.language() == Some(tag)) {
            continue;
        }
        if let Some(catalog) = read(&path, tag) {
            held.push(catalog);
        }
    }
    held.sort_by_key(Catalog::language);
    held.into()
}

/// The file of `language` among `languages`: that exact language, else its primary one (`pt` for `pt-br`).
pub fn of(languages: &[Catalog], language: Language) -> Option<&Catalog> {
    let find = |wanted: Language| {
        languages
            .iter()
            .find(|catalog| catalog.language() == Some(wanted))
    };
    find(language).or_else(|| find(language.primary()))
}

fn read(path: &Path, tag: Language) -> Option<Catalog> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    let catalog = Catalog::parse(&text).ok()?;
    (catalog.language() == Some(tag)).then(|| catalog.without_problems(None).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(language: &str, english: &str, translated: &str) -> String {
        format!(
            "msgid \"\"\nmsgstr \"Language: {language}\\n\"\n\nmsgid \"{english}\"\nmsgstr \"{translated}\"\n"
        )
    }

    #[test]
    fn a_package_brings_only_the_files_that_are_what_they_say() {
        let package = tempfile::tempdir().unwrap();
        let folder = package.path().join(DIRECTORY);
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("ro.po"), file("ro", "Share", "Partajează")).unwrap();
        // Its header says another language than its name.
        fs::write(folder.join("de.po"), file("fr", "Share", "Partager")).unwrap();
        // Not a language file at all, and not one by its name.
        fs::write(folder.join("it.po"), "not gettext").unwrap();
        fs::write(folder.join("notes.txt"), file("es", "Share", "Compartir")).unwrap();
        let languages = load(package.path());
        assert_eq!(languages.len(), 1);
        let ro = Language::parse("ro").unwrap();
        assert_eq!(
            of(&languages, Language::parse("ro-RO").unwrap()).and_then(|c| c.get(None, "Share")),
            Some("Partajează"),
            "a regional language falls back to its primary one"
        );
        assert!(of(&languages, Language::ENGLISH).is_none());
        assert!(of(&languages, ro).is_some());
    }

    #[test]
    fn a_package_without_languages_or_with_a_link_brings_none() {
        let package = tempfile::tempdir().unwrap();
        assert!(load(package.path()).is_empty());
        #[cfg(unix)]
        {
            let elsewhere = tempfile::tempdir().unwrap();
            fs::write(
                elsewhere.path().join("ro.po"),
                file("ro", "Share", "Partajează"),
            )
            .unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), package.path().join(DIRECTORY)).unwrap();
            assert!(load(package.path()).is_empty());
        }
    }
}
