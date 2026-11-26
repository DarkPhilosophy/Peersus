use peersus_api::contributions::{Kind, Scope, Setting};
use peersus_core::{
    Settings, SettingsError,
    config::{BindChange, SettingsPatch},
    plugins::{PluginId, Value, Values},
};

#[test]
fn cli_and_ui_changes_preserve_unrelated_preferences() {
    let original = Settings {
        device_name: "Desk".into(),
        reduced_motion: true,
        ..Default::default()
    };
    let updated = SettingsPatch {
        bind: Some(BindChange::Address("127.0.0.1".parse().unwrap())),
        allow_relay: Some(false),
        background: Some(true),
        ..Default::default()
    }
    .apply(&original)
    .unwrap();
    assert_eq!(updated.network.bind, Some("127.0.0.1".parse().unwrap()));
    assert!(updated.background);
    assert!(updated.reduced_motion);
    assert_eq!(updated.device_name, "Desk");
    assert_eq!(original.network.bind, None);
}

#[test]
fn invalid_combined_change_does_not_mutate_original() {
    let original = Settings::default();
    let patch = SettingsPatch {
        background: Some(true),
        scale: Some(f32::NAN),
        ..Default::default()
    };
    assert_eq!(patch.apply(&original), Err(SettingsError::Scale));
    assert!(!original.background);
}

#[test]
fn automatic_bind_is_distinct_from_leaving_bind_unchanged() {
    let mut original = Settings::default();
    original.network.bind = Some("127.0.0.1".parse().unwrap());
    original.network.allow_relay = false;
    assert_eq!(SettingsPatch::default().apply(&original).unwrap(), original);
    let result = SettingsPatch {
        bind: Some(BindChange::Automatic),
        ..Default::default()
    }
    .apply(&original)
    .unwrap();
    assert_eq!(result.network.bind, None);
}

#[test]
fn binding_without_disabling_relay_cannot_publish_an_unusable_configuration() {
    let current = Settings::default();
    let mut patch = SettingsPatch {
        bind: Some(BindChange::Address("127.0.0.1".parse().unwrap())),
        ..Default::default()
    };
    assert_eq!(patch.apply(&current), Err(SettingsError::BoundRelay));
    patch.allow_relay = Some(false);
    assert!(patch.apply(&current).is_ok());
}

#[test]
fn a_patch_never_touches_saved_plugin_values() {
    let declared = [Setting {
        public_read: None,
        platforms: Vec::new(),
        id: "enabled".into(),
        label: "Enabled".into(),
        help: None,
        scope: Scope::Global,
        kind: Kind::Toggle { default: false },
    }];
    let id = PluginId::new("anything").unwrap();
    let mut values = Values::default();
    values
        .set(&declared, Scope::Global, "enabled", Value::Toggle(true))
        .unwrap();
    let mut original = Settings::default();
    original.plugins.insert(id.clone(), values.clone());
    let updated = SettingsPatch {
        theme: Some(peersus_core::Theme::Dark),
        ..Default::default()
    }
    .apply(&original)
    .unwrap();
    assert_eq!(updated.plugins.get(&id), Some(&values));
}
