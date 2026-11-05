use peersus_core::config::{CustomTheme, NamedTheme, SettingsPatch, Theme, ThemeBase, ThemeColor};
use peersus_net::settings::SettingsStore;

#[test]
fn named_library_and_active_palette_survive_actual_store_reopen_and_other_setting_edits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    let mut store = SettingsStore::open(&path).unwrap();
    let palette = CustomTheme {
        base: ThemeBase::Light,
        colors: [
            (ThemeColor::Header, [10, 20, 30]),
            (ThemeColor::ActiveIcons, [40, 50, 60]),
        ]
        .into(),
    };
    store
        .update(&SettingsPatch {
            theme: Some(Theme::Custom),
            custom_theme: Some(palette.clone()),
            save_theme: Some(NamedTheme {
                name: "My custom theme".into(),
                palette: palette.clone(),
            }),
            ..Default::default()
        })
        .unwrap();
    store
        .update(&SettingsPatch {
            theme: Some(Theme::System),
            background: Some(true),
            ..Default::default()
        })
        .unwrap();
    drop(store);
    let store = SettingsStore::open(&path).unwrap();
    assert_eq!(store.current().theme, Theme::System);
    assert!(store.current().background);
    assert_eq!(store.current().custom_theme, palette);
    assert_eq!(
        store.current().saved_themes.get("My custom theme"),
        Some(&palette)
    );
    assert!(store.set_aside().is_none());
}

#[test]
fn saving_another_theme_merges_the_library_and_invalid_names_do_not_publish() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    let mut store = SettingsStore::open(&path).unwrap();
    for name in ["First", "Second"] {
        store
            .update(&SettingsPatch {
                save_theme: Some(NamedTheme {
                    name: name.into(),
                    palette: CustomTheme::default(),
                }),
                ..Default::default()
            })
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    assert!(
        store
            .update(&SettingsPatch {
                save_theme: Some(NamedTheme {
                    name: " bad ".into(),
                    palette: CustomTheme::default()
                }),
                ..Default::default()
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(store);
    let store = SettingsStore::open(&path).unwrap();
    assert!(store.current().saved_themes.contains_key("First"));
    assert!(store.current().saved_themes.contains_key("Second"));
}
