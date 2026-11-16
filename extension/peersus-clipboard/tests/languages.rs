//! The extension's own translations, `wasm/languages/<tag>.po`, beside the manifest every package of it is made from: each
//! file is the language its name says, keeps every translation the application would keep, and translates every word of that
//! manifest the application translates (its contributions and its description). A word changed in the manifest and not in a
//! file would stay English in that language, silently. What a capability lets it do is never translated: a person grants on
//! the manifest's own English.

use peersus_api::language::{Catalog, Language};
use std::{fs, path::Path};

const MANIFEST: &str = include_str!("../wasm/plugin.json");

#[test]
fn every_language_file_translates_every_word_the_manifest_shows() {
    let split = peersus_api::package::split(MANIFEST.as_bytes()).unwrap();
    let manifest: peersus_api::manifest::Manifest = serde_json::from_slice(&split.manifest).unwrap();
    let package = split.package.expect("the manifest describes its package");
    let words: Vec<&str> = manifest
        .contributions
        .words()
        .chain(std::iter::once(package.description.as_str()))
        .collect();
    assert!(!words.is_empty());
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("wasm/languages");
    let mut files = 0;
    for entry in fs::read_dir(&folder).unwrap() {
        let path = entry.unwrap().path();
        let tag = path.file_stem().and_then(|stem| stem.to_str()).unwrap();
        let catalog = Catalog::parse(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|error| panic!("{tag}: {error:?}"));
        assert_eq!(catalog.language(), Some(Language::parse(tag).unwrap()), "{tag}");
        let problems = catalog.problems(None);
        assert!(problems.is_empty(), "{tag}: {problems:?}");
        let (kept, _) = catalog.without_problems(None);
        let missing: Vec<&&str> = words.iter().filter(|word| kept.get(None, word).is_none()).collect();
        assert!(missing.is_empty(), "{tag} does not translate {missing:?}");
        files += 1;
    }
    assert_eq!(files, 11);
}
