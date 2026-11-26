//! Install the window's glibc allocator default before the allocator starts.
use std::{ffi::OsStr, io, os::unix::process::CommandExt, path::Path, process::Command};

const THRESHOLD: &str = "MALLOC_MMAP_THRESHOLD_";
const MARKER: &str = "PEERSUS_LAUNCH_MMAP_DEFAULT";
const DEFAULT: &str = "4194304";
// exec preserves the PID. Another program inheriting the marker has a different PID and
// must not turn an explicit override (including 4194304) into an application default.
const REEXEC_PID: &str = "PEERSUS_WINDOW_MMAP_PID";

/// Re-execute the current window if its glibc startup settings need adjusting.
pub fn prepare() -> io::Result<()> {
    prepare_at(Path::new("/proc/self/exe"), std::env::args_os().skip(1))
}

fn prepare_at(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> io::Result<()> {
    let threshold = std::env::var_os(THRESHOLD);
    if threshold.as_ref().is_none_or(|value| value.is_empty()) {
        Err(Command::new(path)
            .args(args)
            .env(THRESHOLD, DEFAULT)
            .env(MARKER, DEFAULT)
            .env(REEXEC_PID, std::process::id().to_string())
            .exec())
    } else if std::env::var_os(MARKER).is_some() && !applied_here() {
        Err(Command::new(path)
            .args(args)
            .env_remove(MARKER)
            .env_remove(REEXEC_PID)
            .exec())
    } else {
        Ok(())
    }
}

fn applied_here() -> bool {
    std::env::var_os(THRESHOLD).as_deref() == Some(OsStr::new(DEFAULT))
        && std::env::var_os(MARKER).as_deref() == Some(OsStr::new(DEFAULT))
        && std::env::var_os(REEXEC_PID).as_deref()
            == Some(OsStr::new(&std::process::id().to_string()))
}

pub(crate) fn owner_env(command: &mut Command) {
    command.env_remove(REEXEC_PID);
    if applied_here() {
        command.env(MARKER, "window-owner");
    } else {
        command.env_remove(MARKER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_child() {
        let Some(mode) = std::env::var_os("PEERSUS_ALLOCATOR_TEST") else {
            return;
        };
        let Some(record) = std::env::var_os("PEERSUS_ALLOCATOR_RECORD") else {
            return;
        };
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(record)
            .unwrap();
        writeln!(
            log,
            "{} {} {}",
            std::process::id(),
            std::env::var(THRESHOLD).unwrap_or("-".into()),
            std::env::var(MARKER).unwrap_or("-".into())
        )
        .unwrap();
        if mode == "fail" {
            assert!(
                prepare_at(
                    Path::new("/proc/self/no-such-exe"),
                    std::env::args_os().skip(1)
                )
                .is_err()
            );
        } else {
            prepare().unwrap();
        }
        // The child starts with a cleared environment. Exercise the same exec-time reading the
        // memory gate uses, not just getenv's view.
        let captured = std::fs::read("/proc/self/environ").unwrap();
        let fixed = captured
            .split(|byte| *byte == 0)
            .any(|item| item == b"MALLOC_MMAP_THRESHOLD_=4194304");
        assert_eq!(
            fixed,
            matches!(mode.to_str(), Some("default" | "empty" | "stale"))
        );
        let mut owner = Command::new("unused");
        owner_env(&mut owner);
        let variables: std::collections::BTreeMap<_, _> = owner.get_envs().collect();
        assert_eq!(variables[OsStr::new(REEXEC_PID)], None);
        assert_eq!(
            variables[OsStr::new(MARKER)],
            matches!(mode.to_str(), Some("default" | "empty"))
                .then_some(OsStr::new("window-owner"))
        );
        writeln!(log, "returned").unwrap();
    }

    #[test]
    fn the_default_reexecs_once_and_explicit_values_survive() {
        for (mode, threshold, marker, states) in [
            ("default", None, None, vec!["- -", "4194304 4194304"]),
            (
                "empty",
                Some(""),
                Some("stale"),
                vec![" stale", "4194304 4194304"],
            ),
            ("explicit", Some("8192"), None, vec!["8192 -"]),
            (
                "stale",
                Some(DEFAULT),
                Some(DEFAULT),
                vec!["4194304 4194304", "4194304 -"],
            ),
            ("fail", None, None, vec!["- -"]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let record = dir.path().join("record");
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "launch_env::tests::allocator_child",
                    "--nocapture",
                ])
                .env_clear()
                .env("PEERSUS_ALLOCATOR_TEST", mode)
                .env("PEERSUS_ALLOCATOR_RECORD", &record);
            if let Some(value) = threshold {
                command.env(THRESHOLD, value);
            }
            if let Some(value) = marker {
                command.env(MARKER, value);
            }
            let mut child = command.spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("allocator child did not stop");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert!(status.success());
            let mut expected: Vec<_> = states
                .iter()
                .map(|state| format!("{} {state}", child.id()))
                .collect();
            expected.push("returned".into());
            let actual = std::fs::read_to_string(record).unwrap();
            assert_eq!(actual.lines().collect::<Vec<_>>(), expected, "{mode}");
        }
    }
}
