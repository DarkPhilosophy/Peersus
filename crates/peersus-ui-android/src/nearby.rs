//! The phone's Bluetooth Low Energy as the radio nearness is found with (`peersus_app::nearby::Radio`; experimental).
//!
//! It translates and decides nothing: Java's `NearbyRadio` announces and listens, and what it heard waits in the bridge
//! until the owner takes it. The one thing done here that is not a translation is asking the person for the system's
//! permission, once for each time the switch is turned on, and only with the window in front.
use peersus_app::nearby::{Radio, Reading};
use peersus_core::nearby::{TAG_BYTES, Trouble};

use crate::bridge;

#[derive(Default)]
pub struct PhoneRadio {
    /// Whether the person was asked for the permission since the switch was last off.
    asked: bool,
}

/// The numbers `NearbyRadio` answers with. No answer at all (neither the service nor the activity could be asked, or
/// Java threw) is a failure like any other the system does not explain.
fn answered(code: Option<i32>) -> Result<(), Trouble> {
    match code {
        Some(0) => Ok(()),
        Some(1) => Err(Trouble::NoRadio),
        Some(2) => Err(Trouble::Off),
        Some(3) => Err(Trouble::NotAllowed),
        _ => Err(Trouble::Failed),
    }
}

impl Radio for PhoneRadio {
    async fn announce(&mut self, tag: Option<[u8; TAG_BYTES]>) -> Result<(), Trouble> {
        let result = answered(
            tokio::task::spawn_blocking(move || {
                bridge::nearby_announce(tag.as_ref().map(<[u8; TAG_BYTES]>::as_slice))
            })
            .await
            .ok()
            .flatten(),
        );
        if tag.is_none() {
            self.asked = false;
        } else if result == Err(Trouble::NotAllowed) && !self.asked && bridge::window_in_front() {
            self.asked = true;
            let _ = tokio::task::spawn_blocking(bridge::ask_nearby_permission).await;
        }
        result
    }

    async fn offer(&mut self, offer: Option<&[u8]>) -> Result<(), Trouble> {
        let offer = offer.map(<[u8]>::to_vec);
        answered(
            tokio::task::spawn_blocking(move || bridge::nearby_offer(offer.as_deref()))
                .await
                .ok()
                .flatten(),
        )
    }

    async fn listen(&mut self, on: bool) -> Result<(), Trouble> {
        answered(
            tokio::task::spawn_blocking(move || bridge::nearby_listen(on))
                .await
                .ok()
                .flatten(),
        )
    }

    fn heard(&mut self) -> Vec<Reading> {
        bridge::take_heard()
    }
}
