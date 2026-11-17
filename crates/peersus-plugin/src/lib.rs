//! Everything that runs Peersus plugins, owned by the application: the hub that routes every
//! call and event between participants, and each plugin's isolated process.
//!
//! * [`hub`]: the application-owned hub that routes calls and events between plugins.
//! * [`frame`]: bounded length-prefixed JSON frames.
//! * [`link`]: token-authenticated bridge between a plugin's stream and its hub channel.
//! * [`supervisor`]: starts, watches, restarts and stops the plugin process.
//! * [`data`]: the data connection of a plugin, authenticated and bound to one accepted call.
//! * [`pump`]: carries bulk bytes between a peer and a plugin, so plugins never touch
//!   the network.
//!
//! What crosses between a plugin and the application is the contract of `peersus-api`; this
//! crate never interprets a message beyond the hub's rules of who may call whom.
#![forbid(unsafe_code)]

pub mod data;
pub mod frame;
pub mod hub;
pub mod link;
pub mod module;
pub mod pump;
pub mod supervisor;
pub mod transport;

mod service;
pub use service::{Accepted, Event, Plugin, Service};
