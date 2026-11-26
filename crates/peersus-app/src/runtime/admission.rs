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
    pub(crate) guard: Option<Permission>,
    pub(crate) replays_input: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    MissingPermission {
        plugin: PluginId,
        permission: Permission,
    },
    ExtensionAbsent,
    Failed(&'static str),
}

/// Identifies the inbound call before a claim and stops just its copy after a claim.
pub(super) struct Receiving {
    pub(super) owner: crate::parked::Owner,
    pub(super) guard: Option<Permission>,
    pub(super) replays_input: bool,
    pub(super) visit: Option<VisitKey>,
    /// Trusted broker ticket of the claimed receive, and its platform gate.
    pub(super) replay: Option<(u64, crate::platform_services::InputLease)>,
    pub(super) data_commit: Option<(u64, crate::extension_data::Fence)>,
    pub(super) stop: Option<oneshot::Sender<()>>,
    pub(super) parked_stop: Option<tokio::task::AbortHandle>,
}

pub(super) struct ReplayController {
    pub(super) call: u64,
    pub(super) visit: Option<VisitKey>,
    pub(super) guard: Option<Permission>,
    pub(super) lease: crate::platform_services::InputLease,
}

impl Receiving {
    pub(super) fn retire(&mut self) {
        if let Some(watch) = self.parked_stop.take() {
            watch.abort();
        }
        if let Some((_, fence)) = self.data_commit.take() {
            fence.revoke();
        }
        if let Some((_, lease)) = self.replay.take() {
            lease.revoke();
        }
    }
}

/// The declared input transport binds its receive service to platform replay, not
/// every unrelated receive from a guest that can also inject input.
pub(super) fn input_guard_for(spec: &PluginSpec, service: &str) -> Option<Permission> {
    (spec
        .manifest
        .contributions
        .input
        .as_ref()
        .is_some_and(|input| input.source.service == service)
        && spec.manifest.imports.iter().any(|import| {
            import.provider == HOST
                && import.service == crate::platform_services::INPUT_SERVICE
                && import.version == 1
                && import.methods.contains("inject")
        }))
    .then(|| Permission::new(HOST, crate::platform_services::INPUT_CAPABILITY))
    .flatten()
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
) -> Result<Admitted, Refusal> {
    // The boundary that says whether a device's stream may reach an extension at all.
    let admitted = admit_inner(plugins, peers, peer, service, version);
    peersus_diagnostics::dev_trace!(
        stage = "admit",
        outcome = if admitted.is_ok() { "ok" } else { "refused" },
        reason = match &admitted {
            Ok(_) => "ok",
            Err(Refusal::MissingPermission { .. }) => "missing-permission",
            Err(Refusal::ExtensionAbsent) => "extension-absent",
            Err(Refusal::Failed(reason)) => reason,
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
) -> Result<Admitted, Refusal> {
    let peer = peers
        .iter()
        .find(|candidate| &candidate.id == peer)
        .ok_or(Refusal::Failed("Device is not paired"))?;
    let Route::Plugin { index, capability } = plugins.route(service, version) else {
        return Err(Refusal::ExtensionAbsent);
    };
    let spec = plugins
        .get(index)
        .ok_or(Refusal::Failed("Plugin unavailable"))?;
    let plugin = spec.id.clone();
    if !permitted(peer, &plugin, capability) {
        if let Some(permission) = capability
            .filter(|name| spec.manifest.capabilities.contains_key(*name))
            .and_then(|name| Permission::new(plugin.as_str(), name))
        {
            return Err(Refusal::MissingPermission { plugin, permission });
        }
        return Err(Refusal::Failed("Plugin is not permitted for this device"));
    }
    Ok(Admitted {
        plugin,
        service: service.to_owned(),
        version,
        guard: capability.and_then(|name| Permission::new(spec.id.as_str(), name)),
        replays_input: input_guard_for(spec, service).is_some(),
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
    standalone: &BTreeMap<PluginId, Grants>,
) -> Vec<peersus_net::streams::ServiceRef> {
    plugins
        .iter()
        .filter(|spec| running.contains(&spec.id))
        .flat_map(|spec| {
            spec.manifest.exports.iter().filter_map(move |export| {
                let member = export.methods.get(crate::plugin_host::RECEIVE_METHOD)?;
                if let Some(guard) = input_guard_for(spec, &export.service)
                    && !standalone.get(&spec.id).is_some_and(|grants| grants.allows(&guard))
                {
                    return None;
                }
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
    receiving: &mut HashMap<u64, Receiving>,
    id: u64,
    owner: crate::parked::Owner,
    item: T,
    now: std::time::Instant,
    announce: impl FnOnce() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    parked
        .park(id, owner.clone(), item, now, crate::parked::MAX_WAIT)
        .map_err(|_| "Too many transfers are waiting for plugins")?;
    if let Err(refusal) = announce() {
        let _ = parked.claim(id, now);
        return Err(refusal);
    }
    receiving.insert(
        id,
        Receiving {
            owner,
            guard: None,
            replays_input: false,
            visit: None,
            replay: None,
            data_commit: None,
            stop: None,
            parked_stop: None,
        },
    );
    Ok(())
}
