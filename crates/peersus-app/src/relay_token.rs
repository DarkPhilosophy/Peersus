//! Desktop storage of the access token of a personal relay. Like the identity: the system credential store only, no
//! plaintext fallback, and no credential in any error. It is a secret and so is never part of the preferences.
//! Call off the UI thread. Mobile supplies its native keystore.
use keyring::{Entry, Error};
use peersus_net::transport::RelayAccess;
use zeroize::Zeroizing;

const SERVICE: &str = "Peersus";
const ACCOUNT: &str = "relay-token";

fn entry() -> Result<Entry, &'static str> {
    Entry::new(SERVICE, ACCOUNT).map_err(|_| "System credential store is unavailable")
}

/// The saved token, or `None` when the person has not saved one. A token that cannot be read is an error and is left as
/// it is: it is never taken for "none", which would send the next relay request without it and never say why.
pub fn load() -> Result<Option<RelayAccess>, &'static str> {
    load_entry(&entry()?)
}

/// Save this token, or forget the saved one when `token` is empty. The stored value is read back before this says it worked.
pub fn save(token: &str) -> Result<(), &'static str> {
    save_entry(&entry()?, token)
}

/// The system credential store as the place a runtime keeps the token. A call may wait for the credential service for a moment; it is made
/// only when the person saves or forgets a token, which is rare, and never from the window's own thread.
pub struct Keyring;

impl crate::runtime::TokenStore for Keyring {
    fn save(&self, token: &str) -> Result<(), &'static str> {
        save(token)
    }
}

fn load_entry(entry: &Entry) -> Result<Option<RelayAccess>, &'static str> {
    match entry.get_password() {
        Ok(saved) => {
            let saved = Zeroizing::new(saved);
            RelayAccess::new(&saved)
                .map(Some)
                .ok_or("The saved relay token is not usable; it has not been changed")
        }
        Err(Error::NoEntry) => Ok(None),
        Err(_) => Err("Cannot read the relay token; the saved one was not changed"),
    }
}

fn save_entry(entry: &Entry, token: &str) -> Result<(), &'static str> {
    let token = token.trim();
    if token.is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(_) => Err("Cannot forget the relay token"),
        };
    }
    // The same rules as the token that will be used, so that what is saved is what can be loaded.
    if RelayAccess::new(token).is_none() {
        return Err("The relay token must be one line of at most 512 characters");
    }
    entry
        .set_password(token)
        .map_err(|_| "Cannot save the relay token in the system credential store")?;
    let read_back = Zeroizing::new(
        entry
            .get_password()
            .map_err(|_| "Cannot verify the saved relay token")?,
    );
    if read_back.as_str() != token {
        return Err("The saved relay token changed while it was being saved");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()))
    }

    #[test]
    fn nothing_saved_is_none_and_a_saved_token_comes_back() {
        let entry = entry();
        assert!(load_entry(&entry).unwrap().is_none());
        save_entry(&entry, "  s3cret-token  ").unwrap();
        assert_eq!(
            load_entry(&entry).unwrap(),
            RelayAccess::new("s3cret-token")
        );
    }

    #[test]
    fn an_empty_token_forgets_the_saved_one_and_forgetting_twice_is_fine() {
        let entry = entry();
        save_entry(&entry, "s3cret-token").unwrap();
        save_entry(&entry, "   ").unwrap();
        assert!(load_entry(&entry).unwrap().is_none());
        save_entry(&entry, "").unwrap();
    }

    #[test]
    fn a_token_that_could_never_be_loaded_is_not_saved_and_the_old_one_stays() {
        let entry = entry();
        save_entry(&entry, "good-token").unwrap();
        for bad in ["two\nlines", &"x".repeat(513)] {
            assert!(save_entry(&entry, bad).is_err(), "{bad:?}");
        }
        assert_eq!(load_entry(&entry).unwrap(), RelayAccess::new("good-token"));
    }

    #[test]
    fn an_unusable_saved_token_is_reported_and_left_alone_never_taken_for_none() {
        let entry = entry();
        entry.set_password("bad\nvalue").unwrap();
        assert!(load_entry(&entry).is_err());
        assert_eq!(entry.get_password().unwrap(), "bad\nvalue");
    }

    #[test]
    fn no_refusal_carries_the_token() {
        let entry = entry();
        entry.set_password("bad\nsupersecret").unwrap();
        let said = load_entry(&entry).unwrap_err();
        assert!(!said.contains("supersecret"), "{said}");
        let said = save_entry(&entry, "two\nsupersecret").unwrap_err();
        assert!(!said.contains("supersecret"), "{said}");
    }
}
