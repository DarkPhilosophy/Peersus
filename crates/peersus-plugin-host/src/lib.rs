//! Runs each Peersus plugin as its own process, connected to the application hub.
//!
//! * [`frame`]: bounded length-prefixed JSON frames.
//! * [`link`]: token-authenticated bridge between a plugin's stream and its hub channel.
//! * [`supervisor`]: starts, watches, restarts and stops the plugin process.
//! * [`data`]: the data connection of a plugin, authenticated and bound to one accepted call.
//! * [`pump`]: carries bulk bytes between a peer and a plugin, so plugins never touch
//!   the network.
//!
//! The host never interprets a message; every rule about who may call whom lives in the
//! hub. This crate only keeps a misbehaving plugin from affecting anything else.
#![forbid(unsafe_code)]

pub mod data;
pub mod frame;
pub mod link;
pub mod module;
pub mod pump;
pub mod supervisor;

#[cfg(unix)]
mod service;
#[cfg(unix)]
pub use service::{Accepted, Event, Plugin, Service};

#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::{Accepted, Event, Plugin, Service};
