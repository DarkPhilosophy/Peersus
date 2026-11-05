//! The application is the only hub: plugins never hold an address to each other.
//!
//! Every participant owns exactly one [`Channel`]. The identity of the sender is fixed
//! when the channel is connected and is stamped on every message by the hub, so a
//! plugin can neither claim to be another plugin nor be mistaken for the host. A call
//! from plugin A to plugin B is a message A hands to the hub; the hub checks the
//! contracts both sides declared, then delivers it to B with a ticket.
//!
//! Exporting an API is optional and chosen by the plugin author: an export names the
//! participants that may use it, and an empty list means private. The user's grant of
//! the required capability is checked on top, so authors open an API and users approve
//! its use. Queues are bounded, calls expire, and a new generation of a plugin makes
//! every channel of the previous one unusable.
//!
//! The hub does no I/O and starts no threads; a transport (see [`crate::channel`])
//! carries messages between a plugin process and its [`Channel`].
pub use peersus_api::HOST;
use peersus_api::{
    Error, Result,
    manifest::Manifest,
    message::{Delivery, Operation, Outcome, bounded},
    permission::Permission,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::{Duration, Instant},
};

pub const MAX_ENDPOINTS: usize = 128;
pub const MAX_QUEUE: usize = 64;
pub const MAX_PENDING: usize = 256;
pub const MAX_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    owner: String,
    generation: u64,
    connection: u64,
}

struct Endpoint {
    queue: VecDeque<Delivery>,
    wake: Arc<tokio::sync::Notify>,
}
struct Pending {
    caller: Identity,
    provider: Identity,
    id: u64,
    deadline: Instant,
    /// The one data connection a call may have has already been handed out.
    data_taken: bool,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Subscription {
    caller: Identity,
    provider: Identity,
    service: String,
    version: u32,
    event: String,
}
struct Registered {
    generation: u64,
    manifest: Manifest,
    /// Trusted host-only overrides, never populated from an extension manifest.
    method_timeouts: BTreeMap<(String, u32, String), Duration>,
}

/// Shared handle to the single hub state.
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    owners: BTreeMap<String, Registered>,
    /// What the user granted each participant, as permissions of WHOEVER REQUIRES THEM, by id.
    /// Deliberately separate from `owners`: it describes a decision about a participant, not
    /// about one running generation of it, so a restart or re-registration must not reset it.
    /// A grant names the provider it is a permission of, so a participant's own permission of
    /// the same name is a different thing and satisfies nothing here.
    permissions: BTreeMap<String, BTreeSet<Permission>>,
    endpoints: BTreeMap<Identity, Endpoint>,
    pending: BTreeMap<u64, Pending>,
    subscriptions: BTreeSet<Subscription>,
    ticket: u64,
    generation: u64,
    connection: u64,
}

/// A participant's only way to talk to the rest of the system.
#[derive(Clone)]
pub struct Channel {
    hub: Arc<Mutex<State>>,
    identity: Identity,
    wake: Arc<tokio::sync::Notify>,
    _lease: Arc<Lease>,
}

/// Permission to attach one data connection to one accepted call.
///
/// Its fields are private and there is no constructor, so the only way to hold one is to
/// have been given it by [`Channel::open_data`]. Moving bytes therefore always traces back
/// to a call the hub admitted.
#[derive(Debug)]
pub struct DataRight {
    ticket: u64,
    caller: String,
    call_id: u64,
}
impl DataRight {
    /// The call this right belongs to.
    pub fn ticket(&self) -> u64 {
        self.ticket
    }
    /// Who made the call, as stamped by the hub.
    pub fn caller(&self) -> &str {
        &self.caller
    }
    /// The id the caller gave this call. A caller that set something aside for the call, as
    /// the application does with a peer's network stream, knows it by this number: the
    /// hub's ticket is private to the hub and the provider, and the caller never sees it.
    pub fn call_id(&self) -> u64 {
        self.call_id
    }
}
struct Lease {
    hub: Weak<Mutex<State>>,
    identity: Identity,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            lock(&hub).disconnect(&self.identity);
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A poisoned lock only means another thread panicked; the data is still consistent
    // because every mutation completes before it can fail.
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    /// Register (or replace) a plugin. Replacing revokes every channel of the previous
    /// generation, so a restarted plugin starts from nothing.
    ///
    /// The application's own id is reserved: whoever registers a plugin picks its id, and
    /// a plugin that could be given the application's id would inherit everything the
    /// application may do. Use [`Hub::register_host`] for the application itself.
    pub fn register(
        &self,
        owner: &str,
        manifest: Manifest,
        grants: BTreeSet<Permission>,
    ) -> Result<u64> {
        if owner == HOST {
            return Err(Error::InvalidManifest);
        }
        self.register_checked(owner, manifest, grants)
    }

    /// Register the application itself, under the reserved id and under it only.
    ///
    /// It is an ordinary participant with an ordinary manifest: it may export a narrow
    /// service so plugins can ask it for things, but only to plugins its manifest names,
    /// and a plugin still has to import that service and be granted its capability. It
    /// has no shortcut around the checks every other participant goes through.
    pub fn register_host(
        &self,
        owner: &str,
        manifest: Manifest,
        grants: BTreeSet<Permission>,
    ) -> Result<u64> {
        if owner != HOST {
            return Err(Error::InvalidManifest);
        }
        self.register_checked(owner, manifest, grants)
    }

    fn register_checked(
        &self,
        owner: &str,
        manifest: Manifest,
        grants: BTreeSet<Permission>,
    ) -> Result<u64> {
        manifest.validate()?;
        if !peersus_api::name::valid(owner)
            || !grants
                .iter()
                .all(|g| g.is_valid() && Self::may_import(&manifest, &g.owner))
        {
            return Err(Error::InvalidManifest);
        }
        let mut state = lock(&self.0);
        state.revoke(owner);
        state.generation += 1;
        let generation = state.generation;
        // The `grants` argument only seeds a participant the user has never decided about.
        // Once a decision exists it is kept across restarts, so a crash can never hand back
        // access the user withdrew. It follows that granting something the new manifest just
        // gained needs an explicit `set_permissions`; re-registering does not apply it.
        let kept = state.permissions.entry(owner.to_owned()).or_insert(grants);
        // A new manifest may import fewer providers than before. Keep only grants that still
        // mean something, so stale entries never accumulate.
        kept.retain(|g| Self::may_import(&manifest, &g.owner));
        state.owners.insert(
            owner.to_owned(),
            Registered {
                generation,
                manifest,
                method_timeouts: BTreeMap::new(),
            },
        );
        Ok(generation)
    }

    /// Whether `manifest` may be granted permissions of `provider`: it imports that provider,
    /// or the provider is the application itself.
    fn may_import(manifest: &Manifest, provider: &str) -> bool {
        provider == HOST || manifest.imports.iter().any(|i| i.provider == provider)
    }

    /// Replace what the user granted a participant, as permissions of whoever requires them.
    ///
    /// A decision is about the participant and not about one of its processes, so it is
    /// recorded whether or not one is registered right now. A plugin that has just crashed is
    /// between two processes for as long as its supervisor waits, and a withdrawal made then
    /// must be as binding as any other: if it were refused, the next process would start with
    /// the access the user had just taken away.
    ///
    /// While a manifest is registered, a participant can be granted permissions of the
    /// providers it imports, and of nobody else. With none registered that is not known, so
    /// what can be checked (the name, and that each permission is well formed) is, and the
    /// rest is checked when the next process registers, which keeps only grants that still
    /// mean something. Whether a provider really declares the permission is decided when a
    /// call is made, because only then is the provider known to be there. The application
    /// sets what it may call before any plugin has started, so a grant for a provider that is
    /// not registered yet must be storable too.
    ///
    /// Every subscription is re-checked with the same rule that admitted it, so withdrawing
    /// access also ends delivery of protected events that were already subscribed, and any
    /// such event still waiting in a queue is discarded. Calls already accepted are
    /// commitments made while they were allowed and are left to finish; the change governs
    /// everything new.
    pub fn set_permissions(&self, owner: &str, grants: BTreeSet<Permission>) -> Result<()> {
        if !peersus_api::name::valid(owner) || !grants.iter().all(Permission::is_valid) {
            return Err(Error::Denied);
        }
        let mut state = lock(&self.0);
        if let Some(registered) = state.owners.get(owner)
            && !grants
                .iter()
                .all(|g| Self::may_import(&registered.manifest, &g.owner))
        {
            return Err(Error::Denied);
        }
        state.permissions.insert(owner.to_owned(), grants);
        state.drop_unpermitted_subscriptions();
        Ok(())
    }

    /// Remove a participant and every connection, call and subscription it had.
    pub fn revoke(&self, owner: &str) {
        lock(&self.0).revoke(owner);
    }

    /// Remove a participant for good: everything [`Hub::revoke`] removes, and also what it was
    /// granted. `revoke` keeps the grants on purpose, so that a restarted process finds them where
    /// the last one left them; forgetting is for a participant that is taken away altogether, so
    /// that a different package registered under the same name afterwards starts with nothing
    /// instead of inheriting what this one was allowed to do.
    pub fn forget(&self, owner: &str) {
        let mut state = lock(&self.0);
        state.revoke(owner);
        state.permissions.remove(owner);
        state.drop_unpermitted_subscriptions();
    }

    /// Open the participant's channel. One live channel per owner and generation.
    pub fn connect(&self, owner: &str, generation: u64) -> Result<Channel> {
        let mut state = lock(&self.0);
        if state
            .owners
            .get(owner)
            .is_none_or(|r| r.generation != generation)
        {
            return Err(Error::UnknownProvider);
        }
        if state
            .endpoints
            .keys()
            .any(|i| i.owner == owner && i.generation == generation)
        {
            return Err(Error::Busy);
        }
        if state.endpoints.len() >= MAX_ENDPOINTS {
            return Err(Error::QueueFull);
        }
        state.connection += 1;
        let identity = Identity {
            owner: owner.to_owned(),
            generation,
            connection: state.connection,
        };
        let wake = Arc::new(tokio::sync::Notify::new());
        state.endpoints.insert(
            identity.clone(),
            Endpoint {
                queue: VecDeque::new(),
                wake: wake.clone(),
            },
        );
        Ok(Channel {
            hub: self.0.clone(),
            _lease: Arc::new(Lease {
                hub: Arc::downgrade(&self.0),
                identity: identity.clone(),
            }),
            identity,
            wake,
        })
    }

    /// Manifests with the id the host registered them under, for generic frontends.
    pub fn manifests(&self) -> Vec<(String, Manifest)> {
        lock(&self.0)
            .owners
            .iter()
            .map(|(id, r)| (id.clone(), r.manifest.clone()))
            .collect()
    }
}

impl Channel {
    /// The owner this channel speaks for. Fixed at connect time.
    pub fn owner(&self) -> &str {
        &self.identity.owner
    }

    /// The live application may refresh its imports/consumers after package changes.
    /// Unlike registration this preserves its channel, admitted calls and unchanged
    /// subscriptions. No wire operation exposes this to an extension.
    pub fn update_host_manifest(&self, manifest: Manifest) -> Result<()> {
        if self.identity.owner != HOST {
            return Err(Error::Denied);
        }
        manifest.validate()?;
        let mut state = lock(&self.hub);
        if !state.live(&self.identity) || !state.endpoints.contains_key(&self.identity) {
            return Err(Error::Cancelled);
        }
        if let Some(grants) = state.permissions.get_mut(HOST) {
            grants.retain(|grant| Hub::may_import(&manifest, &grant.owner));
        }
        let registered = state.owners.get_mut(HOST).ok_or(Error::UnknownProvider)?;
        registered
            .method_timeouts
            .retain(|(service, version, method), _| {
                manifest.exports.iter().any(|export| {
                    &export.service == service
                        && export.version == *version
                        && export.methods.contains_key(method)
                })
            });
        registered.manifest = manifest;
        state.drop_unpermitted_subscriptions();
        Ok(())
    }

    /// Configure bounded deadlines for interactive host methods. Only the live
    /// authenticated host channel can call this; no guest wire operation exposes it.
    /// Capability, consumer, queue and cancellation checks remain unchanged.
    pub fn set_host_method_timeouts(
        &self,
        timeouts: BTreeMap<(String, u32, String), Duration>,
    ) -> Result<()> {
        if self.identity.owner != HOST {
            return Err(Error::Denied);
        }
        let mut state = lock(&self.hub);
        if !state.live(&self.identity) || !state.endpoints.contains_key(&self.identity) {
            return Err(Error::Cancelled);
        }
        let registered = state.owners.get_mut(HOST).ok_or(Error::UnknownProvider)?;
        for ((service, version, method), timeout) in &timeouts {
            if timeout.is_zero() || *timeout > Duration::from_secs(600) {
                return Err(Error::Timeout);
            }
            if !registered.manifest.exports.iter().any(|export| {
                &export.service == service
                    && export.version == *version
                    && export.methods.contains_key(method)
            }) {
                return Err(Error::UnknownMethod);
            }
        }
        registered.method_timeouts = timeouts;
        Ok(())
    }

    /// Hand one operation to the hub. Fails closed on any contract violation.
    pub fn submit(&self, operation: Operation) -> Result<()> {
        lock(&self.hub).submit(&self.identity, operation, Instant::now())
    }

    /// Tell this channel's own participant that its call `id` was refused, with a terminal
    /// result it can match like any other. Without this a refused call would simply never
    /// answer, because a refusal happens before the call is recorded and therefore before
    /// any timeout could cover it.
    ///
    /// Fails only when the participant's own queue is full or the channel is gone. A
    /// transport must treat that as fatal and drop the connection rather than leave the
    /// participant waiting for an answer that will never come.
    pub fn reject(&self, id: u64, error: &Error) -> Result<()> {
        let code = match error {
            Error::Denied => "denied",
            Error::UnknownProvider | Error::Unavailable => "unavailable",
            Error::UnknownMethod => "unknown-method",
            Error::Version => "version",
            Error::Timeout => "invalid-deadline",
            Error::Oversize => "too-large",
            Error::QueueFull => "busy",
            _ => "refused",
        };
        let result = Outcome::error(code, "The request was refused");
        lock(&self.hub).push(&self.identity, Delivery::Result { id, result })
    }

    /// Claim the one data connection of a call this participant is answering.
    ///
    /// Bulk bytes never travel in control messages, so a provider that needs them asks for
    /// a data connection. That is only possible for a call the hub already accepted and
    /// delivered to this very provider, so the grants, imports and allowlists that
    /// admitted the call also decide whether bytes may move; no second authority exists
    /// that could drift from the first. A call has one such right, taken once, and it
    /// disappears with the call: answered, cancelled, expired or revoked.
    pub fn open_data(&self, ticket: u64) -> Result<DataRight> {
        let mut state = lock(&self.hub);
        state.expire(Instant::now());
        let pending = state.pending.get_mut(&ticket).ok_or(Error::Cancelled)?;
        if pending.provider != self.identity {
            return Err(Error::Denied);
        }
        if pending.data_taken {
            return Err(Error::Busy);
        }
        pending.data_taken = true;
        Ok(DataRight {
            ticket,
            caller: pending.caller.owner.clone(),
            call_id: pending.id,
        })
    }

    /// Next pending delivery, if any. Never blocks.
    pub fn try_recv(&self) -> Result<Option<Delivery>> {
        let mut state = lock(&self.hub);
        state.expire(Instant::now());
        let endpoint = state
            .endpoints
            .get_mut(&self.identity)
            .ok_or(Error::Cancelled)?;
        Ok(endpoint.queue.pop_front())
    }

    /// Wait for the next delivery. Wakes for new work, for a deadline of a call this
    /// channel is party to, and when the channel is revoked.
    pub async fn receive(&self) -> Result<Delivery> {
        loop {
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(delivery) = self.try_recv()? {
                return Ok(delivery);
            }
            let deadline = lock(&self.hub)
                .pending
                .values()
                .filter(|p| p.caller == self.identity || p.provider == self.identity)
                .map(|p| p.deadline)
                .min();
            match deadline {
                Some(deadline) => {
                    tokio::select! {
                        () = &mut notified => {}
                        () = tokio::time::sleep_until(deadline.into()) => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    pub fn disconnect(&self) {
        lock(&self.hub).disconnect(&self.identity);
    }
}

impl State {
    fn live(&self, identity: &Identity) -> bool {
        self.owners
            .get(&identity.owner)
            .is_some_and(|r| r.generation == identity.generation)
    }

    fn reserved(&self, identity: &Identity) -> usize {
        self.pending
            .values()
            .filter(|p| p.caller == *identity)
            .count()
    }

    fn push(&mut self, identity: &Identity, delivery: Delivery) -> Result<()> {
        let reserved = self.reserved(identity);
        let endpoint = self
            .endpoints
            .get_mut(identity)
            .ok_or(Error::UnknownProvider)?;
        // Slots promised to outstanding calls are not available to events or calls.
        if endpoint.queue.len() + reserved >= MAX_QUEUE {
            return Err(Error::QueueFull);
        }
        endpoint.queue.push_back(delivery);
        endpoint.wake.notify_one();
        Ok(())
    }

    /// A result uses the slot its call reserved, so it can never be starved.
    fn push_reserved(&mut self, identity: &Identity, delivery: Delivery) -> Result<()> {
        let endpoint = self
            .endpoints
            .get_mut(identity)
            .ok_or(Error::UnknownProvider)?;
        if endpoint.queue.len() >= MAX_QUEUE {
            return Err(Error::QueueFull);
        }
        endpoint.queue.push_back(delivery);
        endpoint.wake.notify_one();
        Ok(())
    }

    /// Resolve who may be called. Identity comes from the channel, never the message.
    fn permission(
        &self,
        caller: &Identity,
        provider: &str,
        service: &str,
        version: u32,
        member: &str,
        event: bool,
    ) -> Result<Identity> {
        let c = self
            .owners
            .get(&caller.owner)
            .ok_or(Error::UnknownProvider)?;
        let p = self.owners.get(provider).ok_or(Error::UnknownProvider)?;
        let import = c
            .manifest
            .imports
            .iter()
            .find(|i| i.service == service && i.provider == provider)
            .ok_or(Error::Denied)?;
        let export = p
            .manifest
            .exports
            .iter()
            .find(|e| e.service == service)
            .ok_or(Error::UnknownMethod)?;
        if import.version != version || export.version != version {
            return Err(Error::Version);
        }
        let member_details = (if event {
            &export.events
        } else {
            &export.methods
        })
        .get(member)
        .ok_or(Error::UnknownMethod)?;
        let imported = if event {
            &import.events
        } else {
            &import.methods
        };
        if !imported.contains(member)
            || !export.consumers.contains(&caller.owner)
            || member_details.capability.as_deref().is_some_and(|name| {
                // The permission belongs to the provider of the method it guards, whatever
                // the caller may have called one of its own.
                !Permission::new(provider, name)
                    .is_some_and(|permission| self.granted(caller, &permission))
            })
        {
            return Err(Error::Denied);
        }
        self.endpoints
            .keys()
            .find(|i| i.owner == provider && i.generation == p.generation)
            .cloned()
            .ok_or(Error::Unavailable)
    }

    /// Whether `who` was granted `permission`: the permission of whoever requires it, and not
    /// merely one of the same name that `who` happens to have declared itself.
    fn granted(&self, who: &Identity, permission: &Permission) -> bool {
        self.permissions
            .get(&who.owner)
            .is_some_and(|granted| granted.contains(permission))
    }

    fn submit(&mut self, caller: &Identity, operation: Operation, now: Instant) -> Result<()> {
        self.expire(now);
        if !self.live(caller) || !self.endpoints.contains_key(caller) {
            return Err(Error::Cancelled);
        }
        if !bounded(&operation) {
            return Err(Error::Oversize);
        }
        match operation {
            Operation::Call {
                id,
                provider,
                service,
                version,
                method,
                args,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(u64::from(timeout_ms));
                // Long waits exist only for a real host invoking an explicitly declared
                // user action. Plugins cannot extend their own broker-call lifetime.
                let declared_action =
                    caller.owner == HOST
                        && self.owners.get(&provider).is_some_and(|provider| {
                            provider.manifest.exports.iter().any(|export| {
                                export.service == service && export.version == version
                            }) && provider
                                .manifest
                                .contributions
                                .actions
                                .iter()
                                .any(|action| {
                                    action.target.service == service
                                        && action.target.method == method
                                        && timeout_ms <= action.timeout_ms
                                })
                        });
                let host_method = provider == HOST
                    && self.owners.get(HOST).is_some_and(|host| {
                        host.method_timeouts
                            .get(&(service.clone(), version, method.clone()))
                            .is_some_and(|maximum| timeout <= *maximum)
                    });
                if id == 0
                    || timeout.is_zero()
                    || (timeout > MAX_TIMEOUT && !declared_action && !host_method)
                {
                    return Err(Error::Timeout);
                }
                if self.pending.len() >= MAX_PENDING
                    || self
                        .pending
                        .values()
                        .any(|p| p.caller == *caller && p.id == id)
                {
                    return Err(Error::QueueFull);
                }
                let queued = self
                    .endpoints
                    .get(caller)
                    .map_or(MAX_QUEUE, |e| e.queue.len());
                let reserved = self.reserved(caller);
                if queued + reserved >= MAX_QUEUE {
                    return Err(Error::QueueFull);
                }
                let provider =
                    self.permission(caller, &provider, &service, version, &method, false)?;
                // A call to oneself needs room for the call and for its own result.
                if provider == *caller && queued + reserved + 2 > MAX_QUEUE {
                    return Err(Error::QueueFull);
                }
                self.ticket = self.ticket.checked_add(1).ok_or(Error::QueueFull)?;
                let ticket = self.ticket;
                self.push(
                    &provider,
                    Delivery::Call {
                        ticket,
                        caller: caller.owner.clone(),
                        service,
                        version,
                        method,
                        args,
                    },
                )?;
                self.pending.insert(
                    ticket,
                    Pending {
                        caller: caller.clone(),
                        provider,
                        id,
                        deadline: now + timeout,
                        data_taken: false,
                    },
                );
                if let Some(endpoint) = self.endpoints.get(caller) {
                    endpoint.wake.notify_one();
                }
            }
            Operation::Reply { ticket, result } => {
                let pending = self.pending.get(&ticket).ok_or(Error::Cancelled)?;
                if pending.provider != *caller {
                    return Err(Error::Denied);
                }
                let (target, id) = (pending.caller.clone(), pending.id);
                self.push_reserved(&target, Delivery::Result { id, result })?;
                self.pending.remove(&ticket);
            }
            Operation::Cancel { id } => {
                let ticket = self
                    .pending
                    .iter()
                    .find(|(_, p)| p.caller == *caller && p.id == id)
                    .map(|(t, _)| *t)
                    .ok_or(Error::Cancelled)?;
                if let Some(pending) = self.pending.remove(&ticket) {
                    self.cancel_provider(&pending.provider, ticket);
                }
            }
            Operation::Subscribe {
                provider,
                service,
                version,
                event,
            } => {
                let provider =
                    self.permission(caller, &provider, &service, version, &event, true)?;
                if self.subscriptions.len() >= MAX_PENDING {
                    return Err(Error::QueueFull);
                }
                self.subscriptions.insert(Subscription {
                    caller: caller.clone(),
                    provider,
                    service,
                    version,
                    event,
                });
            }
            Operation::Unsubscribe {
                provider,
                service,
                version,
                event,
            } => {
                self.subscriptions.retain(|s| {
                    !(s.caller == *caller
                        && s.provider.owner == provider
                        && s.service == service
                        && s.version == version
                        && s.event == event)
                });
            }
            Operation::Publish {
                service,
                version,
                event,
                data,
            } => {
                let exported = self.owners.get(&caller.owner).is_some_and(|r| {
                    r.manifest.exports.iter().any(|e| {
                        e.service == service
                            && e.version == version
                            && e.events.contains_key(&event)
                    })
                });
                if !exported {
                    return Err(Error::UnknownMethod);
                }
                let targets: Vec<Identity> = self
                    .subscriptions
                    .iter()
                    .filter(|s| {
                        s.provider == *caller
                            && s.service == service
                            && s.version == version
                            && s.event == event
                    })
                    .map(|s| s.caller.clone())
                    .collect();
                // Delivery is all-or-nothing so a slow subscriber is visible, never
                // silently skipped while others receive the event.
                if targets.iter().any(|t| {
                    self.endpoints
                        .get(t)
                        .is_none_or(|e| e.queue.len() + self.reserved(t) >= MAX_QUEUE)
                }) {
                    return Err(Error::QueueFull);
                }
                for target in targets {
                    self.push(
                        &target,
                        Delivery::Event {
                            provider: caller.owner.clone(),
                            service: service.clone(),
                            version,
                            event: event.clone(),
                            data: data.clone(),
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    fn cancel_provider(&mut self, provider: &Identity, ticket: u64) {
        if let Some(endpoint) = self.endpoints.get_mut(provider) {
            // Work that never started is simply withdrawn.
            if let Some(index) = endpoint
                .queue
                .iter()
                .position(|d| matches!(d, Delivery::Call { ticket: t, .. } if *t == ticket))
            {
                endpoint.queue.remove(index);
                return;
            }
        }
        let _ = self.push(provider, Delivery::Cancelled { ticket });
    }

    fn expire(&mut self, now: Instant) {
        let tickets: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(t, _)| *t)
            .collect();
        for ticket in tickets {
            if let Some(p) = self.pending.remove(&ticket) {
                let _ = self.push_reserved(
                    &p.caller,
                    Delivery::Result {
                        id: p.id,
                        result: Outcome::error("timeout", "The request timed out"),
                    },
                );
                self.cancel_provider(&p.provider, ticket);
            }
        }
    }

    fn disconnect(&mut self, identity: &Identity) {
        if let Some(endpoint) = self.endpoints.remove(identity) {
            endpoint.wake.notify_waiters();
        }
        self.subscriptions
            .retain(|s| s.caller != *identity && s.provider != *identity);
        let tickets: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.caller == *identity || p.provider == *identity)
            .map(|(t, _)| *t)
            .collect();
        for ticket in tickets {
            if let Some(p) = self.pending.remove(&ticket) {
                if p.provider == *identity {
                    // The caller learns immediately instead of waiting for a timeout.
                    let _ = self.push_reserved(
                        &p.caller,
                        Delivery::Result {
                            id: p.id,
                            result: Outcome::error("unavailable", "The provider disconnected"),
                        },
                    );
                } else {
                    self.cancel_provider(&p.provider, ticket);
                }
            }
        }
    }

    fn revoke(&mut self, owner: &str) {
        self.owners.remove(owner);
        let identities: Vec<Identity> = self
            .endpoints
            .keys()
            .filter(|i| i.owner == owner)
            .cloned()
            .collect();
        for identity in identities {
            self.disconnect(&identity);
        }
    }

    /// Remove every subscription the current contracts and grants no longer allow, along
    /// with events already queued for them. Uses [`State::permission`] itself, so there is
    /// exactly one definition of what a participant may receive.
    fn drop_unpermitted_subscriptions(&mut self) {
        let stale: Vec<Subscription> = self
            .subscriptions
            .iter()
            .filter(|s| {
                self.permission(
                    &s.caller,
                    &s.provider.owner,
                    &s.service,
                    s.version,
                    &s.event,
                    true,
                )
                .is_err()
            })
            .cloned()
            .collect();
        for subscription in &stale {
            self.subscriptions.remove(subscription);
            if let Some(endpoint) = self.endpoints.get_mut(&subscription.caller) {
                endpoint.queue.retain(|delivery| {
                    !matches!(delivery, Delivery::Event { provider, service, version, event, .. }
                        if *provider == subscription.provider.owner
                            && *service == subscription.service
                            && *version == subscription.version
                            && *event == subscription.event)
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
