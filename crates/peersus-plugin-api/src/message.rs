//! Language-neutral messages exchanged with the hub.
//!
//! These are the only things that cross the hub boundary, whether the plugin is a
//! thread in a test, a local process or something else entirely. A message never
//! carries a sender: the hub stamps the identity of the channel it arrived on.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upper bound for one serialized control message.
pub const MAX_PAYLOAD: usize = 48 * 1024;

/// What a participant asks the hub to do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    /// Call a method on a provider's exported service. `id` is chosen by the caller.
    Call {
        id: u64,
        provider: String,
        service: String,
        version: u32,
        method: String,
        args: Value,
        timeout_ms: u32,
    },
    /// Answer a call previously delivered to this participant.
    Reply { ticket: u64, result: Outcome },
    /// Withdraw one of this participant's own pending calls.
    Cancel { id: u64 },
    Subscribe {
        provider: String,
        service: String,
        version: u32,
        event: String,
    },
    Unsubscribe {
        provider: String,
        service: String,
        version: u32,
        event: String,
    },
    /// Publish an event this participant exported.
    Publish {
        service: String,
        version: u32,
        event: String,
        data: Value,
    },
}

/// What the hub delivers to a participant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Delivery {
    /// A call to answer with [`Operation::Reply`] carrying the same ticket.
    Call {
        ticket: u64,
        /// Stamped by the hub from the caller's channel.
        caller: String,
        service: String,
        version: u32,
        method: String,
        args: Value,
    },
    /// The outcome of a call this participant made, matched by its own `id`.
    Result { id: u64, result: Outcome },
    /// A call that was cancelled or expired before it was answered.
    Cancelled { ticket: u64 },
    Event {
        provider: String,
        service: String,
        version: u32,
        event: String,
        data: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Outcome {
    Ok { value: Value },
    Error { code: String, message: String },
}

impl Outcome {
    pub fn ok(value: Value) -> Self {
        Self::Ok { value }
    }
    pub fn error(code: &str, message: &str) -> Self {
        Self::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        }
    }
}

/// Serialized size check without keeping the encoded copy.
pub fn bounded(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_PAYLOAD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_message_has_no_sender_field_to_forge() {
        // The caller of a delivered call is stamped by the hub; an operation that tries
        // to carry its own `caller` or `owner` is rejected rather than ignored.
        let forged = json!({"op":"cancel","id":1,"caller":"host"});
        assert!(serde_json::from_value::<Operation>(forged).is_err());
        let ok = json!({"op":"cancel","id":1});
        assert!(serde_json::from_value::<Operation>(ok).is_ok());
    }

    #[test]
    fn size_bound_is_applied_to_the_serialized_form() {
        let big = Operation::Publish {
            service: "s".into(),
            version: 1,
            event: "e".into(),
            data: json!("x".repeat(MAX_PAYLOAD)),
        };
        assert!(!bounded(&big));
        let small = Operation::Cancel { id: 1 };
        assert!(bounded(&small));
    }
}
