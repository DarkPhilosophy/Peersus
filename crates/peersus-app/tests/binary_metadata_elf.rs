//! A real shared object, compiled from C with the SDK header, read without being loaded.
#![cfg(target_os = "linux")]

use peersus_app::binary_metadata::{Absent, agrees, read};
use std::{path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

const MANIFEST: &str = r#"{
  "manifest_version": 1, "capabilities": {}, "imports": [], "exports": [],
  "package": {"name": "Echo", "version": "1.0.0", "author": "Someone",
              "link": "https://example.org/echo", "description": "Echoes."}
}"#;

fn build(dir: &std::path::Path, with_record: bool) -> Vec<u8> {
    let root = root();
    std::fs::write(dir.join("plugin.json"), MANIFEST).unwrap();
    let header = Command::new("python3")
        .arg(root.join("scripts/embed-metadata.py"))
        .arg("header")
        .arg(dir.join("plugin.json"))
        .output()
        .unwrap();
    assert!(
        header.status.success(),
        "{}",
        String::from_utf8_lossy(&header.stderr)
    );
    let record = if with_record {
        "PEERSUS_METADATA_RECORD(PEERSUS_METADATA_BYTES);"
    } else {
        ""
    };
    let source = format!(
        "#include \"peersus_plugin.h\"\n{}\n{record}\nint32_t peersus_plugin_main(const PeersusHostV1 *h){{ (void)h; return 0; }}\n",
        String::from_utf8(header.stdout).unwrap()
    );
    std::fs::write(dir.join("plugin.c"), source).unwrap();
    let out = dir.join("plugin.so");
    let status = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()))
        .args([
            "-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror", "-I",
        ])
        .arg(root.join("crates/peersus-plugin-runtime/sdk"))
        .arg(dir.join("plugin.c"))
        .arg("-o")
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "the plugin did not compile");
    std::fs::read(out).unwrap()
}

#[test]
fn an_elf_shared_object_carries_the_record_and_it_matches_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let binary = build(dir.path(), true);
    let embedded = read(&binary).expect("embedded metadata");
    assert_eq!(embedded.name, "Echo");
    assert_eq!(embedded.author, "Someone");
    assert_eq!(embedded.link, "https://example.org/echo");
    let split = peersus_api::package::split(MANIFEST.as_bytes()).unwrap();
    assert!(agrees(&embedded, &split.package.unwrap()));
}

#[test]
fn an_elf_without_the_record_is_missing_and_a_manifest_edit_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(&build(dir.path(), false)), Err(Absent::Missing));
    let embedded = read(&build(dir.path(), true)).unwrap();
    let edited = MANIFEST.replace("Someone", "Impostor");
    let package = peersus_api::package::split(edited.as_bytes())
        .unwrap()
        .package
        .unwrap();
    assert!(
        !agrees(&embedded, &package),
        "an edited manifest author was accepted"
    );
}

#[test]
fn flipping_a_byte_of_the_embedded_json_is_refused_or_changes_what_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut binary = build(dir.path(), true);
    let needle = b"https://example.org/echo";
    let at = binary
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("record present");
    binary[at + 4] = b'X'; // https -> httpX
    match read(&binary) {
        Err(Absent::Malformed) => {}
        Ok(changed) => assert_ne!(changed.link, "https://example.org/echo"),
        other => panic!("unexpected {other:?}"),
    }
}
