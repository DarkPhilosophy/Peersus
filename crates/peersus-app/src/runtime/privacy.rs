//! Private mode (the `private_mode` preference): this device stays connected and tells its paired devices nothing of itself,
//! until its person starts something for one of them. What may leave, for whom and for how long is `crate::permits`; what a
//! device is told and takes from another is the network host's (`shown_to`). Here the two are joined to what the person does.
use super::*;

/// What an extension is answered when it asks to send to a device this one is not shown to.
pub(super) const PRIVATE_REFUSED: &str =
    "Private mode is on: nothing is sent to that device until you start something for it";

impl Runtime {
    /// The person started something for `peer`, through the application itself (`None`: a file or a folder sent) or through
    /// an extension (a device chosen on its page, an action run for the device, input forwarded to it). While this device
    /// keeps to itself that is what shows it to `peer`; otherwise there is nothing to open.
    pub(super) fn opened_by_person(&mut self, peer: &PeerId, holder: Option<&PluginId>) {
        if !self.network.is_private() {
            return;
        }
        self.permits
            .grant(peer, holder.cloned(), std::time::Instant::now());
        self.network.open_to(peer);
    }

    /// Whether an extension may send to `peer` now. Always, unless this device keeps to itself: then only to a device it is
    /// shown to, which only the person's act makes it. A send renews nothing: how long a device stays open is the person's
    /// (see `crate::permits`), so neither an extension nor the device it answers can hold it open.
    pub(super) fn may_send_now(&self, peer: &PeerId) -> bool {
        self.network.shown_to(peer)
    }

    /// The person did something on a page of `plugin`, or the page is open and ticking: what they opened through it stays.
    pub(super) fn person_is_on(&mut self, plugin: &PluginId) {
        if !self.permits.is_empty() {
            self.permits.renew_holder(plugin, std::time::Instant::now());
        }
    }

    /// Looked at once a second: a device nothing has been done for since the grace ran out is told nothing again.
    pub(super) fn close_idle_devices(&mut self) {
        if !self.network.is_private() || self.permits.is_empty() {
            return;
        }
        let sending: Vec<PeerId> = self
            .network
            .transfers()
            .into_iter()
            .filter(|transfer| !transfer.incoming && transfer.state.is_active())
            .map(|transfer| transfer.peer)
            .collect();
        let (network, carrying_out) = (&self.network, &self.carrying_out);
        let lapsed = self
            .permits
            .expire(std::time::Instant::now(), |peer, holder| match holder {
                None => sending.contains(peer) || network.sending_folder_to(peer),
                Some(plugin) => carrying_out.any_for(peer, plugin),
            });
        for peer in lapsed {
            self.close_device(&peer);
        }
    }

    /// `peer` is told that this device says nothing more, and what it had started here while it was shown this one ends.
    fn close_device(&mut self, peer: &PeerId) {
        self.network.close_to(peer);
        let ids = self.parked.drop_for(peer, None);
        self.carrying.stop_for(peer, None);
        // And what any extension was still sending to it: another one may have sent while the device was open, and a
        // device told that this one says nothing more must receive nothing more.
        self.outbound.drop_for(peer, None);
        self.carrying_out.stop_for(peer, None);
        self.cancel_calls(ids);
    }

    /// The preference as it is now, applied: at start and after every change of the preferences. Turning the mode on ends
    /// what extensions were carrying to and from every device, since none of it was started under it.
    pub(super) fn apply_private_mode(&mut self) {
        let private = self.settings.current().private_mode;
        if private == self.network.is_private() {
            return;
        }
        self.permits.clear();
        self.network.set_private(private);
        if private {
            for peer in self.network.peers() {
                let ids = self.parked.drop_for(&peer.id, None);
                self.carrying.stop_for(&peer.id, None);
                self.outbound.drop_for(&peer.id, None);
                self.carrying_out.stop_for(&peer.id, None);
                self.cancel_calls(ids);
            }
        }
    }

    /// An extension stopped or was removed: what the person opened through it closes with it.
    pub(super) fn forget_permits(&mut self, plugin: &PluginId) {
        for peer in self.permits.forget_holder(plugin) {
            self.close_device(&peer);
        }
    }
}
