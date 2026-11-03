//! Toolkit-independent and plugin-independent domain rules for peer transfers.
//!
//! * [`config`]: preferences and the atomic patch every client uses.
//! * [`model`]: commands, snapshots and the peers/transfers they describe.
//! * [`pairing`]: mutual human consent bound to an authenticated session.
//! * [`plugins`]: opaque plugin identities, per-device grants and typed values.
#![forbid(unsafe_code)]

pub mod config;
pub mod model;
pub mod pairing;
pub mod plugins;
pub mod profile;

pub use config::{Navigation, NetworkSettings, Renderer, Settings, SettingsError, Theme};
