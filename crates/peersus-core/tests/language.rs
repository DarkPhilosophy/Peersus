use peersus_core::{
    Settings,
    config::{Language, SettingsPatch},
};

fn ro() -> Language {
    Language::parse("ro").unwrap()
}

#[test]
fn older_settings_follow_the_device_language_without_rewriting_their_schema() {
    let original = serde_json::to_value(Settings::default()).unwrap();
    assert!(original.get("language").is_none());
    let loaded: Settings = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(loaded.language, None);
    assert_eq!(serde_json::to_value(loaded).unwrap(), original);
    // A file written while languages were a closed list still loads: a tag is what it saved.
    let mut written = original;
    written["language"] = "ro".into();
    let loaded: Settings = serde_json::from_value(written).unwrap();
    assert_eq!(loaded.language, Some(ro()));
}

#[test]
fn choosing_a_language_preserves_other_preferences_and_null_follows_the_device_again() {
    let current = Settings {
        device_name: "My device".into(),
        background: true,
        ..Settings::default()
    };
    let patch: SettingsPatch = serde_json::from_str(r#"{"language":"ro"}"#).unwrap();
    let changed = patch.apply(&current).unwrap();
    assert_eq!(changed.language, Some(ro()));
    assert_eq!(
        Settings {
            language: None,
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
    assert_eq!(
        unrelated.language,
        Some(ro()),
        "a patch without the field leaves it"
    );
    let english = SettingsPatch {
        language: Some(Some(Language::ENGLISH)),
        ..SettingsPatch::default()
    }
    .apply(&unrelated)
    .unwrap();
    assert_eq!(
        serde_json::to_value(&english).unwrap()["language"],
        "en",
        "English chosen is kept apart from following the device"
    );
    let device: SettingsPatch = serde_json::from_str(r#"{"language":null}"#).unwrap();
    assert_eq!(device.language, Some(None));
    assert_eq!(
        serde_json::to_value(&device).unwrap(),
        serde_json::json!({"language": null})
    );
    let following = device.apply(&english).unwrap();
    assert_eq!(following.language, None);
}

#[test]
fn what_is_not_a_language_tag_is_rejected_without_applying_a_partial_patch() {
    for bad in [r#""unknown""#, r#""ro ro""#, "3"] {
        let text = format!(r#"{{"language":{bad},"background":true}}"#);
        assert!(
            serde_json::from_str::<SettingsPatch>(&text).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        serde_json::to_value(SettingsPatch::default()).unwrap(),
        serde_json::json!({})
    );
}
