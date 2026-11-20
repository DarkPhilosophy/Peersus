//! How a device is named everywhere in the product. Apart from the commands and snapshots that carry
//! it ([`crate::model`], which gives it out again under its old path), so that a part that only names
//! a device does not need all of them.
use serde::{Deserialize, Serialize};

/// A peer's authenticated public identity, never its display name or IP address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub String);
