//! A secret line of text and nothing else, apart from the commands that carry it ([`crate::model`],
//! which gives it out again under its old path).
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// A secret line of text. It serializes as a plain string, but `Debug` never shows it, it is wiped when dropped, and it
/// cannot be longer than any token that could be used, so a client cannot make the owner hold or copy more than that.
#[derive(Clone, Deserialize, Zeroize)]
#[serde(try_from = "String")]
pub struct SecretText(String);

/// The longest secret line kept, in bytes: far past any token, and a bound on what a command can carry.
pub const MAX_SECRET_BYTES: usize = 512;

impl SecretText {
    /// `None` when it is too long or is not one line, because a token that could not be used must not be sent as if it could.
    pub fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        (text.len() <= MAX_SECRET_BYTES && !text.chars().any(char::is_control))
            .then(|| Self(text.to_owned()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretText {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let made = Self::new(&value).ok_or("a secret must be one line of at most 512 bytes");
        let mut value = value;
        zeroize::Zeroize::zeroize(&mut value);
        made
    }
}

impl Serialize for SecretText {
    /// Written from the borrowed text, so that no second, unwiped copy of the secret is made on the way out.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretText([redacted])")
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}
