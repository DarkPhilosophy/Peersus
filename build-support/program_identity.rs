//! What Windows and macOS show for one program of Peersus, written into that program when it is linked.
//!
//! Linux reads a program's publisher from the package it came from (the AppStream file under `packaging/linux/`), never
//! from the program file. Windows reads it from a version resource in each `.exe` (Task Manager's description, the file's
//! Details), and macOS from the `Info.plist` of a bundle or, for a helper beside the bundle's main program, from one
//! embedded in the helper itself (its code-signing identifier, Activity Monitor). This file writes those two and nothing
//! else; each program's build script names the program and says in one line what it does.
//!
//! Shared by the build scripts through `#[path]`: a build script cannot depend on a workspace crate without making
//! that crate a build dependency of every program.

use std::path::{Path, PathBuf};
use std::{env, fs};

/// The application's bundle identifier; a helper's is this with its role appended.
const IDENTIFIER: &str = "io.github.DarkPhilosophy.Peersus";
const PRODUCT: &str = "Peersus";
const PUBLISHER: &str = "DarkPhilosophy";

/// One program as Windows and macOS describe it.
pub struct Program<'a> {
    /// The Cargo binary target, which is also the file's name without `.exe`.
    pub binary: &'a str,
    /// What the program does, in a few words: Task Manager's "Description" column.
    pub description: &'a str,
    /// The last part of a macOS helper's identifier (`core` gives `io.github.DarkPhilosophy.Peersus.core`); `None` for
    /// the bundle's main program, whose identity is the bundle's own `Info.plist` (`scripts/stage-macos.py`).
    pub helper_role: Option<&'a str>,
}

/// Writes the program's identity for the target being built, when that target reads one from the program.
pub fn write(program: &Program<'_>) {
    println!("cargo:rerun-if-env-changed=PEERSUS_BUILD_NUMBER");
    println!("cargo:rerun-if-changed=../../build-support/program_identity.rs");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let out = PathBuf::from(env::var("OUT_DIR").expect("set by cargo"));
    let version = env::var("CARGO_PKG_VERSION").expect("set by cargo");
    // The same build number `scripts/stage-macos.py` and the Android build read; 1 when none is given.
    let build = env::var("PEERSUS_BUILD_NUMBER").unwrap_or_else(|_| "1".to_owned());
    match os.as_str() {
        // MSVC's linker takes a compiled resource (`.res`) as an input of its own; the GNU toolchain is not handled.
        "windows" if target_env == "msvc" => {
            let path = out.join(format!("{}.res", program.binary));
            fs::write(&path, version_resource(program, &version, &build))
                .expect("the version resource is writable");
            link_argument(program.binary, &path, |path| path.display().to_string());
        }
        "macos" => {
            let Some(role) = program.helper_role else {
                return;
            };
            let path = out.join(format!("{}.Info.plist", program.binary));
            fs::write(&path, property_list(program, role, &version, &build))
                .expect("the property list is writable");
            link_argument(program.binary, &path, |path| {
                format!("-Wl,-sectcreate,__TEXT,__info_plist,{}", path.display())
            });
        }
        _ => {}
    }
}

fn link_argument(binary: &str, path: &Path, argument: impl Fn(&Path) -> String) {
    println!("cargo:rustc-link-arg-bin={binary}={}", argument(path));
}

/// `major.minor.patch` of the crate and the build number, as the four 16-bit parts Windows keeps.
fn parts(version: &str, build: &str) -> [u16; 4] {
    let mut parts = [0u16; 4];
    let release = version.split(['-', '+']).next().unwrap_or_default();
    for (slot, part) in parts.iter_mut().zip(release.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    parts[3] = build.parse().unwrap_or(0);
    parts
}

fn property_list(program: &Program<'_>, role: &str, version: &str, build: &str) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>CFBundleIdentifier</key><string>{IDENTIFIER}.{}</string>\n\
         <key>CFBundleName</key><string>{}</string>\n\
         <key>CFBundleGetInfoString</key><string>{}</string>\n\
         <key>CFBundleShortVersionString</key><string>{}</string>\n\
         <key>CFBundleVersion</key><string>{}</string>\n\
         <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>\n\
         </dict></plist>\n",
        escape(role),
        escape(program.binary),
        escape(program.description),
        escape(version),
        escape(build),
    )
}

/// A compiled Windows resource file holding one `VERSIONINFO` resource (US English, Unicode), in the layout of
/// the `.res` format: an empty entry first, then the resource with its header.
fn version_resource(program: &Program<'_>, version: &str, build: &str) -> Vec<u8> {
    let [major, minor, patch, number] = parts(version, build);
    let shown = format!("{major}.{minor}.{patch}.{number}");
    let high = |a: u16, b: u16| (u32::from(a) << 16) | u32::from(b);
    let mut fixed = Vec::with_capacity(52);
    for value in [
        0xFEEF_04BD,        // signature
        0x0001_0000,        // structure version
        high(major, minor), // file version
        high(patch, number),
        high(major, minor), // product version
        high(patch, number),
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,           // subtype
        0,           // date
        0,
    ] {
        fixed.extend_from_slice(&u32::to_le_bytes(value));
    }
    let file_name = format!("{}.exe", program.binary);
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", PUBLISHER),
        ("FileDescription", program.description),
        ("FileVersion", shown.as_str()),
        ("InternalName", program.binary),
        ("LegalCopyright", "Apache-2.0"),
        ("OriginalFilename", file_name.as_str()),
        ("ProductName", PRODUCT),
        ("ProductVersion", version),
    ]
    .into_iter()
    .map(|(key, value)| node(key, Value::Text(value), &[]))
    .collect();
    let table = node("040904b0", Value::None, &strings);
    let string_info = node("StringFileInfo", Value::None, &[table]);
    // Language 0x0409 (US English), code page 1200 (UTF-16).
    let translation = node("Translation", Value::Binary(&[0x09, 0x04, 0xB0, 0x04]), &[]);
    let var_info = node("VarFileInfo", Value::None, &[translation]);
    let data = node(
        "VS_VERSION_INFO",
        Value::Binary(&fixed),
        &[string_info, var_info],
    );

    let mut res = Vec::new();
    // The empty entry every `.res` file starts with.
    entry(&mut res, &[], 0, 0, 0, 0);
    // RT_VERSION (16), resource 1, moveable and pure, US English.
    entry(&mut res, &data, 16, 1, 0x0030, 0x0409);
    res
}

enum Value<'a> {
    None,
    Text(&'a str),
    Binary(&'a [u8]),
}

fn wide(bytes: &mut Vec<u8>, text: &str) {
    for unit in text.encode_utf16().chain([0]) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
}

fn align(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// One node of a version resource: its length, its value's length (in characters for text, bytes otherwise), its type
/// (1 text, 0 binary), its key, its value and its children, each starting on a 32-bit boundary.
fn node(key: &str, value: Value<'_>, children: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0; 6];
    wide(&mut bytes, key);
    align(&mut bytes);
    let (value_length, kind) = match value {
        Value::None => (0, 1u16),
        Value::Text(text) => {
            wide(&mut bytes, text);
            (text.encode_utf16().count() + 1, 1)
        }
        Value::Binary(data) => {
            bytes.extend_from_slice(data);
            (data.len(), 0)
        }
    };
    for child in children {
        align(&mut bytes);
        bytes.extend_from_slice(child);
    }
    let length = u16::try_from(bytes.len()).expect("a version resource node fits 64 KiB");
    let value_length = u16::try_from(value_length).expect("a version resource value fits 64 KiB");
    bytes[0..2].copy_from_slice(&length.to_le_bytes());
    bytes[2..4].copy_from_slice(&value_length.to_le_bytes());
    bytes[4..6].copy_from_slice(&kind.to_le_bytes());
    bytes
}

/// One entry of a `.res` file: a 32-byte header naming the type and the resource by number, then the data.
fn entry(res: &mut Vec<u8>, data: &[u8], kind: u16, name: u16, memory: u16, language: u16) {
    let size = u32::try_from(data.len()).expect("a resource fits 4 GiB");
    res.extend_from_slice(&size.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    for unit in [0xFFFF, kind, 0xFFFF, name] {
        res.extend_from_slice(&u16::to_le_bytes(unit));
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&memory.to_le_bytes());
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    align(res);
}
