//! The one way a text field is made.
//!
//! Every field of the interface (a search box, a device name, an address, an API key) is built here and nowhere else, so that what a
//! field IS decides how it behaves, in one place. A rule added here reaches every field; a field built by hand somewhere else would
//! miss it, and that is the divergence this module exists to end. `tests::no_field_is_built_by_hand` scans every source file of the
//! crate and fails if one is.
//!
//! WHAT A FIELD IS is data, a [`Profile`]: plain properties, none of them tied to a screen, an extension or a provider. The kinds the
//! application knows ([`Profile::TEXT`], [`Profile::SEARCH`], [`Profile::ADDRESS`], [`Profile::SECRET`]) are only values of that same
//! type, listed for convenience; the behaviour never looks at which one it was given, only at the properties. So a profile nobody here
//! has seen (an extension's own idea of a field, a person's setting) is built from the same properties and gets exactly what the
//! application's own fields get, with nothing to edit in the application.
//!
//! A property is added to [`Profile`] only together with the code that acts on it. What is NOT here, on purpose: whether a keyboard
//! may learn what is typed, and the input type a phone keyboard is given. Both need a bridge from the toolkit to the platform's input
//! method that does not exist yet (the toolkit's Android window ignores an input purpose, and the activity builds no input type), so a
//! property for them would promise what nothing delivers. When that bridge exists, the property is added here and every field has it.
use freya::prelude::*;

/// The character drawn in place of each one typed, unless a profile says another.
pub const MASK: char = '•';

/// How a field behaves. Every property is acted on in [`field`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// What is drawn in place of each character typed; `None` shows the text as typed. A masked field also cannot be copied out of: what
    /// is hidden on the screen is never one keystroke from the clipboard (the toolkit enforces this for every masked field).
    pub mask: Option<char>,
    /// The role the field has for assistive technology, and so for a keyboard that is chosen by role. `None` is decided by the toolkit
    /// from how the text is shown: a password field when masked, a plain text field otherwise.
    pub role: Option<Role>,
}

/// The roles a field can take. Plain data a profile points at; adding one is a line here and in [`Role::accessibility`], and no field has
/// to change. The toolkit's own set of roles bounds what can be named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Search,
    Address,
    Secret,
}

impl Role {
    /// The toolkit's own name for this role.
    pub fn accessibility(self) -> AccessibilityRole {
        match self {
            Role::Search => AccessibilityRole::SearchInput,
            Role::Address => AccessibilityRole::UrlInput,
            Role::Secret => AccessibilityRole::PasswordInput,
        }
    }
}

/// Whether a text field of this window has the keyboard now: a key that the field takes for itself (Escape lets go of it) is then
/// not also the window's.
pub(crate) fn editing_text() -> bool {
    matches!(
        Platform::get()
            .current_window()
            .focused_accessibility_node
            .peek()
            .role(),
        AccessibilityRole::TextInput
            | AccessibilityRole::MultilineTextInput
            | AccessibilityRole::SearchInput
            | AccessibilityRole::UrlInput
            | AccessibilityRole::PasswordInput
            | AccessibilityRole::EmailInput
            | AccessibilityRole::NumberInput
            | AccessibilityRole::PhoneNumberInput
    )
}

impl Profile {
    /// Free text: a name, a note.
    pub const TEXT: Profile = Profile {
        mask: None,
        role: None,
    };
    /// A query typed to find something in a list. Nothing in it is private.
    pub const SEARCH: Profile = Profile {
        role: Some(Role::Search),
        ..Profile::TEXT
    };
    /// An address a person types or pastes: a link, a server, a pairing code.
    pub const ADDRESS: Profile = Profile {
        role: Some(Role::Address),
        ..Profile::TEXT
    };
    /// Something private: a key, a token, a password. Hidden and not copyable out.
    pub const SECRET: Profile = Profile {
        mask: Some(MASK),
        role: Some(Role::Secret),
    };

    /// Whether what is typed is hidden on the screen.
    pub fn masked(self) -> bool {
        self.mask.is_some()
    }
}

/// A text field with the given profile over `value`. The only constructor of a field; see the module's documentation.
pub fn field(profile: Profile, value: impl Into<WritableSetter<String>>) -> Input {
    let mut input = Input::new(value);
    if let Some(mask) = profile.mask {
        input = input.mode(InputMode::Hidden(mask));
    }
    match profile.role {
        Some(role) => input.role(role.accessibility()),
        None => input,
    }
}

/// How a field is submitted. Enter always submits; [`Submit::button`] only decides whether a visible Enter button submits too.
pub struct Submit {
    pub on_submit: EventHandler<()>,
    /// Show the trailing Enter button.
    pub button: bool,
    /// Whether submitting is currently possible; the button is disabled otherwise.
    pub enabled: bool,
    /// The colour of the button's mark, so a caller can say whether the last submission failed.
    pub colour: Color,
}

/// Give `input` its submit behaviour: the one place a field's Enter key and Enter button are wired.
pub fn submit(input: Input, submit: Submit) -> Input {
    let Submit {
        on_submit,
        button,
        enabled,
        colour,
    } = submit;
    let on_enter = on_submit.clone();
    let input = input.on_submit(move |_| on_enter.call(()));
    if !button {
        return input;
    }
    input.trailing(
        Button::new()
            .layout_variant(ButtonLayoutVariant::Compact)
            .enabled(enabled)
            .child(label().text("↵").color(colour))
            .on_press(move |_| on_submit.call(())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_the_application_knows_differ_only_in_their_properties() {
        for known in [Profile::TEXT, Profile::SEARCH, Profile::ADDRESS] {
            assert!(!known.masked(), "{known:?} must show what is typed");
        }
        assert!(Profile::SECRET.masked());
        assert_eq!(Profile::TEXT.role, None);
        assert_eq!(Profile::SEARCH.role, Some(Role::Search));
        assert_eq!(Profile::ADDRESS.role, Some(Role::Address));
        assert_eq!(Profile::SECRET.role, Some(Role::Secret));
    }

    #[test]
    fn every_role_names_a_distinct_toolkit_role() {
        let all = [Role::Search, Role::Address, Role::Secret].map(Role::accessibility);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two roles would give the keyboard the same hint");
            }
        }
        assert_eq!(
            Role::Secret.accessibility(),
            AccessibilityRole::PasswordInput
        );
    }

    #[test]
    fn a_profile_made_from_the_same_properties_needs_no_new_kind() {
        // An extension's own idea of a field: masked with another character, and addressed like a link.
        let odd = Profile {
            mask: Some('*'),
            role: Some(Role::Address),
        };
        assert!(odd.masked());
        assert_eq!(
            odd.role.map(Role::accessibility),
            Some(AccessibilityRole::UrlInput)
        );
    }

    /// Every field is built through [`field`]. A field made by hand skips whatever the profiles decide, which is how a search box came
    /// to work where an API key did not. Every source file of the crate is read, so a file added later cannot escape; comments, this
    /// module and test code are not screens.
    #[test]
    fn no_field_is_built_by_hand() {
        let sources = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut by_hand = Vec::new();
        let mut scanned = 0;
        for entry in std::fs::read_dir(&sources).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                || path.file_name().and_then(|n| n.to_str()) == Some("field.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            scanned += 1;
            // Only the trailing test module is not a screen. Cutting at the first `#[cfg(test)]` would drop everything after a single
            // test-only import near the top of a file, which is exactly how a guard goes blind in the file that matters.
            let production = source
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(&source);
            for (number, line) in production.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("Input::new(") || code.contains("InputMode::Hidden(") {
                    by_hand.push(format!(
                        "{}:{}: {}",
                        path.file_name().unwrap().to_string_lossy(),
                        number + 1,
                        code.trim()
                    ));
                }
            }
        }
        assert!(
            scanned > 10,
            "the scan found only {scanned} files; it is not looking where the screens are"
        );
        assert!(
            by_hand.is_empty(),
            "build these through crate::field::field with the Profile they are: {by_hand:#?}"
        );
    }
}
