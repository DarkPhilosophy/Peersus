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

/// What one extension may ask of the hub. An operation that starts work (a call, a subscription, a
/// published event) spends one unit of its rate; a reply, a cancellation and an unsubscription
/// spend nothing, since they end work. The application itself is not limited: it owns the hub and
/// supervises every extension.
///
/// Operations a second, sustained, across all duties of one extension. A desktop
/// visit paced at 8 ms can alone require about 125 replays/s, before other calls.
pub const RATE: u64 = 600;
/// Operations allowed at once after a quiet time.
pub const BURST: u32 = 300;
/// Refusals with no quiet second between them after which the extension is suspended.
pub const SUSPEND_AFTER: u32 = 600;
/// How long a suspended extension is neither listened to nor called.
pub const SUSPENSION: Duration = Duration::from_secs(30);
/// An extension refused nothing for this long starts its count of refusals again.
const QUIET: Duration = Duration::from_secs(1);
/// What one operation costs, as the time in which the rate allows one.
const COST: Duration = Duration::from_nanos(1_000_000_000 / RATE);

/// What a participant has left of its rate.
struct Budget {
    /// Time saved up, at most a burst's worth; an operation spends [`COST`] of it.
    credit: Duration,
    /// When `credit` was last brought up to date.
    at: Instant,
    refused: u32,
    last_refused: Instant,
    suspended_until: Option<Instant>,
}
impl Budget {
    fn full(now: Instant) -> Self {
        Self {
            credit: COST * BURST,
            at: now,
            refused: 0,
            last_refused: now,
            suspended_until: None,
        }
    }
}

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
    /// Owner-only notices, one pending copy of each cause, independent of guest reply queues.
    missing_permissions: BTreeSet<(String, Permission)>,
    permission_wake: Arc<tokio::sync::Notify>,
    ticket: u64,
    generation: u64,
    connection: u64,
    /// What each extension has left of its rate, by id like `permissions` and for the same reason:
    /// a restart must neither hand a flooding extension a fresh allowance nor lift its suspension.
    budgets: BTreeMap<String, Budget>,
    /// Extensions suspended since the application last asked.
    suspended: BTreeSet<String>,
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

    /// Current registration of a participant, for state the host keeps only
    /// while that particular instance exists. No guest wire operation exposes it.
    pub fn registration_generation(&self, owner: &str) -> Option<u64> {
        lock(&self.0).owners.get(owner).map(|r| r.generation)
    }

    /// Current grant at the broker's coarse admission layer, for the owner to retire notices.
    pub fn has_permission(&self, caller: &str, permission: &Permission) -> bool {
        lock(&self.0).granted(caller, permission)
    }

    /// Authentic missing-grant decisions, never results submitted by an extension.
    /// Waited on by the one owner; dropping the wait does not drop any queued decisions.
    pub async fn missing_permission_notices(&self) -> BTreeSet<(String, Permission)> {
        let wake = lock(&self.0).permission_wake.clone();
        loop {
            let pending = std::mem::take(&mut lock(&self.0).missing_permissions);
            if !pending.is_empty() {
                return pending;
            }
            wake.notified().await;
        }
    }

    /// Remove a participant for good: everything [`Hub::revoke`] removes, and also what it was
    /// granted. `revoke` keeps the grants on purpose, so that a restarted process finds them where
    /// the last one left them; forgetting is for a participant that is taken away altogether, so
    /// that a different package registered under the same name afterwards starts with nothing
    /// instead of inheriting what this one was allowed to do.
    pub fn forget(&self, owner: &str) {
        let mut state = lock(&self.0);
        state.budgets.remove(owner);
        state.suspended.remove(owner);
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

    /// The extensions suspended for flooding since this was last asked, each named once, so that
    /// the application can tell the person. See [`SUSPEND_AFTER`].
    pub fn take_suspended(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.0).suspended)
            .into_iter()
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
        self.submit_bound(operation, None)
    }

    /// Publish an empty host notice only to the subscribers in an owner-selected scope.
    /// The host's manifest and normal subscription checks still apply.
    pub fn publish_host_notice(
        &self,
        service: &str,
        version: u32,
        event: &str,
        targets: &BTreeSet<&str>,
    ) -> Result<()> {
        if self.identity.owner != HOST {
            return Err(Error::Denied);
        }
        lock(&self.hub).submit(
            &self.identity,
            Operation::Publish {
                service: service.to_owned(),
                version,
                event: event.to_owned(),
                data: serde_json::json!({}),
            },
            Instant::now(),
            None,
            Some(targets),
        )
    }

    /// Submit a host call only to the provider instance the host approved.
    /// The route checks that instance under the same lock that admits the call.
    /// This is not a wire operation and does not bypass any normal call checks.
    pub fn submit_call_to_generation(&self, generation: u64, operation: Operation) -> Result<()> {
        if self.identity.owner != HOST || !matches!(&operation, Operation::Call { .. }) {
            return Err(Error::Denied);
        }
        self.submit_bound(operation, Some(generation))
    }

    fn submit_bound(&self, operation: Operation, expected_generation: Option<u64>) -> Result<()> {
        let (kind, number) = match &operation {
            Operation::Call { id, .. } => ("call", *id),
            Operation::Reply { ticket, .. } => ("reply", *ticket),
            Operation::Cancel { id } => ("cancel", *id),
            Operation::Subscribe { .. } => ("subscribe", 0),
            Operation::Unsubscribe { .. } => ("unsubscribe", 0),
            Operation::Publish { .. } => ("publish", 0),
        };
        let result = {
            let mut hub = lock(&self.hub);
            let result = hub.submit(
                &self.identity,
                operation,
                Instant::now(),
                expected_generation,
                None,
            );
            if let Err(Error::MissingPermission(permission)) = &result
                && self.identity.owner != HOST
                && hub.missing_permissions.len() < MAX_QUEUE
                && hub
                    .missing_permissions
                    .insert((self.identity.owner.clone(), permission.clone()))
            {
                hub.permission_wake.notify_one();
            }
            result
        };
        peersus_diagnostics::dev_trace!(
            stage = "broker-submit",
            operation = kind,
            number,
            connection = self.identity.connection,
            generation = self.identity.generation,
            outcome = result
                .as_ref()
                .err()
                .map_or("ok", peersus_diagnostics::refusal),
            "Broker operation completed"
        );
        result
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
        if let Error::MissingPermission(permission) = error {
            return lock(&self.hub).push(
                &self.identity,
                Delivery::Result {
                    id,
                    result: Outcome::MissingPermission {
                        permission: permission.clone(),
                    },
                },
            );
        }
        let code = match error {
            Error::Denied => "denied",
            Error::UnknownProvider | Error::Unavailable => "unavailable",
            Error::UnknownMethod => "unknown-method",
            Error::Version => "version",
            Error::Timeout => "invalid-deadline",
            Error::Oversize => "too-large",
            Error::QueueFull => "busy",
            Error::RateLimited => "rate-limited",
            _ => "refused",
        };
        let result = Outcome::error(code, "The request was refused");
        lock(&self.hub).push(&self.identity, Delivery::Result { id, result })
    }

    /// How long a transport should leave this participant unread after a rate or permission
    /// refusal: until its rate allows one more operation, or until its suspension ends. `None` when it may
    /// go on at once. What it sends meanwhile waits in its own connection, so an extension that
    /// floods is slowed down by its own writes instead of costing the application a refusal each.
    pub fn retry_after(&self) -> Option<Duration> {
        let state = lock(&self.hub);
        let budget = state.budgets.get(&self.identity.owner)?;
        let now = Instant::now();
        let wait = match budget.suspended_until {
            Some(until) => until.saturating_duration_since(now),
            None => COST.saturating_sub(budget.credit + now.saturating_duration_since(budget.at)),
        };
        (!wait.is_zero()).then_some(wait)
    }

    /// The authenticated caller instance of a pending call to the application.
    /// A delivery already taken from the queue may have been revoked before the
    /// host acts on it; unlike a name taken from that delivery, this checks the call.
    pub fn pending_caller_generation(&self, ticket: u64) -> Option<u64> {
        if self.identity.owner != HOST {
            return None;
        }
        let state = lock(&self.hub);
        let pending = state.pending.get(&ticket)?;
        (pending.provider == self.identity && pending.deadline > Instant::now())
            .then_some(pending.caller.generation)
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
        if !imported.contains(member) || !export.consumers.contains(&caller.owner) {
            return Err(Error::Denied);
        }
        if let Some(name) = member_details.capability.as_deref() {
            // The registered provider's member is the only source of the permission name.
            let permission = Permission::new(provider, name).ok_or(Error::InvalidManifest)?;
            if !self.granted(&caller.owner, &permission) {
                return Err(Error::MissingPermission(permission));
            }
        }
        self.endpoints
            .keys()
            .find(|i| i.owner == provider && i.generation == p.generation)
            .cloned()
            .ok_or(Error::Unavailable)
    }

    /// Whether `who` was granted `permission`: the permission of whoever requires it, and not
    /// merely one of the same name that `who` happens to have declared itself.
    fn granted(&self, who: &str, permission: &Permission) -> bool {
        self.permissions
            .get(who)
            .is_some_and(|granted| granted.contains(permission))
    }

    /// Whether `owner` is suspended for flooding at `now`.
    fn paused(&self, owner: &str, now: Instant) -> bool {
        self.budgets
            .get(owner)
            .and_then(|budget| budget.suspended_until)
            .is_some_and(|until| now < until)
    }

    /// Spend one unit of `caller`'s rate, or refuse. An extension that goes on being refused is
    /// suspended and named for the application to report; see [`RATE`] and [`SUSPEND_AFTER`].
    fn admit(&mut self, caller: &Identity, now: Instant, blocked: bool) -> Result<()> {
        if caller.owner == HOST {
            return Ok(());
        }
        let budget = self
            .budgets
            .entry(caller.owner.clone())
            .or_insert_with(|| Budget::full(now));
        if let Some(until) = budget.suspended_until {
            if now < until {
                return Err(Error::RateLimited);
            }
            // Served: it starts again with nothing held against it.
            *budget = Budget::full(now);
        }
        budget.credit =
            (budget.credit + now.saturating_duration_since(budget.at)).min(COST * BURST);
        budget.at = now;
        if budget.credit >= COST {
            budget.credit -= COST;
            return Ok(());
        }
        if blocked {
            // This starts no work. The transport uses retry_after to pace its refusals,
            // but never counts a missing grant toward the suspension threshold.
            return Ok(());
        }
        if now.saturating_duration_since(budget.last_refused) > QUIET {
            budget.refused = 0;
        }
        budget.refused += 1;
        budget.last_refused = now;
        if budget.refused >= SUSPEND_AFTER {
            budget.suspended_until = Some(now + SUSPENSION);
            self.suspended.insert(caller.owner.clone());
            peersus_diagnostics::dev_trace!(
                stage = "broker-suspended",
                connection = caller.connection,
                generation = caller.generation,
                "A participant that kept flooding was suspended"
            );
        }
        Err(Error::RateLimited)
    }

    fn submit(
        &mut self,
        caller: &Identity,
        operation: Operation,
        now: Instant,
        expected_generation: Option<u64>,
        publication_scope: Option<&BTreeSet<&str>>,
    ) -> Result<()> {
        self.expire(now);
        if !self.live(caller) || !self.endpoints.contains_key(caller) {
            return Err(Error::Cancelled);
        }
        let route = match &operation {
            Operation::Call {
                provider,
                service,
                version,
                method,
                ..
            } => Some(self.permission(caller, provider, service, *version, method, false)),
            Operation::Subscribe {
                provider,
                service,
                version,
                event,
            } => Some(self.permission(caller, provider, service, *version, event, true)),
            _ => None,
        };
        if route.is_some() || matches!(operation, Operation::Publish { .. }) {
            let blocked = matches!(&route, Some(Err(Error::MissingPermission(_))));
            self.admit(caller, now, blocked)?;
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
                let provider = match route.ok_or(Error::InvalidPayload)? {
                    // The instance the host approved is gone: that is said before naming a
                    // guard of the one that replaced it.
                    Err(Error::MissingPermission(_))
                        if expected_generation.is_some_and(|expected| {
                            self.owners
                                .get(&provider)
                                .is_some_and(|now| now.generation != expected)
                        }) =>
                    {
                        return Err(Error::Unavailable);
                    }
                    other => other?,
                };
                if expected_generation.is_some_and(|expected| expected != provider.generation) {
                    return Err(Error::Unavailable);
                }
                // A suspended extension is not listened to, so nothing sent to it now could be
                // answered: say so at once instead of letting the caller wait out its deadline.
                if self.paused(&provider.owner, now) {
                    return Err(Error::Unavailable);
                }
                // A call to oneself needs room for the call and for its own result.
                if provider == *caller && queued + reserved + 2 > MAX_QUEUE {
                    return Err(Error::QueueFull);
                }
                self.ticket = self.ticket.checked_add(1).ok_or(Error::QueueFull)?;
                let ticket = self.ticket;
                peersus_diagnostics::dev_trace!(
                    stage = "broker-route",
                    call = id,
                    ticket,
                    caller = caller.connection,
                    caller_generation = caller.generation,
                    provider = provider.connection,
                    provider_generation = provider.generation,
                    method = peersus_diagnostics::method(&method),
                    service_slot = self.owners.get(&provider.owner).and_then(|r| r
                        .manifest
                        .exports
                        .iter()
                        .position(|e| e.service == service)),
                    method_slot = self
                        .owners
                        .get(&provider.owner)
                        .and_then(|r| r.manifest.exports.iter().find(|e| e.service == service))
                        .and_then(|e| e.methods.keys().position(|m| m == &method)),
                    timeout_ms,
                    "Broker call admitted"
                );
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
                // Guests may report a block, but a reply never queries the declaration of
                // an unrelated provider. A foreign name has no meaning to another guest.
                let result = match result {
                    Outcome::MissingPermission { .. }
                        if pending.caller.owner != HOST && caller.owner != HOST =>
                    {
                        Outcome::NotAccepted {}
                    }
                    Outcome::MissingPermission { ref permission } if !permission.is_valid() => {
                        return Err(Error::InvalidPayload);
                    }
                    result => result,
                };
                let (target, id) = (pending.caller.clone(), pending.id);
                peersus_diagnostics::dev_trace!(
                    stage = "broker-reply",
                    ticket,
                    call = id,
                    caller = target.connection,
                    provider = caller.connection,
                    outcome = peersus_diagnostics::outcome(&result),
                    "Broker call answered"
                );
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
                provider: _,
                service,
                version,
                event,
            } => {
                let provider = route.ok_or(Error::InvalidPayload)??;
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
                            && publication_scope
                                .is_none_or(|targets| targets.contains(s.caller.owner.as_str()))
                    })
                    .map(|s| s.caller.clone())
                    .collect();
                // An event goes to every subscriber that has room for it. One that has not
                // read what it was already sent misses this event, alone: its full queue
                // costs the publisher and the other subscribers nothing. An event is a
                // notice and never the state itself, so what a subscriber missed it can
                // still ask the service for.
                for target in targets {
                    let missed = self
                        .push(
                            &target,
                            Delivery::Event {
                                provider: caller.owner.clone(),
                                service: service.clone(),
                                version,
                                event: event.clone(),
                                data: data.clone(),
                            },
                        )
                        .is_err();
                    if missed {
                        peersus_diagnostics::dev_trace!(
                            stage = "broker-event-missed",
                            publisher = caller.connection,
                            subscriber = target.connection,
                            "A subscriber with a full queue missed an event"
                        );
                    }
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
                peersus_diagnostics::dev_trace!(
                    stage = "broker-expire",
                    ticket,
                    call = p.id,
                    caller = p.caller.connection,
                    provider = p.provider.connection,
                    "Broker deadline expired"
                );
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
        peersus_diagnostics::dev_trace!(
            stage = "broker-disconnect",
            connection = identity.connection,
            generation = identity.generation,
            "Broker participant disconnected"
        );
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
                    // The caller learns immediately instead of waiting for a timeout, and learns that the provider had the call
                    // outstanding (handed to it, read or not) and ended before answering: not the same as a provider that was never there.
                    let _ = self.push_reserved(
                        &p.caller,
                        Delivery::Result {
                            id: p.id,
                            result: Outcome::error(
                                "stopped",
                                "The provider stopped before it answered",
                            ),
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
