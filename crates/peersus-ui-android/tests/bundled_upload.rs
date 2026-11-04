#![cfg(unix)]

#[path = "../src/bundled_upload.rs"]
mod bundled_upload;

use peersus_app::plugin_manager::{Manager, Standing};
use serde_json::json;
use std::io::Write;

fn package(id: &str, contents: &str) -> Vec<u8> {
    // A minimal valid WASM container is sufficient for installer-state tests;
    // the real upload guest and isolated execution have separate mandatory gates.
    let manifest = json!({
        "manifest_version": 1,
        "platforms": ["android", "linux", "macos"],
        "program": "module.wasm",
        "execution": {"kind": "wasm", "abi": 1},
        "capabilities": {}, "imports": [], "exports": [],
        "files": ["data.txt"]
    });
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in [
        ("plugin.json", serde_json::to_vec(&manifest).unwrap()),
        ("module.wasm", b"\0asm\x01\0\0\0".to_vec()),
        ("data.txt", contents.as_bytes().to_vec()),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{id}/{name}"), bytes.as_slice())
            .unwrap();
    }
    let tar = archive.into_inner().unwrap();
    let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    compressed.write_all(&tar).unwrap();
    compressed.finish().unwrap()
}

#[test]
fn first_start_installs_through_the_real_manager_and_a_later_removal_stays_removed() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    let archive = package("upload", "first-party payload");
    bundled_upload::install_once(&manager, data.path(), &archive).unwrap();
    let discovered = manager.load();
    assert!(discovered.problems.is_empty());
    assert!(
        discovered
            .registry
            .find(&peersus_core::plugins::PluginId::new("upload").unwrap())
            .is_some()
    );
    assert_eq!(manager.list().unwrap()[0].standing, Standing::Ready);
    manager.remove("upload").unwrap();
    bundled_upload::install_once(&manager, data.path(), &archive).unwrap();
    assert!(
        manager.list().unwrap().is_empty(),
        "a removed extension was silently reinstalled"
    );
}

#[test]
fn existing_user_package_and_its_disabled_state_are_not_replaced_by_the_apk() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    let existing = peersus_app::archive::keep_source(
        package("upload", "keep the user's package").as_slice(),
        "upload.tar.gz",
        data.path(),
    )
    .unwrap();
    let unpacked = peersus_app::archive::unpack_in(existing.path(), data.path()).unwrap();
    manager.install(unpacked.root()).unwrap();
    manager.set_disabled("upload", true).unwrap();
    let installed = manager.store().join("upload");
    let approval = std::fs::read(installed.join("approval.json")).unwrap();
    bundled_upload::install_once(
        &manager,
        data.path(),
        &package("upload", "different APK payload"),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(installed.join("data.txt")).unwrap(),
        b"keep the user's package"
    );
    assert_eq!(
        std::fs::read(installed.join("approval.json")).unwrap(),
        approval
    );
    assert_eq!(manager.list().unwrap()[0].standing, Standing::Disabled);
}

#[test]
fn damaged_or_wrong_identity_archive_is_not_recorded_as_success_and_can_be_retried() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    assert!(bundled_upload::install_once(&manager, data.path(), b"not an archive").is_err());
    assert!(
        bundled_upload::install_once(&manager, data.path(), &package("other", "wrong identity"))
            .is_err()
    );
    assert!(!manager.store().join("other").exists());
    bundled_upload::install_once(&manager, data.path(), &package("upload", "valid retry")).unwrap();
    assert_eq!(manager.list().unwrap()[0].id, "upload");
}

#[test]
fn apk_upgrade_keeps_disabled_state_and_reapproves_only_the_new_package() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    assert!(
        bundled_upload::install_once(&manager, data.path(), &package("upload", "old")).unwrap()
    );
    manager.set_disabled("upload", true).unwrap();
    assert!(
        bundled_upload::install_once(&manager, data.path(), &package("upload", "new")).unwrap()
    );
    assert_eq!(
        std::fs::read(manager.store().join("upload/data.txt")).unwrap(),
        b"new"
    );
    assert_eq!(manager.list().unwrap()[0].standing, Standing::Disabled);
    manager.set_disabled("upload", false).unwrap();
    assert_eq!(manager.list().unwrap()[0].standing, Standing::Ready);
    assert!(
        !bundled_upload::install_once(&manager, data.path(), &package("upload", "new")).unwrap()
    );
}

#[test]
fn modified_seed_is_not_overwritten_by_a_later_apk() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    bundled_upload::install_once(&manager, data.path(), &package("upload", "old")).unwrap();
    std::fs::write(manager.store().join("upload/data.txt"), b"user edit").unwrap();
    assert!(
        !bundled_upload::install_once(&manager, data.path(), &package("upload", "new")).unwrap()
    );
    assert_eq!(
        std::fs::read(manager.store().join("upload/data.txt")).unwrap(),
        b"user edit"
    );
}

#[test]
fn interrupted_upgrade_restores_the_previous_package_before_retrying() {
    let data = tempfile::tempdir().unwrap();
    let manager = Manager::new(&data.path().join("extensions"));
    let old = package("upload", "old");
    bundled_upload::install_once(&manager, data.path(), &old).unwrap();
    manager.set_disabled("upload", true).unwrap();
    let state = manager.store().parent().unwrap();
    let previous = std::fs::read_to_string(state.join("bundled-upload-seeded-v2")).unwrap();
    let retired = manager.retire("upload").unwrap();
    std::fs::write(
        state.join("bundled-upload-upgrade"),
        serde_json::to_vec(&json!({
            "retired": retired.file_name().unwrap().to_str().unwrap(),
            "previous": previous
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!bundled_upload::install_once(&manager, data.path(), &old).unwrap());
    assert_eq!(manager.list().unwrap()[0].standing, Standing::Disabled);
    assert_eq!(
        std::fs::read(manager.store().join("upload/data.txt")).unwrap(),
        b"old"
    );
    assert!(!retired.exists());
}
