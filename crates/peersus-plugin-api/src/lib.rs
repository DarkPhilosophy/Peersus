//! The Peersus extension contract: everything a plugin or a frontend needs, with no
//! GUI toolkit, operating-system handle, network stack or product concept.
//!
//! * [`name`]: the single identifier grammar and bounded-text rules.
//! * [`manifest`]: capabilities, optional service exports/imports and contributions.
//! * [`package`]: what a plugin says about itself for installing and updating: name,
//!   version, description and changelog. Text to show, never a right.
//! * [`contributions`]: pages, actions and typed settings as plain data.
//! * [`message`]: the only messages that cross the hub boundary.
//! * [`hub`]: the application-owned hub that routes calls and events between plugins.
//!
//! Drawing, persistence, process lifetime, network streams and approval prompts are
//! host concerns. A plugin is identified by an id the host assigns at registration,
//! never by a field the plugin writes about itself.
#![forbid(unsafe_code)]

pub mod contributions;
pub mod error;
pub mod hub;
pub mod manifest;
pub mod message;
pub mod name;
pub mod package;
pub mod permission;
pub mod platform;

pub use error::{Error, Result};
