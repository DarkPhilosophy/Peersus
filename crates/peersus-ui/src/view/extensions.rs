//! The Extensions screen's rules: permissions, what an extension's install, staging, store and update rows say and allow, its
//! main page and actions, and the confirmations before removing one. Pure rules, no toolkit.
use super::*;

/// One row of the Permissions tab: one permission the owner says can be granted, what it lets a
/// plugin do, in words a person can read, and whether it is allowed now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRow {
    pub permission: peersus_api::permission::Permission,
    pub description: String,
    pub allowed: bool,
}

/// What a path dropped on the Extensions page is, as little as the rule needs; taken apart from the
/// file system so that the rule is tried without making files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    File { length: u64 },
    Folder,
    Other,
    Unreadable,
}

impl DropKind {
    /// What a path is, from the file system. A link is followed: a person who drops a link to a
    /// package means the package.
    pub fn of(path: &std::path::Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => Self::File { length: meta.len() },
            Ok(meta) if meta.is_dir() => Self::Folder,
            Ok(_) => Self::Other,
            Err(_) => Self::Unreadable,
        }
    }
}

/// What dropping paths on the Extensions page comes to. There is one way to install, and a package
/// reaches it as a folder or as an archive without the person saying which: the owner looks at the
/// path. So a drop is one path, taken as it is, or nothing at all with the reason in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// Nothing was dropped, so there is nothing to do and nothing to say.
    Nothing,
    /// This is the package: send it to be read and shown.
    Take(std::path::PathBuf),
    /// It cannot be one, and this is what to tell the person.
    Refuse(&'static str),
}

impl Install {
    pub const EMPTY: &'static str = "That file is empty, so there is no package in it.";
    pub const UNREADABLE: &'static str =
        "That could not be read: it is gone, or this program may not open it.";
    pub const NOT_A_PACKAGE: &'static str =
        "That is neither a folder nor a file, so it cannot be a package.";
    pub const ONE_AT_A_TIME: &'static str = "Drop one package at a time.";

    /// Judge `paths`, looking at the one that is left with `kind`.
    pub fn of_drop(
        paths: Vec<std::path::PathBuf>,
        kind: impl Fn(&std::path::Path) -> DropKind,
    ) -> Self {
        let mut distinct: Vec<std::path::PathBuf> = Vec::new();
        for path in paths {
            if !distinct.contains(&path) {
                distinct.push(path);
            }
        }
        match distinct.as_slice() {
            [] => Self::Nothing,
            [only] => match kind(only) {
                DropKind::Folder | DropKind::File { length: 1.. } => Self::Take(only.clone()),
                DropKind::File { length: 0 } => Self::Refuse(Self::EMPTY),
                DropKind::Unreadable => Self::Refuse(Self::UNREADABLE),
                DropKind::Other => Self::Refuse(Self::NOT_A_PACKAGE),
            },
            _ => Self::Refuse(Self::ONE_AT_A_TIME),
        }
    }
}

/// What the Permissions tab shows for a plugin and what it sends when a switch is flipped. Pure,
/// so that the rules are tried without a window.
pub struct PermissionRows;

impl PermissionRows {
    /// The rows for `offered`, the permissions the OWNER says `plugin` can be granted in one scope
    /// (for a device, or outright), on if the permission is in `held`. Nothing else is a row: a name
    /// the plugin merely listed in its manifest is not offered by the owner, so the window never
    /// shows a switch the owner would refuse.
    pub fn of(
        plugin: &PluginInfo,
        offered: &[peersus_api::permission::Permission],
        held: &peersus_core::plugins::Grants,
    ) -> Vec<PermissionRow> {
        Self::of_in(crate::i18n::Language::ENGLISH, plugin, offered, held)
    }

    pub fn of_in(
        language: crate::i18n::Language,
        plugin: &PluginInfo,
        offered: &[peersus_api::permission::Permission],
        held: &peersus_core::plugins::Grants,
    ) -> Vec<PermissionRow> {
        offered
            .iter()
            .map(|permission| PermissionRow {
                permission: permission.clone(),
                description: Self::words_in(language, plugin, permission),
                allowed: held.allows(permission),
            })
            .collect()
    }

    /// What a permission lets a plugin do, written by whoever owns it: the application wrote the
    /// words for its own, and the plugin wrote the words for its own, in its manifest. A permission
    /// nobody described is said to be that, and is not left blank.
    pub fn words(plugin: &PluginInfo, permission: &peersus_api::permission::Permission) -> String {
        Self::words_in(crate::i18n::Language::ENGLISH, plugin, permission)
    }

    pub fn words_in(
        language: crate::i18n::Language,
        plugin: &PluginInfo,
        permission: &peersus_api::permission::Permission,
    ) -> String {
        if permission.owner == peersus_api::HOST {
            // The application's own permissions are described where they are declared, once: asked there, so that none it offers can
            // be without words here.
            if let Some(words) = peersus_app::runtime::host_capability_description(&permission.name)
            {
                return crate::i18n::translate(language, words).to_owned();
            }
        } else if permission.owner == plugin.id.as_str()
            && let Some(words) = plugin.manifest.capabilities.get(&permission.name)
        {
            return words.clone();
        }
        crate::i18n::translate(language, "No description was given for this permission.").to_owned()
    }
}

/// What confirming a package waiting to be installed would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staging {
    /// Nothing of that name is installed.
    New,
    /// A plugin of that name is installed already, and this would take its place.
    Replaces,
}

impl Staging {
    pub fn of(staged: &PluginInfo, installed: &[PluginInfo]) -> Self {
        if installed.iter().any(|p| p.id == staged.id) {
            Self::Replaces
        } else {
            Self::New
        }
    }

    /// The word on the button that confirms.
    pub fn button(self) -> &'static str {
        match self {
            Self::New => "Install",
            Self::Replaces => "Replace",
        }
    }
}

/// What the one button on a store row does, from the version installed on this device and the version the
/// maintainer approved. Decided here and only here, so a screen never compares versions itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreAction {
    /// Not installed: one click installs it. It starts with nothing allowed.
    Install,
    /// An older version is installed and the approved one is strictly newer: one click updates it. What that costs is
    /// [`update_note`]: `peersus_core::plugins::Installation` decides whether what the old one was allowed is kept.
    Update,
    /// The approved version is installed. The only thing to do is remove it.
    Installed,
    /// A NEWER version than the approved one is installed (a development build). It is never offered as an
    /// update, and never silently downgraded: the person can still remove it.
    NewerInstalled,
}

impl StoreAction {
    fn from_versions(installed: Option<&str>, offered: &str) -> Self {
        use peersus_api::package::Version;
        let Some(installed) = installed else {
            return Self::Install;
        };
        match (Version::parse(installed), Version::parse(offered)) {
            (Some(have), Some(next)) if next > have => Self::Update,
            (Some(have), Some(next)) if next < have => Self::NewerInstalled,
            // Unknown versions are never offered as an update on a guess.
            _ => Self::Installed,
        }
    }

    fn for_installation(listing: &peersus_core::model::StoreListing, info: &PluginInfo) -> Self {
        use peersus_core::model::Standing;
        if matches!(info.standing, Standing::Broken | Standing::Changed) {
            return Self::Update;
        }
        info.package
            .as_ref()
            .map(|p| p.version.as_str())
            .or(listing.installed_version.as_deref())
            .map_or(Self::Installed, |have| {
                Self::from_versions(Some(have), &listing.version)
            })
    }

    /// The word on the button, as an English message id for `i18n::tr`. `None` for a row with no install button.
    pub fn button(&self) -> Option<&'static str> {
        match self {
            Self::Install => Some("Install"),
            Self::Update => Some("Update"),
            Self::Installed | Self::NewerInstalled => None,
        }
    }

    /// A line under the row that says what pressing the button costs, where that is not obvious and does not depend on the
    /// installed extension (an update's does: [`update_note`]). Fixed words.
    pub fn note(&self) -> Option<&'static str> {
        match self {
            Self::NewerInstalled => Some(
                "A newer version than the approved one is installed. Remove it to install the approved version.",
            ),
            Self::Install | Self::Update | Self::Installed => None,
        }
    }
}

/// What updating the installed extension `info` to the version `listing` offers costs in what it may do: the owner's rule
/// (`peersus_core::plugins::Installation`), read here, so the words never promise what the owner will not do. Fixed words.
pub fn update_note(listing: &peersus_core::model::StoreListing, info: &PluginInfo) -> &'static str {
    use peersus_core::model::Standing;
    use peersus_core::plugins::{Installation, Present};
    let installed = Present {
        from_store: info.from_store,
        sound: matches!(info.standing, Standing::Ready | Standing::Disabled),
        version: info
            .package
            .as_ref()
            .map(|p| p.version.as_str())
            .or(listing.installed_version.as_deref()),
    };
    if Installation::of(Some(installed), true, Some(&listing.version)).keeps_grants() {
        "Updating keeps what you allowed that the new version still asks for. Anything new it asks for stays off until you allow it."
    } else {
        "Updating forgets what the installed version was allowed to do. You decide again afterwards."
    }
}

/// One row of the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreRow {
    pub listing: peersus_core::model::StoreListing,
    pub action: StoreAction,
}

impl StoreRow {
    pub fn of(listing: &peersus_core::model::StoreListing) -> Self {
        let action =
            StoreAction::from_versions(listing.installed_version.as_deref(), &listing.version);
        Self {
            listing: listing.clone(),
            action,
        }
    }

    pub fn of_installed(
        listing: &peersus_core::model::StoreListing,
        installed: &PluginInfo,
    ) -> Self {
        Self {
            listing: listing.clone(),
            action: StoreAction::for_installation(listing, installed),
        }
    }
}

/// What needs the person's attention among installed extensions, said when the application opens and again
/// whenever it changes: damaged or changed packages, extensions that stopped working, and signed updates.
/// Each line names the extension as `[id]`, so the notice shows it by its installed name.
pub fn extension_health_notices(
    plugins: &[PluginInfo],
    store: &peersus_core::model::StoreState,
    language: peersus_core::config::Language,
) -> Vec<String> {
    let tr = |english| crate::i18n::translate(language, english);
    use peersus_core::model::{PluginStatus, Standing};
    let mut said = Vec::new();
    for plugin in plugins {
        let id = plugin.id.as_str();
        match (plugin.standing, plugin.status) {
            (Standing::Broken | Standing::Changed, _) => said.push(format!(
                "[{id}] {}",
                tr("Damaged or out of date: install it again to repair it")
            )),
            (_, PluginStatus::Failed) => said.push(format!(
                "[{id}] {}",
                tr("Stopped working: update or install it again")
            )),
            _ => {}
        }
    }
    for listing in &store.listings {
        if StoreAction::from_versions(listing.installed_version.as_deref(), &listing.version)
            == StoreAction::Update
        {
            said.push(format!(
                "[{}] {} {}",
                listing.plugin.as_str(),
                tr("Update available:"),
                listing.version
            ));
        }
    }
    said
}

/// One extension, whether installed locally, offered by the catalog, or both.
/// Catalog order is retained across installation/removal; local-only entries follow it. Extensions installed in this session
/// (`recent`, newest first) come before all of them, so what was just installed is the first thing seen.
pub struct ExtensionRow<'a> {
    pub id: &'a peersus_core::plugins::PluginId,
    pub installed: Option<&'a PluginInfo>,
    pub listing: Option<&'a peersus_core::model::StoreListing>,
}

/// The words at the head of a row: who made it, which version, what it does, and the size of the store's archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowHeader<'a> {
    pub author: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    /// What the store's archive weighs, said only where pressing Install or Update would download it.
    pub bytes: Option<u64>,
}

impl<'a> ExtensionRow<'a> {
    /// What is installed is what the row says: its own version, author and words, never the store's, which may be older or
    /// newer than it (a package installed by hand, or one the store has not caught up with). The store's words are for what
    /// is not installed, and its archive's size for what pressing a button would download.
    pub fn header(&self) -> RowHeader<'a> {
        let bytes = self.listing.and_then(|listing| {
            let action = self.installed.map_or(StoreAction::Install, |info| {
                StoreAction::for_installation(listing, info)
            });
            (listing.bytes > 0 && matches!(action, StoreAction::Install | StoreAction::Update))
                .then_some(listing.bytes)
        });
        if let Some(info) = self.installed {
            let package = info.package.as_ref();
            return RowHeader {
                author: package.map_or("", |p| p.author.as_str()),
                version: package.map_or_else(
                    || {
                        self.listing
                            .and_then(|l| l.installed_version.as_deref())
                            .unwrap_or("")
                    },
                    |p| p.version.as_str(),
                ),
                description: package.map_or("", |p| p.description.as_str()),
                bytes,
            };
        }
        RowHeader {
            author: self.listing.map_or("", |l| l.author.as_str()),
            version: self.listing.map_or("", |l| l.version.as_str()),
            description: self.listing.map_or("", |l| l.description.as_str()),
            bytes,
        }
    }
}

pub fn extension_rows<'a>(
    plugins: &'a [PluginInfo],
    store: &'a peersus_core::model::StoreState,
    query: &str,
    recent: &[peersus_core::plugins::PluginId],
) -> Vec<ExtensionRow<'a>> {
    let needle = query.trim().to_lowercase();
    let matches = |row: &ExtensionRow<'_>| {
        needle.is_empty()
            || row.id.as_str().to_lowercase().contains(&needle)
            || row.listing.is_some_and(|listing| {
                [&listing.author, &listing.description]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
            })
            || row
                .installed
                .and_then(|info| info.package.as_ref())
                .is_some_and(|package| {
                    [&package.name, &package.author, &package.description]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&needle))
                })
    };
    let mut rows = store
        .listings
        .iter()
        .map(|listing| ExtensionRow {
            id: &listing.plugin,
            installed: plugins.iter().find(|info| info.id == listing.plugin),
            listing: Some(listing),
        })
        .chain(
            plugins
                .iter()
                .filter(|info| {
                    !store
                        .listings
                        .iter()
                        .any(|listing| listing.plugin == info.id)
                })
                .map(|info| ExtensionRow {
                    id: &info.id,
                    installed: Some(info),
                    listing: None,
                }),
        )
        .filter(matches)
        .collect::<Vec<_>>();
    // Stable: everything not installed in this session keeps its place.
    rows.sort_by_key(|row| {
        row.installed
            .and_then(|_| recent.iter().position(|id| id == row.id))
            .unwrap_or(usize::MAX)
    });
    rows
}

/// The packages waiting for confirmation that have no row of their own in `rows`: a staged package whose name has a row is
/// shown in that row's place, so one name is never two cards (a store listing beside the same package waiting to be
/// confirmed read as a copy of it).
pub fn staged_apart<'a>(
    staged: &'a [PluginInfo],
    rows: &[ExtensionRow<'_>],
) -> Vec<&'a PluginInfo> {
    staged
        .iter()
        .filter(|package| !rows.iter().any(|row| *row.id == package.id))
        .collect()
}

/// What to say about the store as a whole, as an English message id for `i18n::tr`, from where it has got to. Fixed
/// words: what a download or a package said is never repeated.
pub fn store_notice(state: &peersus_core::model::StoreState) -> Option<&'static str> {
    use peersus_core::model::StoreStatus;
    match state.status {
        StoreStatus::NotConfigured => Some("The extension store is not set up in this build."),
        StoreStatus::Idle => Some("Press Refresh to see the extensions the maintainer approved."),
        StoreStatus::Loading => Some("Working…"),
        StoreStatus::Failed => Some("The store could not complete that. Nothing was changed."),
        StoreStatus::Ready if state.listings.is_empty() => {
            Some("The maintainer has not approved any extension yet.")
        }
        StoreStatus::Ready => None,
    }
}

/// What the person can do next about an update of the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    Check,
    Install,
    Restart,
}

impl UpdateAction {
    /// The words on its button, as an English message id for `i18n::tr`.
    pub const fn label(self) -> &'static str {
        match self {
            UpdateAction::Check => "Check for updates",
            UpdateAction::Install => "Update now",
            UpdateAction::Restart => "Restart now",
        }
    }
}

/// The Updates section: what it says and what it offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdatePanel {
    /// An English message id for `i18n::format`; `{version}` in it stands for [`Self::version`].
    pub line: &'static str,
    /// The build a signed release offers, as that release names it.
    pub version: String,
    /// What installing the offered release downloads, in bytes, while it can be pressed: zero when nothing is offered or
    /// the owner did not say.
    pub bytes: u64,
    /// The application's own sentence about why, when a step failed or this installation does not
    /// replace itself. Never anything a download said.
    pub said: Option<String>,
    /// What the publisher wrote about the offered release: signed, and shown as plain text.
    pub notes: Option<String>,
    /// The one thing that can be pressed now. `None` while a step runs: a second press would only
    /// be refused by the owner.
    pub action: Option<UpdateAction>,
    /// Whether the switches for doing it unasked mean anything for this installation.
    pub switches: bool,
}

/// The version that runs, as the owner names it, or this package's own when the owner has not said.
pub fn running_version(state: &peersus_core::model::UpdateState) -> &str {
    if state.current.is_empty() {
        env!("CARGO_PKG_VERSION")
    } else {
        &state.current
    }
}

/// Where updates of the application stand, in a word, as an English message id for `i18n::tr`. Unlike the Updates section,
/// which a build without updates does not draw, the Overview says it in every build: such a build says it has none.
pub const fn update_word(status: peersus_core::model::UpdateStatus) -> &'static str {
    use peersus_core::model::UpdateStatus;
    match status {
        UpdateStatus::NotConfigured => "not in this build",
        UpdateStatus::Unsupported => "not automatic",
        UpdateStatus::Idle => "not checked yet",
        UpdateStatus::Checking => "checking…",
        UpdateStatus::Current => "up to date",
        UpdateStatus::Available => "update available",
        UpdateStatus::Downloading | UpdateStatus::Installing => "updating…",
        UpdateStatus::Installed | UpdateStatus::Restarting => "restart to finish",
        UpdateStatus::Failed => "failed",
    }
}

/// The Updates section from where updates have got to, or `None` in a build that has no updates:
/// nothing about them is drawn there, not even a sentence saying so.
pub fn update_panel(state: &peersus_core::model::UpdateState) -> Option<UpdatePanel> {
    use peersus_core::model::UpdateStatus;
    let (line, action) = match state.status {
        UpdateStatus::NotConfigured => return None,
        UpdateStatus::Unsupported => ("This installation does not update itself.", None),
        UpdateStatus::Idle => (
            "Updates have not been looked for yet.",
            Some(UpdateAction::Check),
        ),
        UpdateStatus::Checking => ("Looking for an update…", None),
        UpdateStatus::Current => ("This version is up to date.", Some(UpdateAction::Check)),
        UpdateStatus::Available => (
            "Version {version} is available.",
            Some(UpdateAction::Install),
        ),
        UpdateStatus::Downloading => ("Downloading the update…", None),
        UpdateStatus::Installing => ("Putting the update in place…", None),
        UpdateStatus::Installed => (
            "The update is in place. It runs after a restart.",
            Some(UpdateAction::Restart),
        ),
        UpdateStatus::Restarting => ("Restarting…", None),
        // What was verified is still offered after a download or an install failed; after a
        // check failed there is nothing to install, only to look again.
        UpdateStatus::Failed => (
            "The update could not be completed.",
            Some(if state.offered.is_some() {
                UpdateAction::Install
            } else {
                UpdateAction::Check
            }),
        ),
    };
    let explains = matches!(
        state.status,
        UpdateStatus::Unsupported | UpdateStatus::Failed
    );
    Some(UpdatePanel {
        line,
        version: state.offered.clone().unwrap_or_default(),
        bytes: if action == Some(UpdateAction::Install) {
            state.offered_bytes
        } else {
            0
        },
        said: state.error.clone().filter(|_| explains),
        notes: state
            .notes
            .clone()
            .filter(|_| state.status == UpdateStatus::Available),
        action,
        switches: state.status != UpdateStatus::Unsupported,
    })
}

/// Whether the store accepts a click now: it is configured and not in the middle of something. A second click
/// while one runs would only be refused by the owner, so the button is not offered.
pub fn store_can_act(state: &peersus_core::model::StoreState) -> bool {
    use peersus_core::model::StoreStatus;
    !matches!(
        state.status,
        StoreStatus::NotConfigured | StoreStatus::Loading
    )
}

/// What the first button of a plugin does, from what its folder allows and what its process is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginMain {
    /// It may run and does: switch it off.
    Disable,
    /// It may run and does not (switched off, stopped or failed): let it run.
    Enable,
    /// Nobody approved it, or it changed since: approve it as it was shown. Carries the digest the owner reported, which is
    /// given back so that what is approved is what was displayed.
    Approve { program: String },
    /// Nothing can be done with it but remove it: it is broken, or there is nothing to approve it by.
    None,
}

/// The buttons a plugin has, from the state it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginActions {
    pub main: PluginMain,
    /// Restarting only means something for a plugin that is running.
    pub can_restart: bool,
}

impl PluginActions {
    /// Removal is always offered and is not part of this: a plugin must never be something that cannot be removed.
    pub fn for_plugin(info: &PluginInfo) -> Self {
        use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
        use peersus_core::model::Standing;
        match info.standing {
            // Nothing of it runs, so there is nothing to switch or restart. It can only be approved, and only if the owner
            // measured it: without a digest there is nothing for the person to have been shown.
            Standing::NeedsApproval | Standing::Changed => Self {
                main: info
                    .program
                    .clone()
                    .map_or(PluginMain::None, |program| PluginMain::Approve { program }),
                can_restart: false,
            },
            Standing::Broken => Self {
                main: PluginMain::None,
                can_restart: false,
            },
            Standing::Disabled => Self {
                main: PluginMain::Enable,
                can_restart: false,
            },
            Standing::Ready => match info.status {
                Running => Self {
                    main: PluginMain::Disable,
                    can_restart: true,
                },
                Starting | Restarting => Self {
                    main: PluginMain::Disable,
                    can_restart: false,
                },
                Stopped | Failed => Self {
                    main: PluginMain::Enable,
                    can_restart: false,
                },
            },
        }
    }
}

/// How a state is to be coloured: the window maps it to its own palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    /// Something the person is asked to decide.
    Ask,
    Bad,
    /// Neither working nor wrong: switched off.
    Quiet,
    /// Something that is going on now and is neither good nor bad yet: a transfer under way.
    Going,
}

/// The word on a plugin's card. What its folder allows comes first, since a plugin that is not allowed to run says so and is
/// not described as merely stopped; only for one that may run does the state of its process speak.
pub fn plugin_word(info: &PluginInfo) -> (&'static str, Tone) {
    use peersus_core::model::PluginStatus::{Failed, Restarting, Running, Starting, Stopped};
    use peersus_core::model::Standing;
    match info.standing {
        Standing::NeedsApproval => ("NEEDS APPROVAL", Tone::Ask),
        Standing::Changed => ("CHANGED", Tone::Bad),
        Standing::Broken => ("BROKEN", Tone::Bad),
        Standing::Disabled => ("OFF", Tone::Quiet),
        Standing::Ready => match info.status {
            Running => ("RUNNING", Tone::Good),
            Starting => ("STARTING", Tone::Good),
            Restarting => ("RESTARTING", Tone::Bad),
            Failed => ("FAILED", Tone::Bad),
            Stopped => ("STOPPED", Tone::Bad),
        },
    }
}

/// One sentence for the card of a plugin that does not run because of what its folder says, and nothing for the rest: why it
/// is not running and what the person can do. Fixed words: nothing read from the plugin is repeated.
pub fn plugin_note(info: &PluginInfo) -> Option<&'static str> {
    use peersus_core::model::Standing;
    match info.standing {
        Standing::NeedsApproval => Some(
            "Nobody approved this plugin, so it is not running. Approving lets it run with what it asks for, listed below.",
        ),
        Standing::Changed => Some(
            "The program of this plugin is not the one that was approved, so it is not running. Approving it again lets this program run, and what it was allowed to do is forgotten.",
        ),
        Standing::Broken => Some(
            "This plugin cannot be used: it is damaged, or its program cannot be run. It can only be removed.",
        ),
        Standing::Ready | Standing::Disabled => None,
    }
}

/// Removal asks twice, because it also forgets everything the plugin was granted: the first press
/// asks, the second, on the same plugin, does it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RemoveAsk {
    #[default]
    None,
    Asking(peersus_core::plugins::PluginId),
}

impl RemoveAsk {
    /// The state after `plugin`'s remove button is pressed, and whether to remove it now.
    pub fn pressed(self, plugin: &peersus_core::plugins::PluginId) -> (RemoveAsk, bool) {
        match self {
            RemoveAsk::Asking(asked) if &asked == plugin => (RemoveAsk::None, true),
            _ => (RemoveAsk::Asking(plugin.clone()), false),
        }
    }

    /// Given up: nothing is removed.
    pub fn cancelled(self) -> RemoveAsk {
        RemoveAsk::None
    }
}

/// An action that forgets something and has no one thing it is about (Clear) asks twice, as removal does: the first press asks,
/// the second does it. Left alone, or given up with the other button, it does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Confirm {
    #[default]
    Idle,
    Asking,
}

impl Confirm {
    /// The state after the button is pressed, and whether to do the thing now.
    pub fn pressed(self) -> (Confirm, bool) {
        match self {
            Confirm::Asking => (Confirm::Idle, true),
            Confirm::Idle => (Confirm::Asking, false),
        }
    }

    /// Given up: nothing is done.
    pub fn cancelled(self) -> Confirm {
        Confirm::Idle
    }
}
