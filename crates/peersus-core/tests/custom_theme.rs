use peersus_core::config::{CustomTheme, Settings, SettingsPatch, Theme, ThemeBase, ThemeColor};

#[test]
fn custom_palette_survives_serialization_and_switching_back_to_system() {
    let original = Settings::default();
    let custom = CustomTheme {
        base: ThemeBase::Light,
        colors: [(ThemeColor::Background, [22, 30, 45]), (ThemeColor::ButtonText, [251, 249, 243])].into(),
    };
    let selected = SettingsPatch {
        theme: Some(Theme::Custom), custom_theme: Some(custom.clone()), ..Default::default()
    }.apply(&original).unwrap();
    let persisted: Settings = serde_json::from_str(&serde_json::to_string(&selected).unwrap()).unwrap();
    assert_eq!(persisted, selected);
    let system = SettingsPatch { theme: Some(Theme::System), ..Default::default() }.apply(&persisted).unwrap();
    assert_eq!(system.custom_theme, custom);
    let returned = SettingsPatch { theme: Some(Theme::Custom), ..Default::default() }.apply(&system).unwrap();
    assert_eq!(returned, selected);
    assert_eq!(Settings { theme: original.theme, custom_theme: original.custom_theme.clone(), ..returned }, original);
}

#[test]
fn older_preferences_load_without_a_custom_palette_and_default_serialization_stays_compatible() {
    let original = Settings::default();
    let serialized = serde_json::to_value(&original).unwrap();
    assert!(serialized.get("custom_theme").is_none());
    let loaded: Settings = serde_json::from_value(serialized).unwrap();
    assert_eq!(loaded, original);
}

#[test]
fn resetting_custom_colors_does_not_reset_unrelated_preferences() {
    let selected = Settings {
        theme: Theme::Custom,
        reduced_motion: true,
        custom_theme: CustomTheme { base: ThemeBase::Light, colors: [(ThemeColor::Header, [14, 30, 60])].into() },
        ..Default::default()
    };
    let reset = SettingsPatch { custom_theme: Some(CustomTheme::default()), ..Default::default() }.apply(&selected).unwrap();
    assert_eq!(reset.custom_theme, CustomTheme::default());
    assert_eq!(Settings { custom_theme: selected.custom_theme.clone(), ..reset }, selected);
}
