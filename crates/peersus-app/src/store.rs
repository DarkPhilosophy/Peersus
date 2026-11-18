//! What the store shows and the names it fetches. The trust flow itself (verify the catalog, check the
//! archive against the signed entry, bind it to its manifest and program) lives in
//! [`crate::store_service`], which is the one place that decides; this module has no second copy of it.
use crate::catalog::{Catalog, Entry};

/// Catalog and signature file names, fixed.
pub const CATALOG_FILE: &str = "catalog.json";
pub const SIGNATURE_FILE: &str = "catalog.sig";

/// What an offered extension looks like to the window: only what the signed catalog said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub version: String,
    pub author: String,
    pub link: String,
    pub description: String,
    /// The archive's size for this platform, as the signed catalog pinned it.
    pub bytes: u64,
}

impl From<&Entry> for Listing {
    fn from(entry: &Entry) -> Self {
        Self {
            name: entry.name.clone(),
            version: entry.version.clone(),
            author: entry.author.clone(),
            link: entry.link.clone(),
            description: entry.description.clone(),
            bytes: entry.size,
        }
    }
}

/// What a person may search: every approved entry whose name, author or description contains `query`,
/// case-insensitively. An empty query lists everything. Order is the catalog's (sorted by name).
pub fn search(catalog: &Catalog, query: &str) -> Vec<Listing> {
    let needle = query.trim().to_lowercase();
    catalog
        .entries
        .iter()
        .filter(|entry| {
            needle.is_empty()
                || [&entry.name, &entry.author, &entry.description]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
        })
        .map(Listing::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, author: &str, description: &str) -> Entry {
        Entry {
            name: name.into(),
            version: "1.0.0".into(),
            sha256: "0".repeat(64),
            size: 1,
            author: author.into(),
            link: "https://example.org/x".into(),
            description: description.into(),
            archive: format!("{name}.tar.gz"),
        }
    }

    #[test]
    fn search_matches_name_author_and_description_case_insensitively() {
        let catalog = Catalog {
            schema: 1,
            sequence: 1,
            entries: vec![
                entry("echo", "Someone", "Copies text"),
                entry("zeta", "Other", "Copies files"),
            ],
        };
        assert_eq!(search(&catalog, "").len(), 2);
        assert_eq!(search(&catalog, "ECHO")[0].name, "echo");
        assert_eq!(search(&catalog, "other")[0].name, "zeta", "by author");
        assert_eq!(search(&catalog, "copies").len(), 2, "by description");
        assert_eq!(
            search(&catalog, "  echo  ").len(),
            1,
            "surrounding space is ignored"
        );
        assert!(search(&catalog, "nothing like this").is_empty());
    }
}
