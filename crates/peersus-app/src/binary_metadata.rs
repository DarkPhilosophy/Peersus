//! Identity, author and link read out of a program file WITHOUT running or loading it.
//!
//! A manifest is text and can be edited by anyone who can edit the archive. The same facts embedded
//! in the program itself are harder to alter unnoticed, so a package that claims an author and a link
//! must carry the same claim inside its binary. This is an IDENTITY CHECK, not a signature: the
//! trust root is the maintainer-signed catalog digest that covers the whole package, bytes included.
//!
//! Format. A section called `peersus.meta` (ELF and Mach-O, where the segment is `__DATA`; PE; and a
//! WebAssembly custom section of the same name) holding `MAGIC`, a little-endian `u32` length, then
//! that many bytes of JSON in the shape of [`peersus_plugin_api::package::Package`]. Parsing is
//! bounded, never decompresses, never follows relocations and never executes anything.
//!
//! A host-assigned plugin id is never read from here: the id stays the package folder's name.
use object::{Object, ObjectSection};
use peersus_plugin_api::package::Package;

/// Section name for ELF, Mach-O (in segment `__DATA`) and WebAssembly custom sections.
pub const SECTION: &str = "peersus.meta";
/// PE images store section names in eight bytes, so they cannot carry [`SECTION`]; this is the one
/// alias, used only for PE.
pub const PE_SECTION: &str = ".psmeta";
/// Marks the start of the record so that an empty or unrelated section is not read as one.
pub const MAGIC: [u8; 8] = *b"PSUSMETA";
/// Largest binary examined. Matches the module bound used by the isolated runtime.
pub const MAX_BINARY: usize = 128 * 1024 * 1024;
/// Largest JSON record. Package text limits already bound each field; this bounds the whole.
pub const MAX_RECORD: usize = 16 * 1024;

/// Why a program carried no usable record, in words this application chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Absent {
    /// Not a recognised executable, library or WebAssembly module.
    Unreadable,
    /// Readable, but it has no metadata section (or more than one: ambiguous).
    Missing,
    /// There is a section, and it is not a valid, bounded record.
    Malformed,
}

impl Absent {
    pub fn words(self) -> &'static str {
        match self {
            Self::Unreadable => "The program is not a recognised binary",
            Self::Missing => "The program carries no embedded metadata",
            Self::Malformed => "The embedded metadata is not valid",
        }
    }
}

/// The embedded record of `program`, read without executing it.
pub fn read(program: &[u8]) -> Result<Package, Absent> {
    if program.len() > MAX_BINARY {
        return Err(Absent::Unreadable);
    }
    let section = if program.starts_with(b"\0asm") { wasm_section(program)? } else { object_section(program)? };
    parse(section)
}

fn parse(section: &[u8]) -> Result<Package, Absent> {
    let rest = section.strip_prefix(&MAGIC).ok_or(Absent::Malformed)?;
    let (length, body) = rest.split_first_chunk::<4>().ok_or(Absent::Malformed)?;
    let length = u32::from_le_bytes(*length) as usize;
    if length == 0 || length > MAX_RECORD || body.len() < length {
        return Err(Absent::Malformed);
    }
    // Bytes after the record are padding; anything but zeros is refused rather than ignored.
    if body[length..].iter().any(|byte| *byte != 0) {
        return Err(Absent::Malformed);
    }
    let package: Package = serde_json::from_slice(&body[..length]).map_err(|_| Absent::Malformed)?;
    package.validate().then_some(package).ok_or(Absent::Malformed)
}

fn object_section(program: &[u8]) -> Result<&[u8], Absent> {
    let file = object::File::parse(program).map_err(|_| Absent::Unreadable)?;
    // One name per format, never both: a PE image cannot hold the long name, and accepting either
    // name everywhere would let one file carry two records that disagree.
    let wanted = match file.format() {
        object::BinaryFormat::Pe | object::BinaryFormat::Coff => PE_SECTION,
        _ => SECTION,
    };
    let mut found = None;
    for section in file.sections() {
        if section.name_bytes().is_ok_and(|name| name == wanted.as_bytes()) {
            if found.is_some() {
                return Err(Absent::Missing);
            }
            // Raw bytes: a compressed section is never inflated, so a small file cannot expand.
            found = Some(section.data().map_err(|_| Absent::Malformed)?);
        }
    }
    found.ok_or(Absent::Missing)
}

fn wasm_section(program: &[u8]) -> Result<&[u8], Absent> {
    let mut found = None;
    for payload in wasmparser::Parser::new(0).parse_all(program) {
        match payload.map_err(|_| Absent::Unreadable)? {
            wasmparser::Payload::CustomSection(custom) if custom.name() == SECTION => {
                if found.is_some() {
                    return Err(Absent::Missing);
                }
                found = Some(custom.data());
            }
            _ => {}
        }
    }
    found.ok_or(Absent::Missing)
}

/// Whether the embedded record says the same thing the manifest's `package` block says. Name,
/// version, author and link must agree exactly; description and changelog are free text and are
/// compared too, so a divergence anywhere is visible rather than silently preferred.
pub fn agrees(embedded: &Package, manifest: &Package) -> bool {
    embedded == manifest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        Package {
            name: "Thing".into(),
            version: "1.2.3".into(),
            description: "Does a thing.".into(),
            changelog: "Fixed.".into(),
            author: "Someone".into(),
            link: "https://example.org/thing".into(),
        }
    }

    pub(crate) fn record(package: &Package) -> Vec<u8> {
        let json = serde_json::to_vec(package).unwrap();
        let mut out = MAGIC.to_vec();
        out.extend((json.len() as u32).to_le_bytes());
        out.extend(json);
        out
    }

    /// A minimal WebAssembly module with one custom section.
    fn wasm_with(name: &str, data: &[u8]) -> Vec<u8> {
        let mut section = vec![name.len() as u8];
        section.extend(name.as_bytes());
        section.extend(data);
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.push(0);
        // LEB128 length (records here are small enough to need two bytes at most).
        let mut length = section.len();
        loop {
            let byte = (length & 0x7f) as u8;
            length >>= 7;
            if length == 0 {
                module.push(byte);
                break;
            }
            module.push(byte | 0x80);
        }
        module.extend(section);
        module
    }

    #[test]
    fn a_wasm_custom_section_is_read_without_running_anything() {
        let module = wasm_with(SECTION, &record(&package()));
        assert_eq!(read(&module), Ok(package()));
    }

    #[test]
    fn a_module_without_the_section_is_missing_not_malformed() {
        assert_eq!(read(&wasm_with("other", b"x")), Err(Absent::Missing));
        assert_eq!(read(b"\0asm\x01\0\0\0"), Err(Absent::Missing));
    }

    #[test]
    fn two_sections_of_the_same_name_are_ambiguous() {
        let mut module = wasm_with(SECTION, &record(&package()));
        let second = wasm_with(SECTION, &record(&package()));
        module.extend(&second[8..]);
        assert_eq!(read(&module), Err(Absent::Missing));
    }

    #[test]
    fn a_damaged_record_is_refused() {
        let good = record(&package());
        let mut wrong_magic = good.clone();
        wrong_magic[0] ^= 1;
        let mut huge = MAGIC.to_vec();
        huge.extend(u32::MAX.to_le_bytes());
        let mut trailing = good.clone();
        trailing.push(7);
        let mut bad_json = MAGIC.to_vec();
        bad_json.extend(2u32.to_le_bytes());
        bad_json.extend(b"{}");
        let mut bad_link = package();
        bad_link.link = "http://example.org/thing".into();
        let mut unknown_key = MAGIC.to_vec();
        let json = br#"{"name":"T","version":"1.0.0","id":"other"}"#;
        unknown_key.extend((json.len() as u32).to_le_bytes());
        unknown_key.extend(json);
        for (what, bytes) in [
            ("magic", wrong_magic),
            ("length", huge),
            ("trailing junk", trailing),
            ("missing fields", bad_json),
            ("insecure link", record(&bad_link)),
            ("a self-assigned id", unknown_key),
            ("short", MAGIC[..4].to_vec()),
        ] {
            assert_eq!(read(&wasm_with(SECTION, &bytes)), Err(Absent::Malformed), "{what} was accepted");
        }
    }

    #[test]
    fn zero_padding_after_the_record_is_tolerated() {
        let mut padded = record(&package());
        padded.extend([0; 5]);
        assert_eq!(read(&wasm_with(SECTION, &padded)), Ok(package()));
    }

    #[test]
    fn something_that_is_not_a_binary_is_unreadable() {
        for bytes in [&b""[..], b"#!/bin/sh\necho hi\n", b"PK\x03\x04zip", &[0u8; 64]] {
            assert_eq!(read(bytes), Err(Absent::Unreadable));
        }
    }

    #[test]
    fn an_edited_manifest_no_longer_agrees_with_the_binary() {
        let embedded = package();
        assert!(agrees(&embedded, &package()));
        for change in [
            (|p: &mut Package| p.author = "Someone else".into()) as fn(&mut Package),
            |p| p.link = "https://example.org/other".into(),
            |p| p.version = "1.2.4".into(),
            |p| p.name = "Other".into(),
        ] {
            let mut manifest = package();
            change(&mut manifest);
            assert!(!agrees(&embedded, &manifest));
        }
    }
}
