//! How the owner gets bytes for the store: it runs the separate `peersus-store-fetch` helper and reads
//! back what that wrote. The owner never opens a network connection for this itself.
//!
//! What the helper returns is UNTRUSTED bytes. Nothing here checks them: [`crate::store_service`] does,
//! against the compiled-in key and the signed entry, before any byte is unpacked or used.
//!
//! The helper is found beside the application's own executable, as the module executors are, never
//! through `PATH` and never from anything a package says. It runs with an empty environment (no proxy
//! variables, no credentials), is killed if it does not finish in time, and writes to a NEW private file
//! inside a private directory that is removed afterwards.
use crate::catalog::Refusal;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    time::Duration,
};

/// Longest the helper may run. Its own timeouts are shorter; this is the owner's last word.
pub const HELPER_DEADLINE: Duration = Duration::from_secs(90);

/// Something that can fetch one named file from the store, at most `limit` bytes.
pub trait StoreFetcher: Send + Sync + 'static {
    /// Production discovers sources and per-version releases; legacy fixture fetchers
    /// may still supply one signed document to exercise the original trust path.
    fn branch_backed(&self) -> bool {
        false
    }

    fn fetch(
        &self,
        name: String,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Refusal>> + Send>>;
}

/// The production fetcher: the `peersus-store-fetch` helper process.
pub struct HelperFetcher {
    helper: PathBuf,
    scratch: PathBuf,
    #[cfg(all(
        feature = "private-store",
        not(any(target_os = "android", target_os = "ios"))
    ))]
    private_source: Option<std::sync::Arc<PrivateSource>>,
}

impl HelperFetcher {
    /// `executable` is the application's own absolute path; `scratch` a private directory.
    pub fn beside(executable: &Path, scratch: &Path) -> std::io::Result<Self> {
        if !executable.is_absolute() || !scratch.is_absolute() {
            return Err(std::io::Error::other("store helper paths must be absolute"));
        }
        let directory = executable
            .parent()
            .ok_or_else(|| std::io::Error::other("application directory unavailable"))?;
        Ok(Self {
            helper: directory.join(format!(
                "peersus-store-fetch{}",
                std::env::consts::EXE_SUFFIX
            )),
            scratch: scratch.to_owned(),
            #[cfg(all(
                feature = "private-store",
                not(any(target_os = "android", target_os = "ios"))
            ))]
            private_source: None,
        })
    }

    /// Configure only an explicitly private development build. The source is
    /// loaded once by the owner, not from a credential store for every file.
    #[cfg(all(
        feature = "private-store",
        not(any(target_os = "android", target_os = "ios"))
    ))]
    pub fn with_private_source(mut self, source: PrivateSource) -> Self {
        self.private_source = Some(std::sync::Arc::new(source));
        self
    }
}

/// Private helper input. The token is wiped on drop and redacted by its type;
/// this configuration is never part of preferences, argv or the environment.
#[cfg(all(
    feature = "private-store",
    not(any(target_os = "android", target_os = "ios"))
))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateSource {
    pub repository: String,
    pub directory: String,
    pub revision: String,
    pub token: peersus_core::model::SecretText,
}

impl StoreFetcher for HelperFetcher {
    fn branch_backed(&self) -> bool {
        true
    }

    fn fetch(
        &self,
        name: String,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Refusal>> + Send>> {
        let (helper, scratch) = (self.helper.clone(), self.scratch.clone());
        #[cfg(all(
            feature = "private-store",
            not(any(target_os = "android", target_os = "ios"))
        ))]
        if let Some(source) = self.private_source.clone() {
            return Box::pin(async move {
                let input = zeroize::Zeroizing::new(
                    serde_json::to_vec(source.as_ref()).map_err(|_| FAILED)?,
                );
                if input.len() > 16384 {
                    return Err(FAILED);
                }
                run_helper_input(&helper, &scratch, &name, limit, Some(&input)).await
            });
        }
        Box::pin(async move { run_helper(&helper, &scratch, &name, limit).await })
    }
}

const FAILED: Refusal = Refusal("The extension store could not be reached");

async fn run_helper(
    helper: &Path,
    scratch: &Path,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, Refusal> {
    run_helper_input(helper, scratch, name, limit, None).await
}

async fn run_helper_input(
    helper: &Path,
    scratch: &Path,
    name: &str,
    limit: usize,
    private_input: Option<&[u8]>,
) -> Result<Vec<u8>, Refusal> {
    // A private directory of its own, so the helper's `create_new` file can never collide with or
    // overwrite anything, and everything is removed when this returns.
    let holder = tempfile::Builder::new()
        .prefix(".peersus-store")
        .tempdir_in(scratch)
        .map_err(|_| FAILED)?;
    #[cfg(unix)]
    std::fs::set_permissions(
        holder.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .map_err(|_| FAILED)?;
    let output = holder.path().join("download");
    let mut command = tokio::process::Command::new(helper);
    if private_input.is_some() {
        command.arg("--private-github");
    }
    let mut child = command
        .arg(name)
        .arg(limit.to_string())
        .arg(&output)
        .env_clear()
        .stdin(if private_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| FAILED)?;
    let completion = async {
        if let Some(input) = private_input {
            use tokio::io::AsyncWriteExt;
            let mut pipe = child.stdin.take().ok_or(FAILED)?;
            pipe.write_all(input).await.map_err(|_| FAILED)?;
            pipe.shutdown().await.map_err(|_| FAILED)?;
            drop(pipe); // EOF is required before the helper can parse its input.
        }
        child.wait().await.map_err(|_| FAILED)
    };
    match tokio::time::timeout(HELPER_DEADLINE, completion).await {
        Ok(Ok(status)) if status.success() => {}
        Ok(_) => return Err(FAILED),
        Err(_) => {
            let _ = child.kill().await;
            return Err(FAILED);
        }
    }
    read_bounded(&output, limit)
}

/// Read what the helper wrote, refusing anything over `limit` WITHOUT reading it all: the helper is
/// trusted to cap its own output but the owner does not rely on that.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, Refusal> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| FAILED)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(Refusal("What the store sent is larger than allowed"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| FAILED)?;
    if bytes.len() > limit {
        return Err(Refusal("What the store sent is larger than allowed"));
    }
    Ok(bytes)
}

/// A fetcher over files held in memory, for tests that must not touch a process or a network. The files can be
/// changed between steps, so a test can serve a tampered archive or a newer catalog. Only under the same feature
/// as the other test support (`test-plugin`) and in unit tests: never part of a product build.
#[cfg(any(test, feature = "test-plugin"))]
pub mod fake {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    pub struct MapFetcher(pub Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

    impl MapFetcher {
        pub fn put(&self, name: &str, bytes: Vec<u8>) {
            self.0.lock().unwrap().insert(name.to_owned(), bytes);
        }
    }

    impl StoreFetcher for MapFetcher {
        fn fetch(
            &self,
            name: String,
            limit: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Refusal>> + Send>> {
            let found = self.0.lock().unwrap().get(&name).cloned();
            Box::pin(async move {
                match found {
                    Some(bytes) if bytes.len() <= limit => Ok(bytes),
                    Some(_) => Err(Refusal("What the store sent is larger than allowed")),
                    None => Err(FAILED),
                }
            })
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("peersus-store-fetch");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn run(helper: &Path, scratch: &Path, limit: usize) -> Result<Vec<u8>, Refusal> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_helper(helper, scratch, "catalog.json", limit))
    }

    #[cfg(feature = "private-store")]
    #[test]
    fn private_helper_receives_credentials_only_on_stdin_and_observes_eof() {
        let dir = tempfile::tempdir().unwrap();
        let helper = script(
            dir.path(),
            r#"
test "$#" = 4 || exit 1
test "$1" = "--private-github" || exit 2
test "$2" = "catalog.json" || exit 3
test -z "${CARGO_MANIFEST_DIR+x}" || exit 4
case "$*" in *fake-test-only*) exit 5;; esac
IFS= read -r input || :
case "$input" in *'"token":"fake-test-only"'*) ;; *) exit 6;; esac
printf private > "$4"
"#,
        );
        let fetcher = HelperFetcher::beside(&dir.path().join("peersus"), dir.path())
            .unwrap()
            .with_private_source(PrivateSource {
                repository: "example/private".to_owned(),
                directory: "catalog".to_owned(),
                revision: "main".to_owned(),
                token: peersus_core::model::SecretText::new("fake-test-only").unwrap(),
            });
        assert_eq!(fetcher.helper, helper);
        let bytes = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(3),
                    fetcher.fetch("catalog.json".into(), 16),
                )
                .await
                .expect("stdin was not closed")
            });
        assert_eq!(bytes, Ok(b"private".to_vec()));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn what_the_helper_writes_is_read_back_and_the_private_folder_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        // $3 is the output path the owner chose; the helper's own `create_new` is what protects it.
        let helper = script(dir.path(), r#"printf hello > "$3""#);
        assert_eq!(run(&helper, dir.path(), 16), Ok(b"hello".to_vec()));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers,
            ["peersus-store-fetch"],
            "the private folder was left behind: {leftovers:?}"
        );
    }

    #[test]
    fn a_helper_that_fails_or_writes_too_much_or_nothing_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            run(&script(dir.path(), "exit 1"), dir.path(), 16).is_err(),
            "failure status"
        );
        assert!(
            run(
                &script(dir.path(), r#"head -c 100 /dev/zero > "$3""#),
                dir.path(),
                16
            )
            .is_err(),
            "over the limit"
        );
        assert!(
            run(&script(dir.path(), "exit 0"), dir.path(), 16).is_err(),
            "success but no file"
        );
        assert!(
            run(&dir.path().join("missing"), dir.path(), 16).is_err(),
            "no helper installed"
        );
    }

    #[test]
    fn the_helper_runs_with_an_empty_environment() {
        let dir = tempfile::tempdir().unwrap();
        // `CARGO_MANIFEST_DIR` is in the environment of every test cargo runs, and no shell makes one up (unlike
        // PATH, which a shell given none sets to a default). A plain child must see it, so the proof cannot pass
        // vacuously; the helper, started with the environment cleared, must not.
        assert!(
            std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
            "this proof needs a variable that is set here"
        );
        let body = r#"if [ -z "${CARGO_MANIFEST_DIR+x}" ]; then printf clean > "$3"; else printf leaked > "$3"; fi"#;
        let helper = script(dir.path(), body);
        let control = std::process::Command::new(&helper)
            .arg("n")
            .arg("0")
            .arg(dir.path().join("control"))
            .status()
            .unwrap();
        assert!(control.success());
        assert_eq!(
            std::fs::read(dir.path().join("control")).unwrap(),
            b"leaked",
            "the control must see the variable"
        );
        assert_eq!(
            run(&helper, dir.path(), 16),
            Ok(b"clean".to_vec()),
            "the helper saw the owner's environment"
        );
    }

    #[test]
    fn paths_must_be_absolute() {
        assert!(HelperFetcher::beside(Path::new("relative/peersus"), Path::new("/tmp")).is_err());
        assert!(
            HelperFetcher::beside(Path::new("/usr/bin/peersus"), Path::new("relative")).is_err()
        );
        let fetcher =
            HelperFetcher::beside(Path::new("/opt/peersus/bin/peersus"), Path::new("/tmp"))
                .unwrap();
        assert_eq!(
            fetcher.helper,
            Path::new("/opt/peersus/bin/peersus-store-fetch")
        );
    }
}

/// One owner-wide polling budget, shared by startup, manual refresh and the timer.
pub(crate) struct RefreshSchedule {
    next: tokio::time::Instant,
    failures: u8,
}

impl Default for RefreshSchedule {
    fn default() -> Self {
        Self {
            next: tokio::time::Instant::now(),
            failures: 0,
        }
    }
}

impl RefreshSchedule {
    pub fn due(&self, now: tokio::time::Instant) -> bool {
        now >= self.next
    }

    pub fn started(&mut self, now: tokio::time::Instant) {
        self.next = now + Duration::from_secs(300);
    }

    pub fn finished(&mut self, success: bool, now: tokio::time::Instant) {
        self.failures = if success {
            0
        } else {
            self.failures.saturating_add(1).min(7)
        };
        let seconds = if success {
            300
        } else {
            (60u64 << (self.failures - 1)).min(3600)
        };
        self.next = now + Duration::from_secs(seconds);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn startup_manual_and_background_checks_share_a_bounded_retry_budget() {
        let mut schedule = RefreshSchedule::default();
        let start = tokio::time::Instant::now();
        assert!(schedule.due(start));
        schedule.started(start);
        assert!(!schedule.due(start + Duration::from_secs(299)));
        schedule.finished(false, start);
        assert!(!schedule.due(start + Duration::from_secs(59)));
        assert!(schedule.due(start + Duration::from_secs(60)));
        schedule.finished(false, start);
        assert!(!schedule.due(start + Duration::from_secs(119)));
        for _ in 0..20 {
            schedule.finished(false, start);
        }
        assert!(!schedule.due(start + Duration::from_secs(3599)));
        assert!(schedule.due(start + Duration::from_secs(3600)));
        schedule.finished(true, start);
        assert!(!schedule.due(start + Duration::from_secs(299)));
        assert!(schedule.due(start + Duration::from_secs(300)));
        schedule.finished(false, start);
        assert!(schedule.due(start + Duration::from_secs(60)));
    }
}
