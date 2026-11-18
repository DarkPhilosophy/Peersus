//! What an extension holds, from what the person granted it, per device or once; what it is told of devices and of other
//! extensions; and what it may send where.
use super::*;
/// What the application itself may call on the hub, derived from what the user has
/// granted devices.
///
/// The user never talks to the hub; they decide per device what a plugin may do. The
/// application can run a plugin's action only if the user allowed that capability to
/// that plugin for at least one device, so nothing is granted until they have decided.
/// Only capabilities the application will really present to that plugin are carried over,
/// those of its actions and the one guarding `receive`, and a grant made to a different
/// plugin says nothing about this one.
///
/// The result is computed from every device each time, never patched by the latest
/// change: withdrawing one device's grant must not erase what another device was given.
pub(crate) fn host_grants(plugins: &Registry, peers: &[Peer]) -> BTreeSet<Permission> {
    let mut granted = BTreeSet::new();
    for spec in plugins.iter() {
        // What the application presents to this plugin is its OWN guards (the ones on its
        // actions and on `receive`): permissions of the plugin. Each carries the plugin as
        // its owner, so a grant given to one plugin can never match another's guard that
        // happens to be named the same.
        let presented: BTreeSet<Permission> = presented_by_the_application(spec);
        for peer in peers {
            let Some(given) = peer.grants.get(&spec.id) else {
                continue;
            };
            granted.extend(presented.iter().filter(|p| given.allows(p)).cloned());
        }
    }
    granted
}

/// The guards of one plugin that the application will present when it calls the plugin: the
/// ones on its actions and the one on `receive`, each a permission OF THAT PLUGIN.
fn presented_by_the_application(spec: &PluginSpec) -> BTreeSet<Permission> {
    let owner = spec.id.as_str();
    let mut names: BTreeSet<&String> = spec
        .manifest
        .contributions
        .actions
        .iter()
        .filter_map(|action| action.capability.as_ref())
        .collect();
    names.extend(
        spec.manifest
            .contributions
            .input
            .iter()
            .map(|input| &input.capability),
    );
    names.extend(
        spec.manifest
            .exports
            .iter()
            .filter_map(|export| export.methods.get(crate::plugin_host::RECEIVE_METHOD))
            .filter_map(|member| member.capability.as_ref()),
    );
    names
        .into_iter()
        .filter_map(|name| Permission::new(owner, name.as_str()))
        .collect()
}

/// The guards of the application that this plugin asks it to enforce: the ones on the
/// methods it imports from the application, each a permission OF THE APPLICATION.
///
/// Read from the one table of what the application offers ([`host_exports`]), for exactly what the plugin imports: a method or an
/// event that is offered guarded is a permission it can be granted, and one it does not import is not, so that nobody is offered a
/// permission for something it never asked for. There is no second list to forget a guard in.
fn guarding_what_it_imports(spec: &PluginSpec) -> BTreeSet<Permission> {
    // Asked at every publish for every extension: the table for this one alone, with no registry built and its manifest not
    // checked again (it was when it was registered or read from its package).
    let offered = exports_for(&[spec]);
    spec.manifest
        .imports
        .iter()
        .filter(|import| import.provider == HOST)
        .filter_map(|import| {
            let export = offered.iter().find(|export| {
                export.service == import.service && export.version == import.version
            })?;
            Some(
                import
                    .methods
                    .iter()
                    .filter_map(|method| export.methods.get(method))
                    .chain(
                        import
                            .events
                            .iter()
                            .filter_map(|event| export.events.get(event)),
                    ),
            )
        })
        .flatten()
        .filter_map(|member| member.capability.as_deref())
        .filter_map(|capability| Permission::new(HOST, capability))
        .collect()
}

/// Whether a permission is about no device, so that it is granted to a plugin once and not
/// per device. The single definition: a permission is device-independent because the
/// application says so here, never because a plugin or a caller says so. Everything else is
/// about a device and is granted for one.
pub(super) fn needs_no_device(permission: &Permission) -> bool {
    [
        PLUGINS_RESTART_CAPABILITY,
        PLUGINS_WATCH_CAPABILITY,
        PLATFORM_READ_CAPABILITY,
        LANGUAGE_READ_CAPABILITY,
        crate::system_readings::PROCESSOR.capability,
        crate::system_readings::GRAPHICS.capability,
        crate::system_readings::MEMORY.capability,
        crate::system_readings::STORAGE.capability,
        crate::system_readings::NETWORK.capability,
        crate::system_readings::BATTERY.capability,
        crate::display_service::ARRANGE,
        crate::display_service::WATCH,
        crate::transfer_events::TRANSFERS_WATCH_CAPABILITY,
        crate::transfer_events::TRANSFERS_NAMES_CAPABILITY,
        // Statuses are about THIS device and what it says to all the others, never about one of them.
        crate::status_service::STATUS_DEFINE_CAPABILITY,
        crate::status_service::STATUS_SET_CAPABILITY,
        crate::status_service::STATUS_WATCH_CAPABILITY,
        crate::platform_services::FILES_CAPABILITY,
        crate::platform_services::HTTP_CAPABILITY,
        crate::platform_services::CLIPBOARD_READ_CAPABILITY,
        crate::platform_services::CLIPBOARD_WRITE_CAPABILITY,
        crate::platform_services::CLIPBOARD_WATCH_CAPABILITY,
        // Input is replayed on THIS computer; which device may send it is the plugin's own per-device guard.
        crate::platform_services::INPUT_CAPABILITY,
        // A notification is shown to the person at THIS device, whichever device it is about.
        crate::platform_services::NOTIFY_CAPABILITY,
        // Where THIS device is; to whom the extension then sends it is its own per-device guard.
        crate::platform_services::location::CAPABILITY,
    ]
    .contains(&permission.name.as_str())
        && permission.owner == HOST
}

/// Every permission that can be granted to `spec`, and the only definition of it.
///
/// Derived from the ROLES the plugin plays and never from what it declares for itself: its own
/// guards on what it offers, and the application's guards on what it imports from the
/// application. A name the plugin merely listed in its `capabilities` is neither, and does
/// not become a permission, or anyone's, by being listed.
pub fn grantable(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = presented_by_the_application(spec);
    all.extend(guarding_what_it_imports(spec));
    all
}

/// The part of [`grantable`] that is granted once, for no device.
pub fn grantable_without_a_device(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = grantable(spec);
    all.retain(needs_no_device);
    all
}

/// The part of [`grantable`] that is granted for one device at a time.
pub fn grantable_for_a_device(spec: &PluginSpec) -> BTreeSet<Permission> {
    let mut all = grantable(spec);
    all.retain(|permission| !needs_no_device(permission));
    all
}

/// What a window is told `spec` can be granted: exactly the owner's two sets, split as they are, and
/// each in the order of its names so that the rows do not reshuffle between snapshots. The
/// window decides nothing of its own, so the rule has one definition.
pub fn grantable_for_window(spec: &PluginSpec) -> peersus_core::model::Grantable {
    peersus_core::model::Grantable {
        per_device: grantable_for_a_device(spec).into_iter().collect(),
        outright: grantable_without_a_device(spec).into_iter().collect(),
    }
}

/// `held` with exactly one permission turned on or off and every other one left as it was.
/// Allowing what is allowed and withdrawing what is not change nothing. The only way it can
/// fail is a set that grows past what a plugin may hold.
pub(crate) fn applied(
    held: &Grants,
    permission: &Permission,
    on: bool,
) -> Result<Grants, peersus_core::plugins::PluginStateError> {
    let others = held.iter().filter(|p| *p != permission).cloned();
    if on {
        Grants::new(others.chain(std::iter::once(permission.clone())))
    } else {
        Grants::new(others)
    }
}

/// What one plugin itself may present at the hub, derived from what the user granted it.
///
/// The counterpart of [`host_grants`], for the calls a plugin makes to the application: the
/// application's own permissions that guard what the plugin imports. A permission about a
/// device is held if any device granted it, which is the coarse layer: which device a plugin
/// may actually send to is decided separately, by the grant of that one device, when it asks.
/// A permission about no device is held if it was granted to the plugin outright. The union
/// is recomputed from every source each time, so withdrawing one grant never erases another.
pub(crate) fn plugin_grants(
    spec: &PluginSpec,
    peers: &[Peer],
    standalone: &BTreeMap<PluginId, Grants>,
) -> BTreeSet<Permission> {
    let wanted = guarding_what_it_imports(spec);
    let per_device = peers
        .iter()
        .filter_map(|peer| peer.grants.get(&spec.id))
        .flat_map(|given| {
            wanted
                .iter()
                .filter(|p| !needs_no_device(p) && given.allows(p))
                .cloned()
        });
    let outright = standalone.get(&spec.id).into_iter().flat_map(|given| {
        wanted
            .iter()
            .filter(|p| needs_no_device(p) && given.allows(p))
            .cloned()
    });
    per_device.chain(outright).collect()
}

/// [`peers_listing_with`] without what devices said they take: what the tests of the rest of the listing read.
#[cfg(test)]
pub(crate) fn peers_listing(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    asks: &BTreeSet<Permission>,
    peers: &[Peer],
) -> serde_json::Value {
    peers_listing_with(aliases, plugin, asks, peers, &Taken::default())
}

/// What the devices said they take from this one, and which of those services are the asking plugin's own: what `accepts` of
/// [`peers_listing_with`] is made of. Empty, it adds nothing.
#[derive(Default)]
pub(crate) struct Taken<'a> {
    /// The services the asking plugin exports for streams from devices.
    pub(crate) own: &'a [peersus_net::streams::ServiceRef],
    /// What each connected device said it takes from this one (see `NetworkHost::peer_accepts`).
    pub(crate) by_device: Option<
        &'a std::collections::BTreeMap<
            peersus_core::model::PeerId,
            Vec<peersus_net::streams::ServiceRef>,
        >,
    >,
}

/// The answer to `peersus.peers` `list`: the paired devices this plugin was allowed something for, as it may know them.
///
/// A device the person allowed this plugin nothing for is not listed at all: a plugin learns of a device only through a
/// permission given for that device. Per device: the alias this plugin uses to name it (what `send` accepts back), the
/// name the user gave it, whether there is a connection to it (`online`, the word plugins have always read), which of the
/// application's three words is true of it now (`presence`: a device that is connected and has said it sleeps is
/// `sleeping`, which `online` alone does not tell), whether the person has allowed this plugin, for that device,
/// everything it can be allowed for a device (`allowed`, from `asks`), and `accepts`: which of the asking plugin's OWN
/// services the device said it takes from this one now (its same extension is there, running, and its person allowed
/// this device), by name; absent for a device that said nothing (an older one, or one not connected): not known. Never
/// the network key, when it was paired or last seen, a status another plugin gave it, what any other plugin was granted
/// for it or another plugin's service: a plugin that gets the key can recognise the same device in every other plugin
/// and in every later run, and nothing tells a plugin what else a device runs.
pub(crate) fn peers_listing_with(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    asks: &BTreeSet<Permission>,
    peers: &[Peer],
    taken: &Taken<'_>,
) -> serde_json::Value {
    serde_json::Value::Array(
        peers
            .iter()
            .filter_map(|peer| {
                let given = seen_by(peer, plugin)?;
                let mut entry = serde_json::json!({
                    "device": aliases.alias(plugin, &peer.id),
                    "name": peer.name,
                    "online": peer.online,
                    "presence": crate::status_service::presence_word(peer),
                    "allowed": asks.iter().all(|permission| given.allows(permission)),
                });
                if let Some(accepts) = taken.of(&peer.id) {
                    entry["accepts"] = serde_json::json!(accepts);
                }
                Some(entry)
            })
            .collect(),
    )
}

impl Taken<'_> {
    /// Which of the asking plugin's own services `device` said it takes from this one, by name; `None` when it said nothing.
    /// The one reading of what a device said, for the listing and the chooser alike: never another plugin's service.
    fn of(&self, device: &peersus_core::model::PeerId) -> Option<Vec<&str>> {
        let said = self.by_device?.get(device)?;
        Some(
            self.own
                .iter()
                .filter(|service| said.contains(service))
                .map(|service| service.service.as_str())
                .collect(),
        )
    }
}

/// The services `spec` exports for streams from devices (those with `receive`): what `accepts` is said of, and nothing else.
pub(crate) fn own_streams(spec: &PluginSpec) -> Vec<peersus_net::streams::ServiceRef> {
    spec.manifest
        .exports
        .iter()
        .filter(|export| {
            export
                .methods
                .contains_key(crate::plugin_host::RECEIVE_METHOD)
        })
        .map(|export| peersus_net::streams::ServiceRef {
            service: export.service.clone(),
            version: export.version,
        })
        .collect()
}

/// The devices a [`peersus_api::elements::Node::Devices`] of `plugin` lists, as the application fills them in: the devices it may see
/// (the same rule as `peersus.peers` `list`), each with the owner's key for it (the window draws its picture by it; the extension is
/// told only the alias of the one chosen), its name, how it is reachable, and why `pick` refuses it. Statuses are read, for `pick` and
/// `show` alike, only when `reads_statuses` (the plugin holds `status.watch`): without it a chooser that picks by status chooses
/// nothing, so a chooser can never answer a question about a status the plugin may not read.
pub(crate) fn device_entries(
    plugin: &PluginId,
    asks: &BTreeSet<Permission>,
    peers: &[Peer],
    taken: &Taken<'_>,
    reads_statuses: bool,
    pick: &peersus_api::elements::Pick,
    show: &peersus_api::elements::Show,
) -> Vec<peersus_api::elements::DeviceEntry> {
    use peersus_api::elements::{DeviceEntry, Presence, Refusal};
    peers
        .iter()
        .filter_map(|peer| {
            let given = seen_by(peer, plugin)?;
            let presence = match peer.presence() {
                peersus_core::model::Presence::Online => Presence::Online,
                peersus_core::model::Presence::Sleeping => Presence::Sleeping,
                peersus_core::model::Presence::Offline => Presence::Offline,
            };
            let shown = peer.custom_statuses();
            let refused = if !pick.presence.contains(&presence) {
                Some(Refusal::Presence)
            } else if pick.allowed && !asks.iter().all(|permission| given.allows(permission)) {
                Some(Refusal::NotAllowed)
            } else if let Some(service) = pick.accepts.as_deref()
                // Only a service of its own is asked of a device; another's is never looked up, so it is refused as not there.
                && (!taken.own.iter().any(|own| own.service == service)
                    // Not said (an older device): offered as `peersus.peers` says, by presence and permission.
                    || taken.of(&peer.id).is_some_and(|accepts| !accepts.contains(&service)))
            {
                Some(Refusal::NotThere)
            } else if let Some(wanted) = &pick.status {
                if !reads_statuses {
                    Some(Refusal::StatusNotAllowed)
                } else if !shown.iter().any(|status| {
                    status.key.plugin.as_str() == wanted.plugin
                        && wanted
                            .id
                            .as_deref()
                            .is_none_or(|id| status.key.id.as_str() == id)
                }) {
                    Some(Refusal::NoStatus)
                } else {
                    None
                }
            } else {
                None
            };
            Some(DeviceEntry {
                device: peer.id.0.clone(),
                name: peer.name.clone(),
                presence,
                statuses: if show.statuses && reads_statuses {
                    shown
                        .iter()
                        .take(peersus_api::elements::MAX_DEVICE_STATUSES)
                        .map(|status| status.label.as_str().to_owned())
                        .collect()
                } else {
                    Vec::new()
                },
                refused,
            })
        })
        .take(peersus_api::elements::MAX_DEVICES)
        .collect()
}

/// What `plugin` was granted for `peer`, when that includes seeing it (`peers.watch`): the one rule by which a plugin may
/// learn that a paired device exists. `None` for a device the person did not allow it to see.
pub(crate) fn seen_by<'a>(peer: &'a Peer, plugin: &PluginId) -> Option<&'a Grants> {
    let watch = Permission::new(HOST, PEERS_WATCH_CAPABILITY)?;
    peer.grants.get(plugin).filter(|given| given.allows(&watch))
}

/// Filter stored state through the caller's manifest. Invalid saved values fall back to
/// declared defaults; undeclared keys and other plugins' values never cross this boundary.
pub(crate) fn configuration(
    spec: &PluginSpec,
    saved: Option<&peersus_core::plugins::Values>,
    aliases: &crate::aliases::Aliases,
    peers: &[Peer],
) -> serde_json::Value {
    use peersus_api::contributions::Scope;
    let empty = peersus_core::plugins::Values::default();
    let effective = |values: &peersus_core::plugins::Values, scope| {
        spec.manifest
            .contributions
            .settings
            .iter()
            .filter(|setting| setting.scope == scope)
            .map(|setting| {
                (
                    setting.id.clone(),
                    serde_json::to_value(values.effective(setting))
                        .unwrap_or(serde_json::Value::Null),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>()
    };
    // Only the devices this plugin was allowed to see, by the same rule as `peersus.peers` `list`.
    let devices: Vec<_> = peers.iter().filter_map(|peer| {
        let given = seen_by(peer, &spec.id)?;
        Some(serde_json::json!({
            "device": aliases.alias(&spec.id, &peer.id),
            "online": peer.online,
            "settings": effective(peer.plugin_values.get(&spec.id).unwrap_or(&empty), Scope::Device),
            "permissions": given.iter().collect::<Vec<_>>(),
        }))
    }).collect();
    serde_json::json!({
        "plugin": spec.id.as_str(),
        "settings": effective(saved.unwrap_or(&empty), Scope::Global),
        "peers": devices,
    })
}

/// A bidirectional service's own guard also applies when its provider initiates a send.
/// This closes stale-config races without naming a bundled plugin or inventing grants.
pub(crate) fn authorize_service_send(
    spec: &PluginSpec,
    peer: &Peer,
    service: &str,
    version: u32,
) -> Result<(), &'static str> {
    if let Some(member) = spec
        .manifest
        .exports
        .iter()
        .find(|export| export.service == service && export.version == version)
        .and_then(|export| export.methods.get(crate::plugin_host::RECEIVE_METHOD))
        && !crate::plugin_host::permitted(peer, &spec.id, member.capability.as_deref())
    {
        return Err(SEND_REFUSED);
    }
    Ok(())
}

/// The answer to `peersus.plugins` `list`: each plugin's id and what state it is in.
///
/// Exactly those two things. Not what a plugin declares, what it may do, what was granted it,
/// or where it lives: a plugin that wants to tell whether another is up has no need of any of
/// that, and a plugin that could read it could learn what every other one is allowed. The
/// state is the fixed vocabulary the application itself uses, never anything a plugin wrote.
pub(crate) fn plugins_listing(infos: &[peersus_core::model::PluginInfo]) -> serde_json::Value {
    serde_json::Value::Array(
        infos
            .iter()
            .map(|info| plugin_entry(&info.id, info.status))
            .collect(),
    )
}

/// One plugin as `peersus.plugins` says it, in `list` and in `state-changed` alike: its id and
/// its state. A single definition, so the event can never carry more than the listing does.
pub(crate) fn plugin_entry(
    id: &PluginId,
    status: peersus_core::model::PluginStatus,
) -> serde_json::Value {
    serde_json::json!({ "id": id.as_str(), "status": status })
}

/// What a plugin asks `peersus.plugins` `restart` for: which plugin.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartRequest {
    plugin: PluginId,
}

/// Read a `restart` request exactly. Anything beyond the one field, or an id that is not an
/// identifier, is refused before the application looks at what it names.
pub(crate) fn parse_restart(args: &serde_json::Value) -> Result<PluginId, &'static str> {
    serde_json::from_value::<RestartRequest>(args.clone())
        .map(|request| request.plugin)
        .map_err(|_| "A restart names one plugin")
}

/// What a plugin is told when the application hands it a device: its alias, under `peer`.
///
/// The same name `list` gives and `send` takes, for every way a plugin can be handed a
/// device (an action the user pressed, bytes a device sent), so that one name works for
/// everything and the device's network key never reaches a plugin by any route.
pub(crate) fn peer_argument(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    device: &PeerId,
) -> serde_json::Value {
    serde_json::json!({ "peer": aliases.alias(plugin, device) })
}

/// The one thing said to a plugin whose `send` is refused, whatever the reason.
///
/// An alias that means nothing, a device that was never granted, and a device that is
/// granted but offline all read the same. A plugin told apart "no such device" from "not
/// allowed" could use `send` to find out which devices exist and what the user decided.
pub(crate) const SEND_REFUSED: &str = "That device cannot be sent to";

/// The device layer of a plugin's `send`: which device it may send to, if any.
///
/// `alias` must be one this plugin was given; the device must be one the user has paired,
/// must be reachable now, and must have granted this plugin `peers.send` itself. A grant
/// on one device says nothing about another, and what was granted to a different plugin
/// says nothing about this one. This is the decision made for each device separately; the
/// hub has already checked, coarsely, that the plugin holds the capability at all.
pub(crate) fn authorize_send<'a>(
    aliases: &crate::aliases::Aliases,
    plugin: &PluginId,
    alias: &str,
    peers: &'a [Peer],
) -> Result<&'a Peer, &'static str> {
    let id = aliases
        .resolve(plugin, alias, peers.iter().map(|peer| &peer.id))
        .ok_or(SEND_REFUSED)?;
    let peer = peers
        .iter()
        .find(|peer| &peer.id == id)
        .ok_or(SEND_REFUSED)?;
    // `send` is guarded by the APPLICATION's permission, so that is the one the device must
    // have granted this plugin. `permitted` answers a different question, about the plugin's
    // own guards, and would look up a same-named permission of the plugin instead.
    let guard = Permission::new(HOST, PEERS_SEND_CAPABILITY);
    let granted = guard.is_some_and(|guard| {
        peer.grants
            .get(plugin)
            .is_some_and(|grants| grants.allows(&guard))
    });
    if peer.online && granted {
        Ok(peer)
    } else {
        Err(SEND_REFUSED)
    }
}

/// What a plugin asks `peersus.peers` `send` for: where, and which service there.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendRequest {
    /// The alias this plugin was given for the device by `list`.
    pub(crate) device: String,
    /// The service on the device the stream is for.
    pub(crate) service: String,
    pub(crate) version: u32,
}

/// Decode the arguments of a `send`, refusing anything that is not exactly a
/// [`SendRequest`] with a service the wire would carry.
///
/// The wire's own rule decides what a service name and version may be, so the application
/// never reserves anything for a stream it could not open. Every failure reads as
/// [`SEND_REFUSED`]: what exactly was wrong with a request is not something to tell a plugin
/// that may be probing.
pub(crate) fn parse_send(args: &serde_json::Value) -> Result<SendRequest, &'static str> {
    let request: SendRequest = serde_json::from_value(args.clone()).map_err(|_| SEND_REFUSED)?;
    peersus_net::streams::Request::Plugin {
        service: request.service.clone(),
        version: request.version,
    }
    .validate()
    .map_err(|_| SEND_REFUSED)?;
    Ok(request)
}
