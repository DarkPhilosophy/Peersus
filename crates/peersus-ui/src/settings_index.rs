//! What one entry of Settings search is and where it leads, and the index each page of Settings adds its own entries to.
//! Kept apart from the search itself so that a page depends on the index, never on the search that asks every page.
use crate::{
    i18n::{self, Language},
    view::SettingsTab,
};
use peersus_api::permission::Permission;
use peersus_core::{model::PeerId, plugins::PluginId};

/// Stable destinations, independent of list positions and measured widget coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Group(SettingsTab),
    // Built-in controls lead to their group; dynamic records have exact item anchors.
    Plugin(PluginId),
    PluginPermissions(PluginId),
    Setting {
        plugin: PluginId,
        id: String,
        peer: Option<PeerId>,
    },
    Action {
        plugin: PluginId,
        id: String,
        peer: Option<PeerId>,
    },
    Permission {
        plugin: PluginId,
        permission: Permission,
        peer: Option<PeerId>,
    },
}
impl Target {
    pub fn tab(&self) -> SettingsTab {
        match self {
            Self::Group(tab) => *tab,
            Self::Plugin(_) | Self::Setting { .. } | Self::Action { .. } => SettingsTab::Extensions,
            Self::Permission { .. } | Self::PluginPermissions(_) => SettingsTab::Permissions,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub detail: String,
    pub target: Target,
}

/// What a page of Settings gives the search: an entry for each thing it shows, in its own words. Each page adds its own (its
/// module's `search`, or the functions in `settings_search` for the pages drawn in `lib.rs` and for extensions), and every tab
/// is asked through one exhaustive match in `settings_search::entries`: a new page cannot be left out of the search without
/// the compiler saying so.
pub struct Index {
    language: Language,
    tab: SettingsTab,
    entries: Vec<Entry>,
}

impl Index {
    /// An empty index in the person's language, adding to the first tab.
    pub(crate) fn new(language: Language) -> Self {
        Self {
            language,
            tab: SettingsTab::Overview,
            entries: Vec::new(),
        }
    }

    /// The tab what is added next belongs to.
    pub(crate) fn set_tab(&mut self, tab: SettingsTab) {
        self.tab = tab;
    }

    /// Everything added, in the order it was added.
    pub(crate) fn into_entries(self) -> Vec<Entry> {
        self.entries
    }

    /// The person's language, in which everything is looked for.
    pub fn language(&self) -> Language {
        self.language
    }

    /// An English message of the catalog in the person's language.
    pub fn tr(&self, english: &str) -> String {
        i18n::translate(self.language, english).to_owned()
    }

    /// One thing the page shows: the application's message key and the further words it may be found by. A second
    /// thing of the same title on the same page adds its words to the first.
    pub fn add(&mut self, title: &str, detail: impl Into<String>) {
        self.add_as_shown(self.tr(title), detail.into());
    }

    /// The same group entry for a title that already belongs to its display language.
    pub(crate) fn add_as_shown(&mut self, title: String, detail: String) {
        let target = Target::Group(self.tab);
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.title == title && entry.target == target)
        {
            Some(_) if detail.is_empty() => {}
            Some(entry) if entry.detail.is_empty() => entry.detail = detail,
            Some(entry) => {
                entry.detail.push_str(" · ");
                entry.detail.push_str(&detail);
            }
            None => self.entries.push(Entry {
                title,
                detail,
                target,
            }),
        }
    }

    /// A record with an exact anchor of its own (an extension, one of its settings, actions or permissions).
    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }
}
