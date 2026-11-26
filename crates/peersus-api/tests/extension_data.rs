use peersus_api::extension_data::{Commit, Key, References, ScopeId};
use serde_json::json;

#[test]
fn virtual_keys_are_validated_even_when_decoded_as_map_keys() {
    for bad in ["", ".", "../outside", "a//b", "a/../b", "a\\b", "/a", "a/"] {
        assert!(serde_json::from_value::<Key>(json!(bad)).is_err(), "{bad}");
    }
    assert!(serde_json::from_value::<Key>(json!("records/room_1")).is_ok());
}

#[test]
fn reference_batches_have_one_canonical_scope_and_no_duplicate_alias() {
    let alias = "a".repeat(32);
    let scope = "ab".repeat(32);
    assert!(serde_json::from_value::<ScopeId>(json!(&scope)).is_ok());
    assert!(serde_json::from_value::<ScopeId>(json!(scope.to_uppercase())).is_err());
    let args = json!({"devices":[alias,alias],"scope":scope,"include_self":true});
    assert!(!serde_json::from_value::<References>(args).unwrap().valid());
}

#[test]
fn overlapping_removal_and_put_are_not_an_ambiguous_transaction() {
    let args = json!({"if_revision":0,"put":[{"key":"x/a","text":"test"}],"remove_prefix":["x"]});
    assert!(!serde_json::from_value::<Commit>(args).unwrap().valid());
}
