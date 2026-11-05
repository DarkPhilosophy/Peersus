//! The one event this guest listens to: the application's note that the clipboard changed.
//!
//! The hub stamps `provider` from the channel that published, so it cannot be forged by another extension. What an
//! extension CAN do is publish an event with the same service and event names, so all four fields are checked: only
//! the application's own `peersus.clipboard` `changed` wakes automatic sending.
use serde_json::{Value, json};

const PROVIDER: &str = "host";
const SERVICE: &str = "peersus.clipboard";
const VERSION: u64 = 1;
const EVENT: &str = "changed";

/// The request that asks the application to tell this guest when the clipboard changes.
pub fn subscribe() -> Value {
    json!({"op":"subscribe","provider":PROVIDER,"service":SERVICE,"version":VERSION,"event":EVENT})
}

/// Whether a delivery is the application's clipboard change notice, and nothing else.
pub fn is_change(delivery: &Value) -> bool {
    delivery["type"] == "event"
        && delivery["provider"] == PROVIDER
        && delivery["service"] == SERVICE
        && delivery["version"] == VERSION
        && delivery["event"] == EVENT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(provider: &str, service: &str, version: u64, name: &str) -> Value {
        json!({"type":"event","provider":provider,"service":service,"version":version,"event":name,"data":{}})
    }

    #[test]
    fn only_the_applications_own_change_notice_counts() {
        assert!(is_change(&event("host", "peersus.clipboard", 1, "changed")));
        for (provider, service, version, name) in [
            ("other", "peersus.clipboard", 1, "changed"),
            ("host", "peersus.files", 1, "changed"),
            ("host", "peersus.clipboard", 2, "changed"),
            ("host", "peersus.clipboard", 1, "removed"),
            ("clipboard", "peersus.clipboard", 1, "changed"),
        ] {
            assert!(!is_change(&event(provider, service, version, name)), "{provider} {service} {version} {name}");
        }
    }

    #[test]
    fn a_delivery_of_another_kind_never_counts_even_with_the_same_fields() {
        let mut call = event("host", "peersus.clipboard", 1, "changed");
        call["type"] = json!("call");
        assert!(!is_change(&call));
        assert!(!is_change(&json!({})));
        assert!(!is_change(&json!({"type":"event"})));
        assert!(!is_change(&json!("event")));
    }

    #[test]
    fn the_subscription_names_exactly_what_the_guest_checks_for() {
        let request = subscribe();
        assert_eq!(request["op"], "subscribe");
        let as_delivery = json!({
            "type":"event","provider":request["provider"],"service":request["service"],
            "version":request["version"],"event":request["event"],"data":{}
        });
        assert!(is_change(&as_delivery), "what is asked for is what is recognised");
    }
}
