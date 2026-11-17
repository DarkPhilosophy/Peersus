//! Toolkit-independent and plugin-independent domain rules for peer transfers.
//!
//! * [`config`]: preferences and the atomic patch every client uses.
//! * [`model`]: commands, snapshots and the peers/transfers they describe.
//! * [`pairing`]: mutual human consent bound to an authenticated session.
//! * [`plugins`]: opaque plugin identities, per-device grants and typed values.
#![forbid(unsafe_code)]

pub mod address;
pub mod agreements;
pub mod bindings;
pub mod config;
pub mod layout;
pub mod model;
pub mod nearby;
pub mod pairing;
pub mod peer;
pub mod plugins;
#[cfg(feature = "private-store")]
pub mod private_store;
pub mod profile;
pub mod relay;
pub mod secret;

pub use config::{Navigation, NetworkSettings, Renderer, Settings, SettingsError, Theme};
