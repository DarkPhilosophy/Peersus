use peersus_core::{
    Settings,
    config::{Language, SettingsPatch},
};

#[test]
fn older_settings_load_in_english_without_rewriting_their_schema() {
    let original = serde_json::to_value(Settings::default()).unwrap();
    assert!(original.get("language").is_none());
    let loaded: Settings = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(loaded.language, Language::English);
    assert_eq!(serde_json::to_value(loaded).unwrap(), original);
}

#[test]
fn changing_language_preserves_other_preferences_and_survives_serialization() {
    let current = Settings {
        device_name: "My device".into(),
        background: true,
        ..Settings::default()
    };
    let patch: SettingsPatch = serde_json::from_str(r#"{"language":"ro"}"#).unwrap();
    let changed = patch.apply(&current).unwrap();
    assert_eq!(changed.language, Language::Romanian);
    assert_eq!(
        Settings {
            language: Language::English,
            ..changed.clone()
        },
        current
    );
    let restored: Settings =
        serde_json::from_slice(&serde_json::to_vec(&changed).unwrap()).unwrap();
    assert_eq!(restored, changed);
    let unrelated = SettingsPatch {
        reduced_motion: Some(true),
        ..SettingsPatch::default()
    }
    .apply(&restored)
    .unwrap();
    assert_eq!(unrelated.language, Language::Romanian);
    let english = SettingsPatch {
        language: Some(Language::English),
        ..SettingsPatch::default()
    }
    .apply(&unrelated)
    .unwrap();
    assert!(
        serde_json::to_value(english)
            .unwrap()
            .get("language")
            .is_none()
    );
}

#[test]
fn unsupported_languages_are_rejected_without_applying_a_partial_patch() {
    assert!(
        serde_json::from_str::<SettingsPatch>(r#"{"language":"unknown","background":true}"#)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(SettingsPatch::default()).unwrap(),
        serde_json::json!({})
    );
}
