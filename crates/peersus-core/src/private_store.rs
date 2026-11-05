//! Explicit private-development catalog configuration. Never part of preferences or snapshots.
use crate::model::SecretText;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateCatalog {
    pub repository: String,
    pub directory: String,
    pub revision: String,
    pub token: SecretText,
    /// Explicitly chosen development trust root; never changes the production key.
    pub public_key: [u8; 32],
}

impl PrivateCatalog {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn component(value: &str) -> bool {
            !value.is_empty()
                && value != "."
                && value != ".."
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }
        if self.repository.len() > 256
            || self.repository.split('/').count() != 2
            || !self.repository.split('/').all(component)
            || self.directory.len() > 512
            || !self.directory.split('/').all(component)
            || self.revision.len() > 256
            || !self.revision.split('/').all(component)
            || self.token.expose().is_empty()
            || !self.token.expose().bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("The private catalog configuration is not valid");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scoped_sources_with_nonempty_credentials_are_accepted() {
        let mut source = PrivateCatalog {
            repository: "example/private".into(),
            directory: "store".into(),
            revision: "main".into(),
            token: SecretText::new("fake-test-only").unwrap(),
            public_key: [0; 32],
        };
        source.validate().unwrap();
        for value in [
            "../private",
            "example/private/extra",
            "https://example/private",
        ] {
            source.repository = value.into();
            assert!(source.validate().is_err());
        }
        source.repository = "example/private".into();
        source.token = SecretText::new("").unwrap();
        assert!(source.validate().is_err());
    }
}
