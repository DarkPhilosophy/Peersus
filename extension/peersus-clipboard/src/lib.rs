//! The clipboard plugin's own code: what it sends to another device and what it will accept.
//!
//! Nothing here reaches the network. The wire format and the picture decoding are the parts
//! that decide whether a device that sends something hostile can make this process allocate
//! without limit, so they are kept apart where they can be tested without a desktop. The one
//! place that listens to the operating system for copies is [`watch`], and it never reads more
//! than the text limit.
#![forbid(unsafe_code)]

pub use peersus_clipboard_platform::{owner, watch};
pub mod picture;
pub mod plugin;
#[cfg(unix)]
pub mod session;
pub mod wire;
