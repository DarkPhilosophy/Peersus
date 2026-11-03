//! `cargo xtask catalog-sign`: add one reviewed archive to the extension catalog and sign it.
//!
//! Maintainer-only; run by the protected `extension-approval` workflow, from TRUSTED code. The signing
//! seed is read ONLY from the `CATALOG_SIGNING_KEY` environment variable (base64 of the 32-byte
//! Ed25519 seed): never from a file, never printed, never written. Signing uses the same
//! `ed25519-dalek` the application verifies with, so the two cannot disagree about the primitive.
//!
//!   cargo xtask catalog-sign --archive A.tar.gz --report report.json --out DIR [--previous catalog.json]
//!
//! The document written is the one `peersus_app::catalog` verifies: schema 1, a rising `sequence`,
//! unique names, and for each entry the name, version, archive SHA-256 and size, author and link.
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::PathBuf};

struct Args {
    archive: PathBuf,
    report: PathBuf,
    out: PathBuf,
    previous: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut values: BTreeMap<&str, PathBuf> = BTreeMap::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument {flag:?}"))?;
        if !["archive", "report", "out", "previous"].contains(&name) {
            return Err(format!("unknown option --{name}"));
        }
        let value = iter
            .next()
            .ok_or_else(|| format!("--{name} needs a value"))?;
        if values.insert(name, PathBuf::from(value)).is_some() {
            return Err(format!("--{name} given twice"));
        }
    }
    let need = |name: &str| {
        values
            .get(name)
            .cloned()
            .ok_or_else(|| format!("--{name} is required"))
    };
    Ok(Args {
        archive: need("archive")?,
        report: need("report")?,
        out: need("out")?,
        previous: values.get("previous").cloned(),
    })
}

fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let mut next = || -> Option<u64> {
        let part = parts.next()?;
        (!part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0')))
        .then(|| part.parse().ok())
        .flatten()
    };
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the report has no {key}"))
}

/// Build the next catalog document and its signature. Pure: no I/O, so it can be tested.
pub fn next_catalog(
    seed: &[u8; 32],
    previous: Option<&Value>,
    report: &Value,
    archive_name: &str,
    archive: &[u8],
) -> Result<(Vec<u8>, [u8; 64], String), String> {
    let digest = hex::encode(Sha256::digest(archive));
    if text(report, "archive_sha256")? != digest
        || report.get("archive_size").and_then(Value::as_u64) != Some(archive.len() as u64)
    {
        return Err("the archive is not the one that was validated".into());
    }
    let (mut entries, sequence) = match previous {
        None => (Vec::new(), 0),
        Some(old) => {
            if old.get("schema").and_then(Value::as_u64) != Some(1) {
                return Err("the previous catalog has an unknown schema".into());
            }
            (
                old.get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or("the previous catalog has no entries")?,
                old.get("sequence")
                    .and_then(Value::as_u64)
                    .ok_or("the previous catalog has no sequence")?,
            )
        }
    };
    let name = text(report, "id")?.to_owned();
    let offered =
        version(text(report, "version")?).ok_or("the version is not major.minor.patch")?;
    if let Some(old) = entries
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(&name))
    {
        let have = old
            .get("version")
            .and_then(Value::as_str)
            .and_then(version)
            .ok_or("the old entry has no valid version")?;
        if offered <= have {
            return Err("an update must have a strictly newer version".into());
        }
    }
    entries.retain(|entry| entry.get("name").and_then(Value::as_str) != Some(&name));
    entries.push(json!({
        "archive": archive_name,
        "author": text(report, "author")?,
        "description": report.get("description").and_then(Value::as_str).unwrap_or(""),
        "link": text(report, "link")?,
        "name": name,
        "sha256": digest,
        "size": archive.len(),
        "version": text(report, "version")?,
    }));
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    // serde_json's map is ordered by key, so the bytes are canonical and reproducible.
    let body =
        serde_json::to_vec(&json!({"entries": entries, "schema": 1, "sequence": sequence + 1}))
            .map_err(|e| e.to_string())?;
    let signature = SigningKey::from_bytes(seed).sign(&body).to_bytes();
    Ok((
        body,
        signature,
        format!(
            "sequence {}: {} ({} entries)",
            sequence + 1,
            name,
            entries.len()
        ),
    ))
}

fn seed_from_env() -> Result<[u8; 32], String> {
    let encoded = std::env::var("CATALOG_SIGNING_KEY")
        .map_err(|_| "CATALOG_SIGNING_KEY is not set (run in the protected environment)")?;
    let bytes = decode_base64(encoded.trim()).ok_or("CATALOG_SIGNING_KEY is not valid base64")?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| "the signing key must be a 32-byte Ed25519 seed".to_owned())
}

/// Standard base64 with padding, no external crate: the key is the only input and is 44 characters.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes().take_while(|b| *b != b'=') {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = buffer << 6 | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let seed = seed_from_env()?;
    let report: Value = serde_json::from_slice(&fs::read(&args.report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let previous = match &args.previous {
        Some(path) if path.is_file() => Some(
            serde_json::from_slice::<Value>(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    let archive = fs::read(&args.archive).map_err(|e| e.to_string())?;
    let name = args
        .archive
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the archive has no usable name")?;
    let (body, signature, summary) =
        next_catalog(&seed, previous.as_ref(), &report, name, &archive)?;
    fs::create_dir(&args.out).map_err(|e| format!("cannot create the output directory: {e}"))?;
    fs::write(args.out.join("catalog.json"), body).map_err(|e| e.to_string())?;
    fs::write(args.out.join("catalog.sig"), signature).map_err(|e| e.to_string())?;
    fs::write(args.out.join(name), archive).map_err(|e| e.to_string())?;
    println!("{summary}");
    Ok(())
}

/// Standard base64 with padding, the inverse of [`decode_base64`].
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `cargo xtask catalog-keygen --out PATH`: make the maintainer's signing key.
///
/// The 32-byte seed comes from the operating system's random source and is written, base64, to a NEW
/// file with mode 0600 (an existing file is never overwritten). It is NEVER printed. Only the PUBLIC
/// key is printed, as the Rust array to commit into `MAINTAINER_PUBLIC_KEY`. Keep the file offline;
/// its contents are what goes into the protected `CATALOG_SIGNING_KEY` secret.
pub fn keygen(args: &[String]) -> Result<(), String> {
    use std::io::{Read, Write};
    // The seed comes from /dev/urandom and the file mode is unix-only; say so rather than fail obscurely.
    if !cfg!(unix) {
        return Err(
            "catalog-keygen currently runs on unix; generate the key on Linux or macOS".into(),
        );
    }
    let [flag, path] = args else {
        return Err("usage: catalog-keygen --out PATH".into());
    };
    if flag != "--out" {
        return Err("usage: catalog-keygen --out PATH".into());
    }
    let mut seed = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut seed))
        .map_err(|_| "no operating-system random source")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|error| {
        format!("cannot create {path}: {error} (an existing file is never overwritten)")
    })?;
    file.write_all(format!("{}\n", encode_base64(&seed)).as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    println!("Signing seed written to {path} (mode 0600). It is not shown and must stay offline.");
    println!(
        "Commit this PUBLIC key as MAINTAINER_PUBLIC_KEY in crates/peersus-app/src/store_trust.rs:"
    );
    println!("Some({public:?})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    fn report(archive: &[u8], version: &str) -> Value {
        json!({"id": "echo", "version": version, "author": "Someone", "link": "https://example.org/echo",
               "archive_sha256": hex::encode(Sha256::digest(archive)), "archive_size": archive.len()})
    }

    #[test]
    fn what_is_signed_verifies_and_sequence_rises() {
        let seed = [7u8; 32];
        let (body, signature, _) =
            next_catalog(&seed, None, &report(b"a", "1.0.0"), "echo.tar.gz", b"a").unwrap();
        SigningKey::from_bytes(&seed)
            .verifying_key()
            .verify(&body, &Signature::from_bytes(&signature))
            .unwrap();
        let first: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(first["sequence"], 1);
        let (next, _, _) = next_catalog(
            &seed,
            Some(&first),
            &report(b"b", "1.0.1"),
            "echo.tar.gz",
            b"b",
        )
        .unwrap();
        let second: Value = serde_json::from_slice(&next).unwrap();
        assert_eq!(second["sequence"], 2);
        assert_eq!(
            second["entries"].as_array().unwrap().len(),
            1,
            "the update replaces, it does not duplicate"
        );
    }

    #[test]
    fn a_report_for_other_bytes_or_an_old_version_is_refused() {
        let seed = [7u8; 32];
        assert!(
            next_catalog(
                &seed,
                None,
                &report(b"a", "1.0.0"),
                "e.tar.gz",
                b"different"
            )
            .is_err()
        );
        let (body, _, _) =
            next_catalog(&seed, None, &report(b"a", "2.0.0"), "e.tar.gz", b"a").unwrap();
        let old: Value = serde_json::from_slice(&body).unwrap();
        for version in ["2.0.0", "1.9.9"] {
            assert!(
                next_catalog(&seed, Some(&old), &report(b"c", version), "e.tar.gz", b"c").is_err(),
                "{version}"
            );
        }
    }

    #[test]
    fn the_key_is_standard_base64_of_exactly_32_bytes() {
        assert_eq!(
            decode_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="),
            Some((0u8..32).collect())
        );
        assert_eq!(decode_base64("not base64!"), None);
    }

    #[test]
    fn base64_round_trips_every_length_the_key_could_have() {
        for length in 0..40 {
            let bytes: Vec<u8> = (0..length as u8)
                .map(|b| b.wrapping_mul(37).wrapping_add(11))
                .collect();
            assert_eq!(
                decode_base64(&encode_base64(&bytes)),
                Some(bytes),
                "length {length}"
            );
        }
        assert_eq!(encode_base64(&[0u8; 32]).len(), 44);
    }
}
