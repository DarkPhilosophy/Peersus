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
            if !matches!(old.get("schema").and_then(Value::as_u64), Some(1 | 2)) {
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
        serde_json::to_vec(&json!({"entries": entries, "schema": previous.and_then(|old| old.get("schema")).and_then(Value::as_u64).unwrap_or(1), "sequence": sequence.checked_add(1).ok_or("catalog sequence exhausted")?}))
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

/// Sign one reviewed plugin version with every platform build in the same catalog entry.
/// The bytes come from the independent review artifact, never from a privileged rebuild.
fn next_bundle(
    seed: &[u8; 32],
    previous: Option<&Value>,
    review: &Value,
    archives: &BTreeMap<String, Vec<u8>>,
) -> Result<(Vec<u8>, [u8; 64], String), String> {
    if review.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err("unknown build review schema".into());
    }
    let packages = review
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("missing reviewed packages")?;
    if packages.is_empty() || packages.len() > 32 || archives.len() != packages.len() {
        return Err("expected between one and 32 uniquely named reviewed packages".into());
    }
    let id = text(review, "id")?;
    let mut artifacts = Vec::new();
    let mut targets = std::collections::BTreeSet::new();
    let mut names = std::collections::BTreeSet::new();
    let first = &packages[0];
    for package in packages {
        if text(package, "id")? != id
            || ["id", "version", "author", "link", "description"]
                .iter()
                .any(|key| package.get(key) != first.get(key))
        {
            return Err("platform packages disagree about their plugin identity or version".into());
        }
        let name = text(package, "archive")?;
        if !names.insert(name) {
            return Err("duplicate reviewed archive".into());
        }
        let bytes = archives.get(name).ok_or("a reviewed archive is missing")?;
        let digest = hex::encode(Sha256::digest(bytes));
        if text(package, "archive_sha256")? != digest
            || package.get("archive_size").and_then(Value::as_u64) != Some(bytes.len() as u64)
        {
            return Err("a platform archive differs from its validated bytes".into());
        }
        let architecture = text(package, "architecture")?;
        if !["any", "aarch64", "x86_64", "arm", "x86"].contains(&architecture) {
            return Err("unknown platform architecture".into());
        }
        let platforms = package
            .get("platforms")
            .and_then(Value::as_array)
            .ok_or("missing platforms")?;
        if platforms.is_empty() {
            return Err("a package must support a platform".into());
        }
        for platform in platforms {
            let platform = platform.as_str().ok_or("platform must be text")?;
            if !["android", "linux", "macos", "windows"].contains(&platform)
                || targets.iter().any(|(os, arch): &(&str, &str)| {
                    *os == platform
                        && (*arch == architecture || *arch == "any" || architecture == "any")
                })
            {
                return Err("unknown or overlapping platform artifacts".into());
            }
            targets.insert((platform, architecture));
            artifacts.push(json!({"platform": platform, "architecture": architecture,
                "archive": name, "sha256": digest, "size": bytes.len()}));
        }
    }
    if artifacts.len() > 32 {
        return Err("too many platform artifacts".into());
    }
    let first_name = text(first, "archive")?;
    let (legacy, _, summary) =
        next_catalog(seed, previous, first, first_name, &archives[first_name])?;
    let mut document: Value = serde_json::from_slice(&legacy).map_err(|error| error.to_string())?;
    document["schema"] = json!(2);
    let entry = document["entries"]
        .as_array_mut()
        .ok_or("missing catalog entries")?
        .iter_mut()
        .find(|entry| entry["name"].as_str() == Some(id))
        .ok_or("missing plugin entry")?;
    let entry = entry.as_object_mut().ok_or("invalid plugin entry")?;
    for key in ["archive", "sha256", "size"] {
        entry.remove(key);
    }
    entry.insert("artifacts".into(), Value::Array(artifacts));
    let body = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
    let signature = SigningKey::from_bytes(seed).sign(&body).to_bytes();
    Ok((body, signature, summary))
}

fn run_bundle(args: &[String]) -> Result<(), String> {
    let mut values = BTreeMap::new();
    let mut arguments = args.iter();
    while let Some(flag) = arguments.next() {
        if !["--review", "--out", "--previous"].contains(&flag.as_str()) {
            return Err(format!("unknown bundle option {flag}"));
        }
        let value = arguments.next().ok_or("bundle option needs a value")?;
        if values.insert(flag.as_str(), PathBuf::from(value)).is_some() {
            return Err("duplicate bundle option".into());
        }
    }
    let review_path = values.get("--review").ok_or("--review is required")?;
    let out = values.get("--out").ok_or("--out is required")?;
    let read_json = |path: &PathBuf| -> Result<Value, String> {
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    };
    let review = read_json(review_path)?;
    let previous = values.get("--previous").map(read_json).transpose()?;
    let packages = review
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("missing reviewed packages")?;
    let mut archives = BTreeMap::new();
    for package in packages {
        let name = text(package, "archive")?;
        if name.is_empty()
            || name.starts_with('.')
            || name.len() > 128
            || !name.ends_with(".tar.gz")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("invalid reviewed archive basename".into());
        }
        let path = review_path
            .parent()
            .ok_or("review needs a parent directory")?
            .join(name);
        if fs::symlink_metadata(&path)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("a reviewed archive must not be a symbolic link".into());
        }
        if archives
            .insert(
                name.to_owned(),
                fs::read(path).map_err(|error| error.to_string())?,
            )
            .is_some()
        {
            return Err("duplicate reviewed archive".into());
        }
    }
    let (body, signature, summary) =
        next_bundle(&seed_from_env()?, previous.as_ref(), &review, &archives)?;
    fs::create_dir(out).map_err(|error| error.to_string())?;
    fs::write(out.join("catalog.json"), body).map_err(|error| error.to_string())?;
    fs::write(out.join("catalog.sig"), signature).map_err(|error| error.to_string())?;
    for (name, bytes) in archives {
        fs::write(out.join(name), bytes).map_err(|error| error.to_string())?;
    }
    println!("{summary}");
    Ok(())
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
    if args.iter().any(|argument| argument == "--review") {
        return run_bundle(args);
    }
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
    fn reviewed_platform_bundle_is_one_signed_entry_and_binds_every_archive() {
        let seed = [7; 32];
        let mut linux = report(b"linux", "1.0.0");
        linux["archive"] = json!("upload-linux.tar.gz");
        linux["id"] = json!("upload");
        linux["platforms"] = json!(["linux"]);
        linux["architecture"] = json!("x86_64");
        let mut android = report(b"android", "1.0.0");
        android["archive"] = json!("upload-android.tar.gz");
        android["id"] = json!("upload");
        android["platforms"] = json!(["android"]);
        android["architecture"] = json!("any");
        let mut review = json!({"schema": 1, "id": "upload", "packages": [linux, android]});
        let mut archives = BTreeMap::from([
            ("upload-linux.tar.gz".into(), b"linux".to_vec()),
            ("upload-android.tar.gz".into(), b"android".to_vec()),
        ]);
        let (body, signature, _) = next_bundle(&seed, None, &review, &archives).unwrap();
        SigningKey::from_bytes(&seed)
            .verifying_key()
            .verify(&body, &Signature::from_bytes(&signature))
            .unwrap();
        let catalog: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(catalog["schema"], 2);
        assert_eq!(catalog["entries"].as_array().unwrap().len(), 1);
        assert_eq!(
            catalog["entries"][0]["artifacts"].as_array().unwrap().len(),
            2
        );
        assert!(next_bundle(&seed, Some(&catalog), &review, &archives).is_err());
        archives.insert("upload-android.tar.gz".into(), b"tampered".to_vec());
        assert!(next_bundle(&seed, None, &review, &archives).is_err());
        archives.insert("upload-android.tar.gz".into(), b"android".to_vec());
        review["packages"][1]["platforms"] = json!(["linux"]);
        assert!(next_bundle(&seed, None, &review, &archives).is_err());
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
