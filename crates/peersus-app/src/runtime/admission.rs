//! Whether what a device opened may reach an extension at all, decided before anything is kept for it.
use super::*;
/// Whether a data right made by `caller` may collect a stream the application parked.
///
/// Only the application's own calls may: it is the application that set the stream aside
/// and numbered the call. The caller is the identity the hub stamped on the call, which no
/// plugin can choose or forge, unlike the call's number, which every participant picks for
/// its own calls and so can coincide.
pub(crate) fn may_collect(caller: &str) -> bool {
    caller == HOST
}

/// A device that may send a stream to a plugin, and where it is going.
#[derive(Debug)]
pub(crate) struct Admitted {
    pub(crate) plugin: PluginId,
    pub(crate) service: String,
    pub(crate) version: u32,
}

/// Decide whether `peer` may open a stream to the service `(service, version)`.
///
/// Everything that can refuse is here, in one place, before anything is kept: a stream from
/// a device that may not use the plugin is never set aside, so an unauthorised device cannot
/// make the application hold memory or a connection for it. The order is the order in which
/// the answers cost nothing: is the device known, is there a plugin for this exact service
/// and version, and did the user grant that plugin this capability for this device.
pub(crate) fn admit(
    plugins: &Registry,
    peers: &[Peer],
    peer: &PeerId,
    service: &str,
    version: u32,
) -> Result<Admitted, &'static str> {
    // The boundary that says whether a device's stream may reach a plugin at all:
    // every refusal is this application's own fixed word, never a peer's text.
    let admitted = admit_inner(plugins, peers, peer, service, version);
    peersus_diagnostics::dev_trace!(
        stage = "admit",
        outcome = if admitted.is_ok() { "ok" } else { "refused" },
        reason = match &admitted {
            Ok(_) => "ok",
            Err(reason) => *reason,
        },
        "Peer stream admission decided"
    );
    admitted
}

fn admit_inner(
    plugins: &Registry,
    peers: &[Peer],
    peer: &PeerId,
    service: &str,
    version: u32,
) -> Result<Admitted, &'static str> {
    let peer = peers
        .iter()
        .find(|candidate| &candidate.id == peer)
        .ok_or("Device is not paired")?;
    let Route::Plugin { index, capability } = plugins.route(service, version) else {
        return Err("No plugin handles this request");
    };
    let plugin = plugins.get(index).ok_or("Plugin unavailable")?.id.clone();
    if !permitted(peer, &plugin, capability) {
        return Err("Plugin is not permitted for this device");
    }
    Ok(Admitted {
        plugin,
        service: service.to_owned(),
        version,
    })
}

/// Every extension service `peer` may open a stream to now: the exact set [`admit`] would let through, for each service a stream
/// can be routed to, under the same rule (the export's receiving method and the permission its person gave this device), of the
/// extensions in `running` only (switched on and connected: one that is stopped, failing or switched off would not answer). What
/// this device tells `peer` it takes from it, and nothing else: a service whose permission was not given is not named.
pub(crate) fn accepted_from(
    plugins: &Registry,
    running: &std::collections::BTreeSet<PluginId>,
    peer: &Peer,
) -> Vec<peersus_net::streams::ServiceRef> {
    plugins
        .iter()
        .filter(|spec| running.contains(&spec.id))
        .flat_map(|spec| {
            spec.manifest.exports.iter().filter_map(move |export| {
                let member = export.methods.get(crate::plugin_host::RECEIVE_METHOD)?;
                // Routed to the first extension that exports it, as `route` does: a second one would never be reached.
                let routed = matches!(
                    plugins.route(&export.service, export.version),
                    Route::Plugin { index, .. } if plugins.get(index).is_some_and(|first| first.id == spec.id)
                );
                (routed && permitted(peer, &spec.id, member.capability.as_deref())).then(|| {
                    peersus_net::streams::ServiceRef {
                        service: export.service.clone(),
                        version: export.version,
                    }
                })
            })
        })
        .collect()
}

/// Set what a peer sent aside for call `id` and announce it to the plugin with `announce`,
/// as one step: either the item is kept, the call is on its way and it is recorded in
/// `receiving` as a receive for that plugin, or none of the three is so.
///
/// A refusal therefore leaves nothing behind. The item is dropped at once, which ends the
/// peer's stream now and not at the deadline, and no record waits for a result that a call
/// never made cannot bring: a device that keeps opening streams while none can be taken
/// grows nothing here.
pub(super) fn keep_and_announce<T>(
    parked: &mut crate::parked::Parked<T>,
    receiving: &mut HashMap<u64, PluginId>,
    id: u64,
    owner: crate::parked::Owner,
    item: T,
    now: std::time::Instant,
    announce: impl FnOnce() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let plugin = owner.plugin.clone();
    parked
        .park(id, owner, item, now, crate::parked::MAX_WAIT)
        .map_err(|_| "Too many transfers are waiting for plugins")?;
    if let Err(refusal) = announce() {
        let _ = parked.claim(id, now);
        return Err(refusal);
    }
    receiving.insert(id, plugin);
    Ok(())
}
