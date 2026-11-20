//! What a person reads, and accepts or refuses, before an extension is installed: its licences and its
//! notice, as plain data.
//!
//! An extension owns the WORDS and the pictures of its agreement; it never owns the screen. Every
//! frontend draws the same standard overlay, at a size it chooses, with `Cancel` at the far left
//! and `Accept` at the right of the last page. Nothing in this module can place, size, press or
//! hide a button: a [`Block`] has no size, no position, no identifier and no action, so an
//! agreement cannot present itself as something else, and cannot accept for the person.
//!
//! Each page says, in the extension's own words, what it IS (`explains`) and how things change for
//! the person once they accept (`changes`); a manifest that leaves either out is refused.
//!
//! The only things an extension ever learns about the outcome are booleans the host answers: `extension.tos.accepted`, true when this
//! person accepted exactly the agreement installed now, and `extension.tos.outside_store`, true once this person accepted the
//! application's own warning about installing from outside the store (the person's, kept for every extension).
use crate::{Error, Result, name};
use serde::{Deserialize, Serialize};

/// Most pages in one agreement.
pub const MAX_PAGES: usize = 12;
/// Most blocks on one page.
pub const MAX_BLOCKS: usize = 80;
/// Most list items, and most table rows, in one block.
pub const MAX_ITEMS: usize = 40;
/// Most columns of a table.
pub const MAX_COLUMNS: usize = 6;
/// Most links in the list of one page.
pub const MAX_LINKS: usize = 8;
/// Longest title of a page, in characters.
pub const MAX_TITLE: usize = 80;
/// Longest `explains` or `changes`, in characters: long enough to be said plainly, short enough to be read.
pub const MAX_SUMMARY: usize = 600;
/// Longest text of one block, list item or link label, in characters.
pub const MAX_TEXT: usize = 1_200;
/// Longest text of one table cell, in characters.
pub const MAX_CELL: usize = 300;
/// Longest address of a link.
pub const MAX_URL: usize = 2_048;
/// Largest agreement, as JSON bytes.
pub const MAX_BYTES: usize = 128 * 1024;

/// What a page is. Both a licence and a notice are required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The licence of the extension or of something it contains: what the person may do with it.
    Licence,
    /// A notice: what the extension does, what it uses and who made what it uses.
    Notice,
    /// Terms the extension's author asks the person to accept.
    Terms,
}

/// How strongly a callout speaks. The frontend chooses the colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Info,
    Warning,
    Danger,
}

/// One address to read: always `https`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// One piece of a page. Plain content only: no size, no position, no identifier and no action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Heading {
        text: String,
    },
    Paragraph {
        text: String,
        /// Set quieter, for what is secondary.
        #[serde(default)]
        muted: bool,
    },
    List {
        items: Vec<String>,
        #[serde(default)]
        ordered: bool,
    },
    /// A paragraph set apart, for what the person must not miss.
    Callout {
        tone: Tone,
        text: String,
    },
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Link {
        label: String,
        url: String,
    },
    Divider,
}

/// One page of the agreement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub kind: Kind,
    pub title: String,
    /// What this licence or notice is and what it lets the person do or asks of them, in plain words.
    pub explains: String,
    /// What changes for the person, their files and their devices once they accept, in plain words.
    pub changes: String,
    /// The licence as an SPDX expression (`MIT`, `Apache-2.0 OR MIT`): required on a licence page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spdx: Option<String>,
    /// Where the full text can be read. A licence page needs at least one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// The page's own content, after what `explains` and `changes` say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Block>,
}

/// The whole agreement of an extension: at least one licence page and one notice page, in the order read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agreements {
    pub pages: Vec<Page>,
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
}

/// A cell: bounded and without control characters, but it may be empty.
fn cell(value: &str) -> bool {
    value.chars().count() <= MAX_CELL && !value.chars().any(char::is_control)
}

/// An address a person is shown and may open: `https`, with a host, no credentials, no spaces or control characters, bounded.
pub fn address(url: &str) -> bool {
    url.len() <= MAX_URL
        && url
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.starts_with('/'))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url::Url::parse(url).is_ok_and(|parsed| {
            parsed.scheme() == "https"
                && parsed.host_str().is_some_and(|host| !host.is_empty())
                && parsed.username().is_empty()
                && parsed.password().is_none()
        })
}

fn spdx(expression: &str) -> bool {
    !expression.trim().is_empty()
        && expression.len() <= 128
        && expression.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'(' | b')' | b' ')
        })
}

impl Link {
    fn check(&self) -> Result<()> {
        ok(name::text(&self.label, MAX_TEXT) && address(&self.url))
    }
}

impl Block {
    fn check(&self) -> Result<()> {
        match self {
            Block::Heading { text }
            | Block::Paragraph { text, .. }
            | Block::Callout { text, .. } => ok(name::text(text, MAX_TEXT)),
            Block::List { items, .. } => ok(!items.is_empty()
                && items.len() <= MAX_ITEMS
                && items.iter().all(|item| name::text(item, MAX_TEXT))),
            Block::Table { header, rows } => ok(!header.is_empty()
                && header.len() <= MAX_COLUMNS
                && header.iter().all(|head| name::text(head, MAX_CELL))
                && rows.len() <= MAX_ITEMS
                && rows
                    .iter()
                    .all(|row| row.len() == header.len() && row.iter().all(|item| cell(item)))),
            Block::Link { label, url } => ok(name::text(label, MAX_TEXT) && address(url)),
            Block::Divider => Ok(()),
        }
    }
}

impl Page {
    fn check(&self) -> Result<()> {
        ok(name::text(&self.title, MAX_TITLE)
            && name::text(&self.explains, MAX_SUMMARY)
            && name::text(&self.changes, MAX_SUMMARY)
            && self.links.len() <= MAX_LINKS
            && self.body.len() <= MAX_BLOCKS)?;
        for link in &self.links {
            link.check()?;
        }
        for block in &self.body {
            block.check()?;
        }
        match self.kind {
            // A licence names itself and says where its text is: a page of words alone is not the licence.
            Kind::Licence => ok(self.spdx.as_deref().is_some_and(spdx) && !self.links.is_empty()),
            Kind::Notice | Kind::Terms => ok(self.spdx.as_deref().is_none_or(spdx)),
        }
    }
}

impl Agreements {
    /// Whether this is an agreement a frontend can show: within every limit, with a licence page and a
    /// notice page, and every page saying what it is and what changes.
    pub fn validate(&self) -> Result<()> {
        ok(!self.pages.is_empty() && self.pages.len() <= MAX_PAGES)?;
        ok(self.pages.iter().any(|page| page.kind == Kind::Licence)
            && self.pages.iter().any(|page| page.kind == Kind::Notice))?;
        for page in &self.pages {
            page.check()?;
        }
        ok(serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_BYTES))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn licence() -> Page {
        Page {
            kind: Kind::Licence,
            title: "MIT licence".into(),
            explains: "You may use, copy and change this extension, and share it, if the notice stays with it.".into(),
            changes: "Nothing on your devices changes by accepting this; it is the permission to use the code.".into(),
            spdx: Some("MIT".into()),
            links: vec![Link {
                label: "Full text".into(),
                url: "https://opensource.org/license/mit".into(),
            }],
            body: vec![],
        }
    }

    fn notice() -> Page {
        Page {
            kind: Kind::Notice,
            title: "What this extension does".into(),
            explains: "It adds a page that shows the clock of each paired device.".into(),
            changes:
                "A page appears in the application; it asks for no permission until you grant one."
                    .into(),
            spdx: None,
            links: vec![],
            body: vec![
                Block::Heading {
                    text: "Uses".into(),
                },
                Block::List {
                    items: vec!["chrono, MIT or Apache-2.0".into()],
                    ordered: false,
                },
                Block::Callout {
                    tone: Tone::Warning,
                    text: "It reads the names of your paired devices.".into(),
                },
                Block::Table {
                    header: vec!["Thing".into(), "Licence".into()],
                    rows: vec![vec!["chrono".into(), "MIT".into()]],
                },
                Block::Divider,
                Block::Link {
                    label: "Source".into(),
                    url: "https://example.org/source".into(),
                },
            ],
        }
    }

    fn agreement() -> Agreements {
        Agreements {
            pages: vec![licence(), notice()],
        }
    }

    #[test]
    fn an_agreement_with_a_licence_and_a_notice_is_accepted_and_round_trips() {
        agreement().validate().unwrap();
        let text = serde_json::to_string(&agreement()).unwrap();
        assert_eq!(
            serde_json::from_str::<Agreements>(&text).unwrap(),
            agreement()
        );
    }

    #[test]
    fn both_a_licence_and_a_notice_are_required_and_the_pages_are_bounded() {
        let mut only_licence = agreement();
        only_licence.pages.pop();
        assert_eq!(only_licence.validate(), Err(Error::InvalidManifest));
        let mut only_notice = agreement();
        only_notice.pages.remove(0);
        assert_eq!(only_notice.validate(), Err(Error::InvalidManifest));
        assert_eq!(
            Agreements { pages: vec![] }.validate(),
            Err(Error::InvalidManifest)
        );
        let mut many = agreement();
        many.pages.extend((0..MAX_PAGES).map(|_| notice()));
        assert_eq!(many.validate(), Err(Error::InvalidManifest));
        let mut full = Agreements {
            pages: vec![licence(), notice()],
        };
        full.pages.extend((0..MAX_PAGES - 2).map(|_| notice()));
        full.validate().unwrap();
    }

    #[test]
    fn every_page_must_say_what_it_is_and_what_changes() {
        for bad in ["", "   ", "bell\u{7}", &"x".repeat(MAX_SUMMARY + 1)] {
            let mut no_explanation = agreement();
            no_explanation.pages[1].explains = bad.into();
            assert_eq!(
                no_explanation.validate(),
                Err(Error::InvalidManifest),
                "{bad:?}"
            );
            let mut no_change = agreement();
            no_change.pages[0].changes = bad.into();
            assert_eq!(no_change.validate(), Err(Error::InvalidManifest), "{bad:?}");
            let mut no_title = agreement();
            no_title.pages[0].title = bad.into();
            assert_eq!(no_title.validate(), Err(Error::InvalidManifest), "{bad:?}");
        }
    }

    #[test]
    fn a_licence_page_names_its_licence_and_says_where_the_text_is() {
        let mut nameless = agreement();
        nameless.pages[0].spdx = None;
        assert_eq!(nameless.validate(), Err(Error::InvalidManifest));
        let mut odd = agreement();
        odd.pages[0].spdx = Some("MIT; rm -rf".into());
        assert_eq!(odd.validate(), Err(Error::InvalidManifest));
        let mut unlinked = agreement();
        unlinked.pages[0].links.clear();
        assert_eq!(unlinked.validate(), Err(Error::InvalidManifest));
        let mut either = agreement();
        either.pages[0].spdx = Some("Apache-2.0 OR MIT".into());
        either.validate().unwrap();
    }

    #[test]
    fn only_plain_https_addresses_are_ever_shown() {
        for bad in [
            "http://example.org/x",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:pw@example.org/",
            "https:///nohost",
            "example.org/no-scheme",
            "https://example.org/\u{7}",
            &format!("https://example.org/{}", "a".repeat(MAX_URL)),
        ] {
            assert!(!address(bad), "{bad:?}");
            let mut link = agreement();
            link.pages[0].links[0].url = bad.into();
            assert_eq!(link.validate(), Err(Error::InvalidManifest), "{bad:?}");
            let mut block = agreement();
            block.pages[1].body.push(Block::Link {
                label: "x".into(),
                url: bad.into(),
            });
            assert_eq!(block.validate(), Err(Error::InvalidManifest), "{bad:?}");
        }
        assert!(address("https://example.org/path?q=1#top"));
    }

    #[test]
    fn blocks_are_plain_content_and_cannot_carry_size_position_or_action() {
        // A block that names a size, a position, an identifier or an action is not one: unknown names are refused,
        // so no agreement can draw a button, move itself over the real ones or press them.
        for json in [
            r#"{"k":"paragraph","text":"x","width":9999}"#,
            r#"{"k":"paragraph","text":"x","at":[0,0]}"#,
            r#"{"k":"paragraph","text":"x","id":"accept"}"#,
            r#"{"k":"button","id":"accept","label":"Accept"}"#,
            r#"{"k":"toggle","id":"a","label":"x","on":true}"#,
            r#"{"k":"box","style":{"overlay":true},"children":[]}"#,
            r#"{"k":"interval","id":"a","milliseconds":100,"child":{"k":"divider"}}"#,
        ] {
            assert!(serde_json::from_str::<Block>(json).is_err(), "{json}");
        }
        let page: std::result::Result<Page, _> = serde_json::from_str(
            r#"{"kind":"notice","title":"t","explains":"e","changes":"c","width":600}"#,
        );
        assert!(page.is_err());
    }

    #[test]
    fn lists_tables_and_texts_are_bounded() {
        let mut empty_list = agreement();
        empty_list.pages[1].body = vec![Block::List {
            items: vec![],
            ordered: true,
        }];
        assert_eq!(empty_list.validate(), Err(Error::InvalidManifest));
        let mut long_list = agreement();
        long_list.pages[1].body = vec![Block::List {
            items: vec!["x".into(); MAX_ITEMS + 1],
            ordered: false,
        }];
        assert_eq!(long_list.validate(), Err(Error::InvalidManifest));
        let mut ragged = agreement();
        ragged.pages[1].body = vec![Block::Table {
            header: vec!["a".into(), "b".into()],
            rows: vec![vec!["only one".into()]],
        }];
        assert_eq!(ragged.validate(), Err(Error::InvalidManifest));
        let mut wide = agreement();
        wide.pages[1].body = vec![Block::Table {
            header: vec!["h".into(); MAX_COLUMNS + 1],
            rows: vec![],
        }];
        assert_eq!(wide.validate(), Err(Error::InvalidManifest));
        let mut long_text = agreement();
        long_text.pages[1].body = vec![Block::Paragraph {
            text: "x".repeat(MAX_TEXT + 1),
            muted: false,
        }];
        assert_eq!(long_text.validate(), Err(Error::InvalidManifest));
        let mut many_blocks = agreement();
        many_blocks.pages[1].body = vec![Block::Divider; MAX_BLOCKS + 1];
        assert_eq!(many_blocks.validate(), Err(Error::InvalidManifest));
        // An empty cell is allowed: a table may have a blank where nothing applies.
        let mut blank_cell = agreement();
        blank_cell.pages[1].body = vec![Block::Table {
            header: vec!["a".into(), "b".into()],
            rows: vec![vec!["x".into(), String::new()]],
        }];
        blank_cell.validate().unwrap();
    }

    #[test]
    fn too_many_links_or_too_large_an_agreement_is_refused() {
        let mut links = agreement();
        links.pages[0].links = vec![links.pages[0].links[0].clone(); MAX_LINKS + 1];
        assert_eq!(links.validate(), Err(Error::InvalidManifest));
        let mut big = agreement();
        big.pages[1].body = (0..MAX_BLOCKS)
            .map(|_| Block::Paragraph {
                text: "y".repeat(MAX_TEXT),
                muted: false,
            })
            .collect();
        let mut huge = Agreements {
            pages: vec![licence()],
        };
        for _ in 0..MAX_PAGES - 1 {
            huge.pages.push(Page {
                body: big.pages[1].body.clone(),
                ..notice()
            });
        }
        assert_eq!(huge.validate(), Err(Error::InvalidManifest));
    }
}
