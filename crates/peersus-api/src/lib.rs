//! The Peersus extension contract: everything a plugin or a frontend needs, with no
//! GUI toolkit, operating-system handle, network stack or product concept.
//!
//! * [`name`]: the single identifier grammar and bounded-text rules.
//! * [`manifest`]: capabilities, optional service exports/imports and contributions.
//! * [`package`]: what a plugin says about itself for installing and updating: name,
//!   version, description and changelog. Text to show, never a right.
//! * [`contributions`]: pages, actions and typed settings as plain data.
//! * [`message`]: the only messages that cross the hub boundary.
//! * [`elements`]: what an extension shows on a page of its own, and the events it gets back.
//! * [`agreement`]: the licences and notice a person reads and accepts before an extension installs.
//! * [`chord`]: the keys pressed together that a person may bind to an action.
//! * [`notification`]: a short text shown for its sender, with the buttons the sender names.
//! * [`language`]: the tag of a person's language, and the language files (gettext PO) that translate English text.
//!
//! This crate is only the contract, the same for plugins and for the application: the hub
//! that routes the messages and everything that runs plugins live in `peersus-plugin`.
//!
//! Drawing, persistence, process lifetime, network streams and approval prompts are
//! host concerns. A plugin is identified by an id the host assigns at registration,
//! never by a field the plugin writes about itself.
#![forbid(unsafe_code)]

pub mod agreement;
pub mod chord;
pub mod contributions;
pub mod elements;
pub mod error;
pub mod language;
pub mod manifest;
pub mod message;
pub mod name;
pub mod notification;
pub mod package;
pub mod permission;
pub mod platform;

pub use error::{Error, Result};
/// The participant that is the application itself, and never a plugin: what a plugin imports
/// the application's services from.
pub const HOST: &str = "host";
