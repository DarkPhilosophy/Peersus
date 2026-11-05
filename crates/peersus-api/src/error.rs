//! Fail-closed errors. Messages never include payload bodies.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid manifest or configuration")]
    InvalidManifest,
    #[error("unknown identity or provider")]
    UnknownProvider,
    #[error("unknown service, method or event")]
    UnknownMethod,
    #[error("unsupported version")]
    Version,
    #[error("operation denied")]
    Denied,
    #[error("control message exceeds limit")]
    Oversize,
    #[error("deadline exceeded")]
    Timeout,
    #[error("operation cancelled")]
    Cancelled,
    #[error("recursive or busy plugin")]
    Busy,
    #[error("event queue full")]
    QueueFull,
    #[error("plugin panicked")]
    PluginPanicked,
    #[error("invalid control payload")]
    InvalidPayload,
    #[error("content unavailable or unsupported")]
    Unavailable,
    #[error("backend operation failed")]
    Backend,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
