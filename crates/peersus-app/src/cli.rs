//! CLI syntax translates into the same validated commands as every frontend.
//!
//! Application preferences are typed flags. Plugins are reached only through four
//! generic subcommands (`plugins`, `grant`, `set`, `run`) built from the manifests in
//! the snapshot, so adding a plugin never changes this file.
use clap::{Args, Parser, Subcommand};
use peersus_core::{
    Renderer, Theme,
    config::{BindChange, RelayChange, SaveFolderChange, SettingsPatch},
    model::{Command, PeerId, PluginInfo, Snapshot},
    plugins::{Grants, PluginId, Value},
};
use peersus_plugin_api::{contributions::Kind, permission::Permission};
use std::{net::IpAddr, path::PathBuf, sync::Arc};

#[derive(Debug, Parser)]
#[command(
    name = "peersus",
    version,
    about = "Private connections. Your devices, together."
)]
pub struct Cli {
    #[command(flatten)]
    pub preferences: Preferences,
    /// Open the window despite a saved background preference.
    #[arg(long, conflicts_with = "background")]
    pub foreground: bool,
    #[command(subcommand)]
    pub action: Option<Action>,
}

#[derive(Debug, Args, Default)]
pub struct Preferences {
    /// Bind to a concrete local address, or 'auto' for system routing.
    #[arg(long, value_parser = parse_bind)]
    pub bind: Option<BindChange>,
    /// Run without a window. Use --background=false to disable the saved option.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub background: Option<bool>,
    #[arg(long)]
    pub device_name: Option<String>,
    #[arg(long, value_parser = parse_renderer)]
    pub renderer: Option<Renderer>,
    #[arg(long, value_parser = parse_theme)]
    pub theme: Option<Theme>,
    /// The one option: whether a closed bar keeps its pictures (on) or is not shown at all (off).
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub collapse_to_icons: Option<bool>,
    /// Android: whether a touch of two fingers opens the Settings overlay (on by default). Use --two-finger-settings=false to turn it off.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub two_finger_settings: Option<bool>,
    /// Accent as six RGB hex digits, optionally prefixed with '#'.
    #[arg(long, value_parser = parse_accent)]
    pub accent: Option<[u8; 3]>,
    #[arg(long)]
    pub scale: Option<f32>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub reduced_motion: Option<bool>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub allow_relay: Option<bool>,
    /// A relay of your own, such as https://relay.example.org, instead of the public ones; or 'public' to go back to them.
    /// Its access token is not an option here: a command line is visible to other programs. Set the token in Settings.
    #[arg(long, value_parser = parse_relay)]
    pub relay: Option<RelayChange>,
    /// Whether a file that is offered is saved without asking. Use --auto-save=false to be asked for every file.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub auto_save: Option<bool>,
    /// Whether touching the circle sends the chosen files at once, instead of holding it until it fills. Use --send-instantly=false to hold.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub send_instantly: Option<bool>,
    /// The folder incoming files are saved in on their own: a full path, or 'auto' for Peersus in the downloads.
    #[arg(long, value_parser = parse_save_folder)]
    pub save_folder: Option<SaveFolderChange>,
    /// How many seconds an offered file waits for an answer before it is refused on its own (5 to 86400).
    #[arg(long)]
    pub offer_timeout: Option<u32>,
}
impl Preferences {
    pub fn patch(&self) -> SettingsPatch {
        SettingsPatch {
            device_name: self.device_name.clone(),
            bind: self.bind,
            background: self.background,
            renderer: self.renderer,
            theme: self.theme,
            language: None,
            accent: self.accent,
            scale: self.scale,
            reduced_motion: self.reduced_motion,
            collapse_to_icons: self.collapse_to_icons,
            two_finger_settings: self.two_finger_settings,
            allow_relay: self.allow_relay,
            auto_save: self.auto_save,
            send_instantly: self.send_instantly,
            save_folder: self.save_folder.clone(),
            offer_timeout_seconds: self.offer_timeout,
            custom_theme: None,
            save_theme: None,
            relay: self.relay.clone(),
            relay_policy: None,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Print the saved preferences or save explicitly supplied options.
    Settings {
        #[arg(long)]
        save: bool,
    },
    /// Create a new short-lived invitation.
    Invite,
    /// Join a QR/link invitation; this does not silently remember the device.
    Join { invitation: String },
    /// Select one of the three locally displayed pairing numbers.
    Pair { request: String, number: u8 },
    /// Reject a pending pairing request.
    Reject { request: String },
    /// List remembered devices and authenticated presence.
    Peers,
    /// Show transfer history.
    History,
    /// Forget the transfers that are over (done, cancelled or failed); those still going are kept.
    ClearHistory,
    /// Offer files to a connected device.
    Send {
        peer: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Accept an offered transfer into a local directory.
    Accept {
        transfer: String,
        directory: PathBuf,
    },
    /// Cancel or reject a transfer.
    Cancel { transfer: String },
    /// Remove a device and every permission granted to plugins for it.
    Forget { peer: String },
    /// List installed plugins, their capabilities, settings and actions.
    Plugins,
    /// Show the extension store: what the maintainer approved, and where the store has got to.
    Store,
    /// Fetch the maintainer-approved extension catalog and check its signature. The application must be running. Nothing is installed.
    StoreRefresh,
    /// Download an approved extension, check it is exactly what the maintainer approved, and install it. It starts with nothing allowed.
    /// Run `store-refresh` first; the application must be running.
    StoreInstall { plugin: String },
    /// Install a plugin package from a directory named for the plugin's id. Works with or
    /// without the application running; a running one sees it after it is restarted.
    PluginInstall { source: std::path::PathBuf },
    /// Remove an installed plugin package. Works with or without the application running.
    PluginRemove { plugin: String },
    /// List installed plugin packages and whether each is as it was installed.
    PluginList,
    /// Stop one plugin's process and start a new one. The application must be running, because
    /// it is a process of the running application that is restarted.
    RestartPlugin { plugin: String },
    /// Replace what a plugin may do. With `--peer` it is for that one device; without, it is
    /// for no device, which is how permissions that are about no device are granted. No
    /// `--allow` revokes all of that kind.
    Grant {
        plugin: String,
        #[arg(long)]
        peer: Option<String>,
        #[arg(long = "allow")]
        allow: Vec<String>,
    },
    /// Set a plugin setting. Per-device settings need `--peer`.
    Set {
        plugin: String,
        setting: String,
        value: String,
        #[arg(long)]
        peer: Option<String>,
    },
    /// Run an action a plugin declared. Device actions need `--peer`.
    Run {
        plugin: String,
        action: String,
        #[arg(long)]
        peer: Option<String>,
    },
    /// Stop the running application service.
    Stop,
}

/// Structured command output; the native entry point chooses human/IPC rendering.
/// Invitation replies intentionally have no Debug representation.
pub enum Output {
    Settings(Box<peersus_core::Settings>),
    Peers(Vec<peersus_core::model::Peer>),
    History(Vec<peersus_core::model::Transfer>),
    Plugins(Vec<PluginInfo>),
    Store(peersus_core::model::StoreState),
    Reply(crate::runtime::Reply),
}

/// Both in-process frontends and local IPC clients execute the same CLI commands.
pub trait Control {
    fn state(&self) -> impl Future<Output = Result<Arc<Snapshot>, String>> + Send;
    fn command(
        &self,
        command: Command,
    ) -> impl Future<Output = Result<crate::runtime::Reply, String>> + Send;
}
impl Control for crate::runtime::Client {
    async fn state(&self) -> Result<Arc<Snapshot>, String> {
        Ok(self.snapshot())
    }
    async fn command(&self, command: Command) -> Result<crate::runtime::Reply, String> {
        crate::runtime::Client::command(self, command)
            .await
            .map_err(str::to_owned)
    }
}

/// Interpret user text according to the type the plugin *declared*, never by guessing.
pub fn parse_value(kind: &Kind, text: &str) -> Result<Value, String> {
    match kind {
        Kind::Toggle { .. } => match text {
            "true" | "on" | "yes" => Ok(Value::Toggle(true)),
            "false" | "off" | "no" => Ok(Value::Toggle(false)),
            _ => Err("use true or false".into()),
        },
        Kind::Number { min, max, .. } => text
            .parse::<i64>()
            .map(Value::Number)
            .map_err(|_| format!("use a whole number from {min} to {max}")),
        Kind::Choice { choices, .. } => {
            if choices.iter().any(|choice| choice == text) {
                Ok(Value::Text(text.to_owned()))
            } else {
                Err(format!("choose one of: {}", choices.join(", ")))
            }
        }
        Kind::Text { .. } => Ok(Value::Text(text.to_owned())),
    }
}

fn plugin<'a>(state: &'a Snapshot, id: &str) -> Result<&'a PluginInfo, String> {
    state
        .plugins
        .iter()
        .find(|p| p.id.as_str() == id)
        .ok_or_else(|| format!("Unknown plugin: {id}"))
}

/// Read a permission as written on the command line: `owner/name`.
///
/// The owner is always written, never inferred, so that whoever grants a permission sees
/// whose it is. A slash cannot occur in an identifier, so it alone divides the two and the
/// split is never ambiguous. Whether that owner really requires that name of this plugin is
/// decided by the application, which refuses anything that is not grantable.
pub fn parse_permission(text: &str) -> Result<Permission, String> {
    let usage = "A permission is written owner/name, for example host/peers.send";
    let (owner, name) = text.split_once('/').ok_or_else(|| usage.to_owned())?;
    Permission::new(owner, name).ok_or_else(|| usage.to_owned())
}

impl Action {
    /// Whether this command works on the plugin files rather than on a running application.
    ///
    /// Those are carried out by the program itself, with or without an owner, because
    /// installing needs nothing the owner holds and must work before one has ever run.
    pub fn is_local_plugin_command(&self) -> bool {
        matches!(
            self,
            Self::PluginInstall { .. } | Self::PluginRemove { .. } | Self::PluginList
        )
    }
}

impl Cli {
    /// Execute against the running application, never a second competing store.
    /// `settings` without `--save` previews supplied preferences without mutation.
    pub async fn execute(self, client: &impl Control) -> Result<Output, String> {
        let current = client.state().await?;
        let patch = self.preferences.patch();
        let settings = patch
            .apply(&current.settings)
            .map_err(|error| error.to_string())?;
        if matches!(self.action, Some(Action::Settings { save: false })) {
            return Ok(Output::Settings(Box::new(settings)));
        }
        if patch != SettingsPatch::default()
            || matches!(self.action, Some(Action::Settings { save: true }))
        {
            client.command(Command::PatchSettings { patch }).await?;
        }
        let command = match self.action {
            None => return Ok(Output::Reply(crate::runtime::Reply::Applied)),
            Some(Action::Settings { .. }) => {
                return Ok(Output::Settings(Box::new(
                    client.state().await?.settings.clone(),
                )));
            }
            Some(Action::Peers) => return Ok(Output::Peers(client.state().await?.peers.clone())),
            Some(Action::History) => {
                return Ok(Output::History(client.state().await?.transfers.clone()));
            }
            Some(Action::Plugins) => {
                return Ok(Output::Plugins(client.state().await?.plugins.clone()));
            }
            Some(Action::Store) => return Ok(Output::Store(client.state().await?.store.clone())),
            Some(Action::StoreRefresh) => Command::StoreRefresh {},
            // Names only the plugin: what is downloaded is decided by the signed catalog, never by this command.
            Some(Action::StoreInstall { plugin: id }) => Command::StoreInstall {
                plugin: peersus_core::plugins::PluginId::new(id)
                    .map_err(|_| "That is not a valid extension name")?,
            },
            Some(Action::Invite) => Command::CreateInvitation {},
            Some(Action::Join { invitation }) => Command::JoinInvitation { invitation },
            Some(Action::Pair { request, number }) => Command::ChoosePairing { request, number },
            Some(Action::Reject { request }) => Command::RejectPairing { request },
            Some(Action::Send { peer, paths }) => Command::SendFiles {
                peer: PeerId(peer),
                paths: paths
                    .into_iter()
                    .map(std::path::absolute)
                    .collect::<Result<_, _>>()
                    .map_err(|_| "Could not resolve local file paths")?,
            },
            Some(Action::Accept {
                transfer,
                directory,
            }) => Command::AcceptTransfer {
                transfer,
                directory: std::path::absolute(directory)
                    .map_err(|_| "Could not resolve destination directory")?,
            },
            Some(Action::Cancel { transfer }) => Command::CancelTransfer { transfer },
            Some(Action::ClearHistory) => Command::ClearHistory {},
            Some(Action::Forget { peer }) => Command::ForgetPeer { peer: PeerId(peer) },
            Some(Action::Stop) => Command::Shutdown {},
            // These work on files, not on the running application, so the program carries
            // them out itself before it ever decides whether to ask an owner. Reaching here
            // would mean that did not happen.
            Some(
                Action::PluginInstall { .. } | Action::PluginRemove { .. } | Action::PluginList,
            ) => return Err("Plugin packages are managed locally, not by the application".into()),
            Some(Action::RestartPlugin { plugin: id }) => {
                let state = client.state().await?;
                let info = plugin(&state, &id)?;
                Command::RestartPlugin {
                    plugin: info.id.clone(),
                }
            }
            Some(Action::Grant {
                plugin: id,
                peer,
                allow,
            }) => {
                let state = client.state().await?;
                let info = plugin(&state, &id)?;
                Command::SetGrants {
                    plugin: info.id.clone(),
                    peer: peer.map(PeerId),
                    grants: Grants::new(
                        allow
                            .iter()
                            .map(|text| parse_permission(text))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                    .map_err(|error| error.to_string())?,
                }
            }
            Some(Action::Set {
                plugin: id,
                setting,
                value,
                peer,
            }) => {
                let state = client.state().await?;
                let info = plugin(&state, &id)?;
                let declared = info
                    .manifest
                    .contributions
                    .settings
                    .iter()
                    .find(|s| s.id == setting)
                    .ok_or_else(|| format!("Unknown setting: {setting}"))?;
                Command::SetPluginSetting {
                    plugin: info.id.clone(),
                    peer: peer.map(PeerId),
                    setting,
                    value: parse_value(&declared.kind, &value)?,
                }
            }
            Some(Action::Run {
                plugin: id,
                action,
                peer,
            }) => {
                let state = client.state().await?;
                let info = plugin(&state, &id)?;
                Command::InvokeAction {
                    plugin: PluginId::new(info.id.as_str()).map_err(|e| e.to_string())?,
                    action,
                    peer: peer.map(PeerId),
                }
            }
        };
        client.command(command).await.map(Output::Reply)
    }
}

fn parse_bind(value: &str) -> Result<BindChange, String> {
    if value == "auto" {
        return Ok(BindChange::Automatic);
    }
    value
        .parse::<IpAddr>()
        .map(BindChange::Address)
        .map_err(|_| "use a local IP address or auto".into())
}

/// A folder for incoming files, or `auto` to give the choice back to the default one (Peersus, in the downloads). Only a
/// full path: a relative one would mean a different folder depending on where the program was started.
fn parse_save_folder(value: &str) -> Result<SaveFolderChange, String> {
    if value == "auto" {
        return Ok(SaveFolderChange::Default);
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(SaveFolderChange::Folder(path))
    } else {
        Err("use a full path, or auto".into())
    }
}

/// The address of a relay of one's own (`https://` and a host, nothing more), or `public` to give the choice back to the
/// public relays. Checked by the same rule the preferences use, so the two cannot disagree about what a usable address is.
fn parse_relay(value: &str) -> Result<RelayChange, String> {
    if value == "public" {
        return Ok(RelayChange::Public);
    }
    if peersus_core::config::valid_relay_url(value) {
        Ok(RelayChange::Own(value.to_owned()))
    } else {
        Err("use https:// and a host name with nothing after it, or public".into())
    }
}
fn parse_renderer(value: &str) -> Result<Renderer, String> {
    match value {
        "auto" => Ok(Renderer::Auto),
        "gpu" => Ok(Renderer::Gpu),
        "cpu" => Ok(Renderer::Cpu),
        _ => Err("choose auto, gpu or cpu".into()),
    }
}
fn parse_theme(value: &str) -> Result<Theme, String> {
    match value {
        "system" => Ok(Theme::System),
        "dark" => Ok(Theme::Dark),
        "light" => Ok(Theme::Light),
        "custom" => Ok(Theme::Custom),
        _ => Err("choose system, dark, light or custom".into()),
    }
}
fn parse_accent(value: &str) -> Result<[u8; 3], String> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.is_ascii() {
        return Err("use six hexadecimal RGB digits".into());
    }
    let mut rgb = [0; 3];
    for (index, channel) in rgb.iter_mut().enumerate() {
        *channel = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "use six hexadecimal RGB digits")?;
    }
    Ok(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::Settings;

    #[test]
    fn background_flag_and_explicit_false_do_not_conflict_with_saved_preferences() {
        let enabled = Cli::try_parse_from([
            "peersus",
            "--background",
            "--bind",
            "127.0.0.1",
            "--allow-relay=false",
        ])
        .unwrap();
        let current = enabled
            .preferences
            .patch()
            .apply(&Settings::default())
            .unwrap();
        assert!(current.background);
        assert_eq!(current.network.bind, Some("127.0.0.1".parse().unwrap()));
        let disabled =
            Cli::try_parse_from(["peersus", "--background=false", "--bind", "auto"]).unwrap();
        let current = disabled.preferences.patch().apply(&current).unwrap();
        assert!(!current.background);
        assert_eq!(current.network.bind, None);
    }

    #[test]
    fn the_sidebar_has_one_option_written_like_the_other_switches() {
        let cli = Cli::try_parse_from(["peersus", "--collapse-to-icons=true"]).unwrap();
        let saved = cli.preferences.patch().apply(&Settings::default()).unwrap();
        assert!(saved.collapse_to_icons);
        // A bare flag means on, as for `--reduced-motion`.
        let bare = Cli::try_parse_from(["peersus", "--collapse-to-icons"]).unwrap();
        assert!(
            bare.preferences
                .patch()
                .apply(&Settings::default())
                .unwrap()
                .collapse_to_icons
        );
        // Not mentioned: whatever was saved stays.
        let cli = Cli::try_parse_from(["peersus", "--background"]).unwrap();
        let kept = Settings {
            collapse_to_icons: true,
            ..Settings::default()
        };
        assert!(
            cli.preferences
                .patch()
                .apply(&kept)
                .unwrap()
                .collapse_to_icons
        );
        // What earlier versions had is gone: it meant arrangements that do not exist.
        assert!(Cli::try_parse_from(["peersus", "--navigation", "bar"]).is_err());
        assert!(Cli::try_parse_from(["peersus", "--sidebar-open=false"]).is_err());
    }

    #[test]
    fn the_local_plugin_commands_parse_with_their_argument() {
        let install = Cli::try_parse_from(["peersus", "plugin-install", "/tmp/thing"]).unwrap();
        assert!(matches!(
            install.action,
            Some(Action::PluginInstall { ref source }) if source == std::path::Path::new("/tmp/thing")
        ));
        let remove = Cli::try_parse_from(["peersus", "plugin-remove", "thing"]).unwrap();
        assert!(matches!(
            remove.action,
            Some(Action::PluginRemove { ref plugin }) if plugin == "thing"
        ));
        let list = Cli::try_parse_from(["peersus", "plugin-list"]).unwrap();
        assert!(matches!(list.action, Some(Action::PluginList)));
    }

    #[test]
    fn a_permission_is_written_with_its_owner_and_parsed_strictly() {
        use peersus_plugin_api::permission::Permission;
        let ok = |text: &str| parse_permission(text);
        assert_eq!(
            ok("host/peers.send"),
            Ok(Permission::new("host", "peers.send").unwrap())
        );
        assert_eq!(ok("clip/go"), Ok(Permission::new("clip", "go").unwrap()));
        // Names may contain dots; a slash never occurs in an identifier, so it alone divides
        // the owner from the name and the split cannot be ambiguous.
        assert_eq!(ok("a.b/c.d"), Ok(Permission::new("a.b", "c.d").unwrap()));
    }

    #[test]
    fn a_bare_name_is_refused_because_it_does_not_say_whose_permission_it_is() {
        // The refusal says what to write instead, so it is a message and not a puzzle.
        let refusal = parse_permission("peers.send").unwrap_err();
        assert!(refusal.contains("owner/name"), "{refusal}");
        for bad in [
            "",
            "/",
            "host/",
            "/peers.send",
            "a b/c",
            "host/a b",
            "a/b/c",
            "é/x",
        ] {
            assert!(parse_permission(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn restart_takes_one_plugin_and_is_carried_out_by_the_running_application() {
        let cli = Cli::try_parse_from(["peersus", "restart-plugin", "clip"]).unwrap();
        assert!(matches!(
            cli.action,
            Some(Action::RestartPlugin { ref plugin }) if plugin == "clip"
        ));
        // It works on a RUNNING process, so unlike install it is not a local file command.
        assert!(!cli.action.unwrap().is_local_plugin_command());
        assert!(Cli::try_parse_from(["peersus", "restart-plugin"]).is_err());
        assert!(Cli::try_parse_from(["peersus", "restart-plugin", "a", "b"]).is_err());
    }

    #[test]
    fn grant_carries_whole_permissions_to_the_command() {
        let cli = Cli::try_parse_from([
            "peersus",
            "grant",
            "clip",
            "--peer",
            "peer",
            "--allow",
            "host/peers.send",
            "--allow",
            "clip/go",
        ])
        .unwrap();
        let Some(Action::Grant { allow, peer, .. }) = cli.action else {
            panic!("not a grant");
        };
        assert_eq!(allow, ["host/peers.send", "clip/go"]);
        assert_eq!(peer.as_deref(), Some("peer"));
    }

    #[test]
    fn a_grant_without_a_device_is_one_for_no_device() {
        let cli = Cli::try_parse_from([
            "peersus",
            "grant",
            "watchdog",
            "--allow",
            "host/plugins.restart",
        ])
        .unwrap();
        let Some(Action::Grant { peer, allow, .. }) = cli.action else {
            panic!("not a grant");
        };
        assert_eq!(peer, None);
        assert_eq!(allow, ["host/plugins.restart"]);
    }

    #[test]
    fn the_local_plugin_commands_need_their_argument() {
        assert!(Cli::try_parse_from(["peersus", "plugin-install"]).is_err());
        assert!(Cli::try_parse_from(["peersus", "plugin-remove"]).is_err());
        // And take no more than one.
        assert!(Cli::try_parse_from(["peersus", "plugin-install", "a", "b"]).is_err());
    }

    #[test]
    fn the_plugins_listing_from_the_owner_is_still_there_and_is_not_the_local_one() {
        let owner = Cli::try_parse_from(["peersus", "plugins"]).unwrap();
        assert!(matches!(owner.action, Some(Action::Plugins)));
    }

    #[test]
    fn only_the_plugin_commands_that_touch_files_are_local() {
        for (args, local) in [
            (vec!["peersus", "plugin-install", "x"], true),
            (vec!["peersus", "plugin-remove", "x"], true),
            (vec!["peersus", "plugin-list"], true),
            (vec!["peersus", "plugins"], false),
            (vec!["peersus", "peers"], false),
            (vec!["peersus", "stop"], false),
            (vec!["peersus", "settings"], false),
        ] {
            let action = Cli::try_parse_from(args.clone()).unwrap().action.unwrap();
            assert_eq!(action.is_local_plugin_command(), local, "{args:?}");
        }
    }

    #[test]
    fn absent_flags_keep_saved_preferences() {
        let saved = Settings {
            background: true,
            scale: 1.5,
            ..Default::default()
        };
        let cli = Cli::try_parse_from(["peersus", "peers"]).unwrap();
        assert_eq!(cli.preferences.patch().apply(&saved).unwrap(), saved);
    }

    #[test]
    fn invalid_bind_and_invalid_accent_fail_without_panicking() {
        for value in ["not-an-ip", "", "127.0.0.1:8000"] {
            assert!(Cli::try_parse_from(["peersus", "--bind", value]).is_err());
        }
        for value in ["é1234", "xxaabb", "abc"] {
            assert!(Cli::try_parse_from(["peersus", "--accent", value]).is_err());
        }
    }

    #[test]
    fn renderer_names_match_the_blitz_backends_not_the_old_toolkit() {
        for (name, expected) in [
            ("auto", Renderer::Auto),
            ("gpu", Renderer::Gpu),
            ("cpu", Renderer::Cpu),
        ] {
            let cli = Cli::try_parse_from(["peersus", "--renderer", name]).unwrap();
            assert_eq!(cli.preferences.renderer, Some(expected));
        }
        for gone in ["opengl", "vulkan", "software"] {
            assert!(Cli::try_parse_from(["peersus", "--renderer", gone]).is_err());
        }
    }

    #[test]
    fn plugin_values_are_read_by_declared_type_never_guessed() {
        let toggle = Kind::Toggle { default: false };
        let number = Kind::Number {
            min: 1,
            max: 9,
            default: 1,
        };
        let choice = Kind::Choice {
            choices: vec!["a".into(), "b".into()],
            default: "a".into(),
        };
        assert_eq!(parse_value(&toggle, "on"), Ok(Value::Toggle(true)));
        assert_eq!(parse_value(&toggle, "no"), Ok(Value::Toggle(false)));
        assert!(parse_value(&toggle, "1").is_err());
        assert_eq!(parse_value(&number, "7"), Ok(Value::Number(7)));
        assert!(parse_value(&number, "true").is_err());
        assert_eq!(parse_value(&choice, "b"), Ok(Value::Text("b".into())));
        assert!(parse_value(&choice, "z").is_err());
        // The text "true" is just text for a text setting, not a boolean.
        let text = Kind::Text {
            max_len: 8,
            default: String::new(),
        };
        assert_eq!(parse_value(&text, "true"), Ok(Value::Text("true".into())));
    }

    #[test]
    fn plugin_subcommands_are_generic_and_old_clipboard_syntax_is_gone() {
        for args in [
            vec!["peersus", "plugins"],
            vec![
                "peersus", "grant", "any", "--peer", "peer", "--allow", "x.use", "--allow",
                "x.admin",
            ],
            vec!["peersus", "grant", "any", "--peer", "peer"],
            vec!["peersus", "grant", "any", "--allow", "host/plugins.restart"],
            vec!["peersus", "grant", "any"],
            vec!["peersus", "set", "any", "setting", "on", "--peer", "p"],
            vec!["peersus", "run", "any", "act", "--peer", "p"],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }
        for gone in [
            // A device is named by `--peer`; a bare second word is no longer one, since with
            // the device optional it would be indistinguishable from a mistake.
            vec!["peersus", "grant", "any", "peer"],
            vec!["peersus", "clipboard", "peer"],
            vec!["peersus", "--clipboard"],
        ] {
            assert!(Cli::try_parse_from(gone.clone()).is_err(), "{gone:?}");
        }
    }
}
