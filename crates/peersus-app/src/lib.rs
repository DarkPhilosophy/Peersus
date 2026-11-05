//! The Peersus application owner, with no GUI toolkit and no concrete plugin.
//!
//! * [`runtime`]: the single owner of the network host, preferences and plugins.
//! * [`plugin_host`]: manifest-driven routing and per-device authorization.
//! * [`cli`]: command-line syntax that becomes the same validated commands.
//! * [`ipc`], [`instance`], [`identity`]: local control, single-owner lock, identity.
#![forbid(unsafe_code)]

mod aliases;
pub mod archive;
pub mod binary_metadata;
mod board;
pub mod catalog;
pub mod cli;
pub mod clipboard_handles;
#[cfg(target_os = "linux")]
pub mod desktop_capture;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod desktop_clipboard;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod desktop_input;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod desktop_services;
pub mod discovery;
mod display_service;
#[cfg(target_os = "linux")]
pub mod displays;
pub mod extension_prompts;
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod identity;
mod in_flight;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod instance;
#[cfg(any(unix, windows))]
pub mod ipc;
pub mod lifetime;
#[cfg(windows)]
pub mod local_pipe;
mod page_offers;
pub mod page_views;
mod parked;
pub mod pass_through;
mod platform_calls;
pub mod platform_services;
#[cfg(unix)]
pub mod plugin_approval;
#[cfg(unix)]
pub mod plugin_folder;
pub mod plugin_host;
#[cfg(unix)]
pub mod plugin_manager;
#[cfg(not(unix))]
#[path = "plugin_manager_unsupported.rs"]
pub mod plugin_manager;
pub mod plugin_state;
#[cfg(all(feature = "private-store", unix))]
pub mod private_store;
mod receive;
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod relay_token;
mod running;
pub mod runtime;
#[cfg(target_os = "linux")]
pub mod sleep;
pub mod status_service;
mod statuses;
pub mod store;
#[cfg(unix)]
pub mod store_branch;
#[cfg(unix)]
pub mod store_runtime;
#[cfg(unix)]
pub mod store_service;
pub mod store_trust;
pub mod surfaces;
pub mod transfer_events;
