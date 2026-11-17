//! The Peersus application owner, with no GUI toolkit and no concrete plugin.
//!
//! * [`runtime`]: the single owner of the network host, preferences and plugins.
//! * [`plugin_host`]: manifest-driven routing and per-device authorization.
//! * [`cli`]: command-line syntax that becomes the same validated commands.
//! * [`ipc`], [`instance`], [`identity`]: local control, single-owner lock, identity.
#![forbid(unsafe_code)]

/// Fixed startup error tag shared by the desktop owner and its window.
pub const IDENTITY_STARTUP_ERROR_PREFIX: &str = "Identity unavailable: ";

mod aliases;
pub mod app_update;
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
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod displays;
pub mod extension_prompts;
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod identity;
mod in_flight;
#[cfg(target_os = "linux")]
pub mod input_follow;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod instance;
#[cfg(any(unix, windows))]
pub mod ipc;
pub mod languages;
pub mod lifetime;
#[cfg(windows)]
pub mod local_pipe;
pub mod nearby;
pub mod notifications;
#[cfg(target_os = "linux")]
pub mod offer_panel;
mod package_languages;
mod page_offers;
pub mod page_views;
mod parked;
pub mod pass_through;
mod platform_calls;
pub mod platform_services;
pub mod plugin_agreements;
pub mod plugin_approval;
pub mod plugin_folder;
pub mod plugin_host;
pub mod plugin_manager;
pub mod plugin_state;
mod private_fs;
#[cfg(feature = "private-store")]
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
pub mod store_branch;
pub mod store_runtime;
pub mod store_service;
pub mod store_trust;
pub mod surfaces;
pub mod system_readings;
pub mod transfer_events;
#[cfg(not(target_os = "android"))]
pub mod update_install;
