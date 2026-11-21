//! The versions the About page shows, read at build time from what the build really resolves:
//! the workspace's `Cargo.lock` (every package, and every version of a package that is there more
//! than once) and the Skia milestone the vendored renderer bindings fetch and build. Nothing is
//! written by hand, so a version on the page cannot differ from the one built. It also writes the
//! window program's identity where Windows reads it from the program file (`build-support/`).
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::{env, fs};

#[path = "../../build-support/program_identity.rs"]
mod program_identity;

fn main() {
    program_identity::write(&program_identity::Program {
        binary: "peersus-ui",
        description: "Peersus window",
        helper_role: None,
    });
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("set by cargo")).join("../..");
    let lock = root.join("Cargo.lock");
    let bindings = root.join("vendor/freya-skia-bindings/Cargo.toml");
    println!("cargo:rerun-if-changed={}", lock.display());
    println!("cargo:rerun-if-changed={}", bindings.display());

    let mut versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let text = fs::read_to_string(&lock).expect("the workspace lockfile is readable");
    let mut name = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            name = None;
        } else if let Some(value) = quoted(line, "name") {
            name = Some(value.to_owned());
        } else if let (Some(package), Some(version)) = (&name, quoted(line, "version")) {
            versions
                .entry(package.clone())
                .or_default()
                .insert(version.to_owned());
        }
    }

    // `[package.metadata] skia = "m156.5"`: the Skia release the bindings fetch and build.
    let manifest = fs::read_to_string(&bindings).unwrap_or_default();
    let skia = manifest
        .lines()
        .skip_while(|line| line.trim() != "[package.metadata]")
        .find_map(|line| quoted(line.trim(), "skia"))
        .unwrap_or_default()
        .to_owned();

    let mut out =
        String::from("/// Every package of the lockfile and its versions, sorted by name.\n");
    out.push_str("pub const VERSIONS: &[(&str, &str)] = &[\n");
    for (package, found) in &versions {
        let joined = found
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" / ");
        out.push_str(&format!("    ({package:?}, {joined:?}),\n"));
    }
    out.push_str("];\n/// The Skia release the vendored bindings build.\n");
    out.push_str(&format!("pub const SKIA: &str = {skia:?};\n"));
    let target = Path::new(&env::var("OUT_DIR").expect("set by cargo")).join("versions.rs");
    fs::write(target, out).expect("the generated versions are writable");
}

/// The value of `key = "value"` on one line of the lockfile or a manifest.
fn quoted<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    rest.strip_prefix('"')?.strip_suffix('"')
}
