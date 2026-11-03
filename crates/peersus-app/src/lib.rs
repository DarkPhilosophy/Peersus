//! The Peersus application owner, with no GUI toolkit and no concrete plugin.
//!
//! * [`runtime`]: the single owner of the network host, preferences and plugins.
//! * [`plugin_host`]: manifest-driven routing and per-device authorization.
//! * [`cli`]: command-line syntax that becomes the same validated commands.
//! * [`ipc`], [`instance`], [`identity`]: local control, single-owner lock, identity.
#![forbid(unsafe_code)]

pub mod catalog;
mod aliases;
pub mod archive;
pub mod binary_metadata;
mod board;
pub mod cli;
pub mod discovery;
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod identity;
mod in_flight;
#[cfg(unix)]
pub mod lifetime;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod instance;
#[cfg(any(unix, windows))]
pub mod ipc;
mod parked;
pub mod plugin_host;
pub mod plugin_state;
pub mod transfer_events;
#[cfg(unix)]
pub mod plugin_approval;
#[cfg(unix)]
pub mod plugin_folder;
#[cfg(unix)]
pub mod plugin_manager;
mod receive;
mod running;
pub mod runtime;
pub mod store;
#[cfg(unix)]
pub mod store_service;
#[cfg(unix)]
pub mod store_runtime;
pub mod store_trust;
