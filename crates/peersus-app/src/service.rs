//! Starting the owner with the person's session (Linux, the systemd user manager).
//!
//! This is not the window's "run in the background": that keeps an owner a window started alive after the window goes.
//! Here the system starts the owner itself, with no window and no lease, at sign-in (or earlier where the user manager
//! already runs, see [`Status::lingering`]), and it stays until the session ends.
//!
//! The unit is written where the systemd user manager reads units (`$XDG_CONFIG_HOME/systemd/user/peersus.service`)
//! and enabled with `systemctl --user enable`. Nothing is installed system-wide, nothing asks for a password, and
//! turning it off only disables the unit: a running owner is never stopped and what was written is never removed.
//!
//! The owner the unit starts (`peersus owner --service`) waits for the system keyring without asking for it: before a sign-in
//! the keyring is absent or locked, and the owner starts neither its network nor its identity until the keyring can be
//! read without a prompt. An installation something else keeps current (Flatpak, AppImage) is refused: the unit would
//! point at a program that is not there after the session, or at one the host manager cannot see.

use crate::identity::{self, ServiceIdentityError};
use crate::instance::Instance;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// The unit's name in the person's user manager.
pub const UNIT: &str = "peersus.service";

/// How often the service looks at the keyring and at another owner while it waits.
const WAIT_EVERY: Duration = Duration::from_secs(2);

const UNAVAILABLE: &str = "The system's user service manager is not available, so Peersus cannot be started with your session.";
const REFUSED_ON: &str = "The system refused to turn the service on.";
const REFUSED_OFF: &str = "The system refused to turn the service off.";
const UNUSABLE_PROGRAM: &str = "The program to start cannot be used in a service file.";
const NOT_WRITTEN: &str = "The service file could not be written.";
const NO_DIRECTORY: &str =
    "This account has no configuration directory, so Peersus cannot be started with your session.";
const NOT_ACQUIRED: &str = "Cannot acquire the application's private directory";

/// What the system's manager says, read from it each time and never from a saved preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The unit file is where the user manager reads it.
    pub installed: bool,
    /// The user manager will start it with the person's session.
    pub enabled: bool,
    /// The service is running now. This is not a claim that it is connected: it waits for the keyring.
    pub running: bool,
    /// Whether the user manager runs before sign-in (`loginctl`'s Linger). `None` when it could not be asked.
    pub lingering: Option<bool>,
}

/// The person's own systemd user manager and unit directory.
#[derive(Debug, Clone)]
pub struct Manager {
    systemctl: PathBuf,
    loginctl: PathBuf,
    unit_directory: PathBuf,
    user: u32,
    /// Why this installation cannot have a unit of its own, when something else keeps it current.
    managed: Option<&'static str>,
}

impl Manager {
    /// The manager of this session, or why this installation cannot use one.
    pub fn detect() -> Result<Self, String> {
        if let Some(reason) = crate::update_install::managed() {
            return Err(reason.to_owned());
        }
        let unit_directory = unit_directory_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .ok_or_else(|| NO_DIRECTORY.to_owned())?;
        Ok(Self {
            systemctl: PathBuf::from("systemctl"),
            loginctl: PathBuf::from("loginctl"),
            unit_directory,
            user: rustix::process::geteuid().as_raw(),
            managed: None,
        })
    }

    /// A manager whose commands, directory and install kind are given: for tests that must not touch a session.
    #[cfg(test)]
    fn with(
        systemctl: PathBuf,
        loginctl: PathBuf,
        unit_directory: PathBuf,
        user: u32,
        managed: Option<&'static str>,
    ) -> Self {
        Self {
            systemctl,
            loginctl,
            unit_directory,
            user,
            managed,
        }
    }

    fn unit_path(&self) -> PathBuf {
        self.unit_directory.join(UNIT)
    }

    /// What the system says now: whether the unit is installed and enabled, whether it runs, and whether the user
    /// manager runs before sign-in.
    pub fn status(&self) -> Result<Status, String> {
        let enabled = self.is_enabled()?;
        let active_state = self.query(&self.systemctl, &["--user", "is-active", UNIT])?;
        Ok(Status {
            installed: self.unit_path().is_file(),
            enabled,
            running: active_state == "active",
            lingering: self.lingering(),
        })
    }

    /// Whether the user manager will start it with the person's session.
    pub fn is_enabled(&self) -> Result<bool, String> {
        let state = self.query(&self.systemctl, &["--user", "is-enabled", UNIT])?;
        match state.as_str() {
            "enabled" | "enabled-runtime" => Ok(true),
            "disabled" | "not-found" | "masked" | "masked-runtime" => Ok(false),
            _ => Err(UNAVAILABLE.to_owned()),
        }
    }

    /// Write the unit for `owner`, reload the manager and enable it. Writes nothing for an installation something
    /// else keeps current, and never starts or stops an owner.
    pub fn enable(&self, owner: &Path) -> Result<(), String> {
        if let Some(reason) = self.managed {
            return Err(reason.to_owned());
        }
        let owner = owner.to_str().ok_or_else(|| UNUSABLE_PROGRAM.to_owned())?;
        if !owner.starts_with('/') || owner.chars().any(char::is_control) {
            return Err(UNUSABLE_PROGRAM.to_owned());
        }
        self.write_unit(&unit_file(owner))?;
        self.run(&self.systemctl, &["--user", "daemon-reload"])
            .map_err(|_| REFUSED_ON.to_owned())?;
        self.run(&self.systemctl, &["--user", "enable", UNIT])
            .map_err(|_| REFUSED_ON.to_owned())?;
        Ok(())
    }

    /// Stop the system from starting it with the person's session. A running owner is left running, and the unit file
    /// is left in place: turning it on again needs no more than the enable.
    pub fn disable(&self) -> Result<(), String> {
        if self
            .run(&self.systemctl, &["--user", "disable", UNIT])
            .is_ok()
        {
            return Ok(());
        }
        match self.is_enabled() {
            Ok(false) => Ok(()),
            Ok(true) => Err(REFUSED_OFF.to_owned()),
            Err(reason) => Err(reason),
        }
    }

    fn write_unit(&self, contents: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.unit_directory).map_err(|_| NOT_WRITTEN.to_owned())?;
        let path = self.unit_path();
        // Never follow a temporary symlink or replace a unit the person maintains.
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.unit_directory)
            .map_err(|_| NOT_WRITTEN.to_owned())?;
        temporary
            .write_all(contents.as_bytes())
            .map_err(|_| NOT_WRITTEN.to_owned())?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => Ok(()),
            Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if path
                    .symlink_metadata()
                    .is_ok_and(|m| m.is_file() && m.len() == contents.len() as u64)
                    && std::fs::read(&path).is_ok_and(|existing| existing == contents.as_bytes())
                {
                    Ok(())
                } else {
                    Err(NOT_WRITTEN.to_owned())
                }
            }
            Err(_) => Err(NOT_WRITTEN.to_owned()),
        }
    }

    /// Ask the manager for one state, as text. A manager that cannot be reached is an error, never an "off".
    fn query(&self, program: &Path, arguments: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|_| UNAVAILABLE.to_owned())?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() {
            return Err(UNAVAILABLE.to_owned());
        }
        Ok(text)
    }

    fn run(&self, program: &Path, arguments: &[&str]) -> Result<(), ()> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|_| ())?;
        output.status.success().then_some(()).ok_or(())
    }

    fn lingering(&self) -> Option<bool> {
        let user = self.user.to_string();
        let output = Command::new(&self.loginctl)
            .args(["show-user", user.as_str(), "-p", "Linger"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "Linger=yes" => Some(true),
            "Linger=no" => Some(false),
            _ => None,
        }
    }
}

/// Where the systemd user manager reads this person's units, or `None` when neither variable gives a directory.
pub fn unit_directory_from(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("systemd/user"))
}

/// The unit that starts `owner` as the session's service.
fn unit_file(owner: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Peersus\n\
         # Started with the person's session; without lingering, that is at sign-in.\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} owner --service\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec_start(owner)
    )
}

/// The program path as one argument of `ExecStart`, quoted so that a space cannot split it, with the two characters
/// the unit file reads specially escaped.
fn exec_start(owner: &str) -> String {
    let mut quoted = String::with_capacity(owner.len() + 2);
    quoted.push('"');
    for character in owner.chars() {
        match character {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            // A specifier in a unit file; the application's own `%` is written twice.
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Wait until an owner may start here, then become it. `Ok(None)` means a signal asked this process to stop while it
/// waited, or the unit was turned off before it could serve; nothing was started in either case.
///
/// The existing identity is read before the application's directory is touched, so a service
/// that waits never keeps a window's owner from starting and setting up its identity. Another
/// owner is waited for rather than replaced: the same lock decides it, and nothing here can stop it.
///
/// A unit that had to wait is asked whether it is still enabled just before it takes the owner's place. Turning the
/// service off never stops an owner that already serves, but it does stop a waiting unit from starting to serve after
/// the person turned it off.
pub async fn acquire(directory: &Path) -> Result<Option<(Instance, iroh::SecretKey)>, String> {
    let manager = Manager::detect()?;
    let still_enabled = move || manager.is_enabled().ok();
    wait_for_owner(
        directory,
        WAIT_EVERY,
        || identity::service_identity(directory),
        still_enabled,
        stop_signal(),
    )
    .await
}

async fn wait_for_owner<K, Fut, E, S>(
    directory: &Path,
    every: Duration,
    keyring: K,
    still_enabled: E,
    stop: S,
) -> Result<Option<(Instance, iroh::SecretKey)>, String>
where
    K: Fn() -> Fut,
    Fut: Future<Output = Result<iroh::SecretKey, ServiceIdentityError>>,
    E: Fn() -> Option<bool>,
    S: Future<Output = ()>,
{
    tokio::pin!(stop);
    let mut said = Said(None);
    let mut waited = false;
    loop {
        let availability = tokio::select! {
            biased;
            () = &mut stop => return Ok(None),
            result = tokio::time::timeout(Duration::from_secs(8), keyring()) =>
                result.unwrap_or(Err(ServiceIdentityError::Unavailable)),
        };
        // A read can take time. Check enablement after it, before granting ownership.
        if waited {
            match still_enabled() {
                Some(false) => {
                    said.say(Phase::Off);
                    return Ok(None);
                }
                None => {
                    said.say(Phase::ManagerUnavailable);
                    tokio::select! {
                        () = &mut stop => return Ok(None),
                        () = tokio::time::sleep(every) => continue,
                    }
                }
                Some(true) => {}
            }
        }
        match availability {
            Ok(key) => match Instance::acquire(directory) {
                Ok(Some(instance)) => {
                    said.say(Phase::Starting);
                    return Ok(Some((instance, key)));
                }
                Ok(None) => {
                    waited = true;
                    said.say(Phase::AnotherOwner);
                }
                Err(_) => return Err(NOT_ACQUIRED.to_owned()),
            },
            Err(ServiceIdentityError::Locked) => {
                waited = true;
                said.say(Phase::Locked);
            }
            Err(ServiceIdentityError::Unavailable) => {
                waited = true;
                said.say(Phase::Unavailable);
            }
            Err(ServiceIdentityError::Missing) => {
                waited = true;
                said.say(Phase::MissingIdentity);
            }
            Err(ServiceIdentityError::Invalid(reason)) => {
                return Err(format!("{}{reason}", crate::IDENTITY_STARTUP_ERROR_PREFIX));
            }
        }
        tokio::select! {
            () = &mut stop => {
                said.say(Phase::Stopping);
                return Ok(None);
            }
            () = tokio::time::sleep(every) => {}
        }
    }
}

async fn stop_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(mut terminate), Ok(mut interrupt)) => {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        }
        (Ok(mut terminate), Err(_)) => {
            terminate.recv().await;
        }
        (Err(_), Ok(mut interrupt)) => {
            interrupt.recv().await;
        }
        (Err(_), Err(_)) => std::future::pending::<()>().await,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Locked,
    Unavailable,
    MissingIdentity,
    ManagerUnavailable,
    Starting,
    AnotherOwner,
    Off,
    Stopping,
}

/// Says what the service is waiting for, once per change: no path, identity or device ever reaches the journal.
struct Said(Option<Phase>);

impl Said {
    fn say(&mut self, phase: Phase) {
        if self.0 == Some(phase) {
            return;
        }
        self.0 = Some(phase);
        match phase {
            Phase::Locked => eprintln!(
                "Waiting for the system keyring to be unlocked; Peersus does not ask for it here"
            ),
            Phase::Unavailable => eprintln!(
                "Waiting for the system keyring; Peersus does not connect before it is available"
            ),
            Phase::MissingIdentity => eprintln!(
                "The service cannot read the existing identity; open the app in your session"
            ),
            Phase::ManagerUnavailable => eprintln!("Waiting for the system's user service manager"),
            Phase::Starting => eprintln!("The system keyring is available; starting Peersus"),
            Phase::AnotherOwner => {
                eprintln!("Another Peersus is running; waiting to take its place")
            }
            Phase::Off => {
                eprintln!("The service was turned off while it waited; nothing was started")
            }
            Phase::Stopping => eprintln!("Stopping"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    fn executable(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake_manager(root: &Path, enabled: &str, active: &str, linger: &str) -> (Manager, PathBuf) {
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("enabled"), enabled).unwrap();
        std::fs::write(state.join("active"), active).unwrap();
        let log = root.join("calls");
        let systemctl = executable(
            root,
            "systemctl",
            &format!(
                "#!/bin/sh\n\
                 echo \"$@\" >> '{}'\n\
                 case \"$*\" in\n\
                   *\"is-enabled\"*) cat '{}'; exit 0;;\n\
                   *\"is-active\"*) cat '{}'; exit 0;;\n\
                   *\"enable\"*) printf enabled > '{}'; exit 0;;\n\
                   *\"disable\"*) printf disabled > '{}'; exit 0;;\n\
                 esac\n\
                 exit 0\n",
                log.display(),
                state.join("enabled").display(),
                state.join("active").display(),
                state.join("enabled").display(),
                state.join("enabled").display(),
            ),
        );
        let loginctl = executable(
            root,
            "loginctl",
            &format!("#!/bin/sh\nprintf 'Linger={}\\n'\n", linger),
        );
        (
            Manager::with(
                systemctl,
                loginctl,
                root.join("config/systemd/user"),
                1000,
                None,
            ),
            log,
        )
    }

    #[test]
    fn the_unit_directory_is_the_one_the_user_manager_reads() {
        let home = OsString::from("/home/me");
        assert_eq!(
            unit_directory_from(Some(OsString::from("/cfg")), Some(home.clone())),
            Some(PathBuf::from("/cfg/systemd/user"))
        );
        assert_eq!(
            unit_directory_from(None, Some(home.clone())),
            Some(PathBuf::from("/home/me/.config/systemd/user"))
        );
        assert_eq!(
            unit_directory_from(Some(OsString::from("")), Some(home.clone())),
            Some(PathBuf::from("/home/me/.config/systemd/user"))
        );
        // A relative XDG_CONFIG_HOME is not a directory systemd can read; the home is used instead.
        assert_eq!(
            unit_directory_from(Some(OsString::from("cfg")), Some(home)),
            Some(PathBuf::from("/home/me/.config/systemd/user"))
        );
        assert_eq!(unit_directory_from(None, None), None);
    }

    #[test]
    fn the_unit_starts_the_owner_as_the_session_service_and_never_as_a_window() {
        let unit = unit_file("/home/me/.local/bin/peersus");
        assert!(unit.contains("ExecStart=\"/home/me/.local/bin/peersus\" owner --service\n"));
        assert!(unit.contains("WantedBy=default.target\n"));
        assert!(unit.contains("Restart=on-failure\n"));
        assert!(!unit.contains("PEERSUS_STARTED_FOR_A_WINDOW"));
        assert!(!unit.contains("--background"));
    }

    #[test]
    fn a_path_is_one_argument_however_it_is_written() {
        assert_eq!(exec_start("/usr/bin/peersus"), "\"/usr/bin/peersus\"");
        assert_eq!(exec_start("/home/a b/peersus"), "\"/home/a b/peersus\"");
        assert_eq!(exec_start("/home/a\"b/peersus"), "\"/home/a\\\"b/peersus\"");
        assert_eq!(exec_start("/home/a\\b/peersus"), "\"/home/a\\\\b/peersus\"");
        assert_eq!(exec_start("/home/100%/peersus"), "\"/home/100%%/peersus\"");
        assert_eq!(
            exec_start("/home/$name/peersus"),
            "\"/home/$$name/peersus\""
        );
    }

    #[test]
    fn the_status_is_what_the_system_says_and_not_a_saved_preference() {
        let root = tempfile::tempdir().unwrap();
        let (manager, _) = fake_manager(root.path(), "enabled", "active", "yes");
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), unit_file("/usr/bin/peersus")).unwrap();
        assert_eq!(
            manager.status().unwrap(),
            Status {
                installed: true,
                enabled: true,
                running: true,
                lingering: Some(true),
            }
        );

        let other = tempfile::tempdir().unwrap();
        let (manager, _) = fake_manager(other.path(), "disabled", "inactive", "no");
        assert_eq!(
            manager.status().unwrap(),
            Status {
                installed: false,
                enabled: false,
                running: false,
                lingering: Some(false),
            }
        );
    }

    #[test]
    fn enabling_writes_the_unit_then_reloads_and_enables_it() {
        let root = tempfile::tempdir().unwrap();
        let (manager, log) = fake_manager(root.path(), "disabled", "inactive", "no");
        manager.enable(Path::new("/home/a b/100%/peersus")).unwrap();
        let unit = std::fs::read_to_string(manager.unit_path()).unwrap();
        assert!(unit.contains("ExecStart=\"/home/a b/100%%/peersus\" owner --service\n"));
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "--user daemon-reload\n--user enable peersus.service\n"
        );
        assert!(manager.status().unwrap().enabled);
    }

    #[test]
    fn turning_it_off_does_not_stop_an_owner_or_remove_the_unit() {
        let root = tempfile::tempdir().unwrap();
        let (manager, log) = fake_manager(root.path(), "enabled", "active", "no");
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), unit_file("/usr/bin/peersus")).unwrap();
        manager.disable().unwrap();
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "--user disable peersus.service\n"
        );
        assert!(manager.unit_path().exists());
        assert!(manager.status().unwrap().running);
    }

    #[test]
    fn a_manager_that_cannot_be_reached_is_never_reported_as_off() {
        let root = tempfile::tempdir().unwrap();
        let failing = executable(root.path(), "systemctl", "#!/bin/sh\nexit 1\n");
        let manager = Manager::with(
            failing,
            PathBuf::from("/bin/false"),
            root.path().join("config/systemd/user"),
            1000,
            None,
        );
        assert_eq!(manager.status().unwrap_err(), UNAVAILABLE);
    }

    #[test]
    fn an_installation_something_else_keeps_current_writes_no_unit_and_asks_no_manager() {
        let root = tempfile::tempdir().unwrap();
        let (mut manager, log) = fake_manager(root.path(), "disabled", "inactive", "no");
        manager.managed = Some("This installation is updated by Flatpak");
        assert_eq!(
            manager.enable(Path::new("/opt/peersus")),
            Err("This installation is updated by Flatpak".to_owned())
        );
        assert!(!manager.unit_path().exists());
        assert!(!log.exists());
    }

    #[test]
    fn an_owner_path_that_cannot_be_a_service_argument_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (manager, log) = fake_manager(root.path(), "disabled", "inactive", "no");
        assert_eq!(
            manager.enable(Path::new("relative/peersus")),
            Err(UNUSABLE_PROGRAM.to_owned())
        );
        assert!(manager.enable(Path::new("/home/a\nb/peersus")).is_err());
        assert!(!manager.unit_path().exists());
        assert!(!log.exists());
    }

    fn ready_identity() -> Result<iroh::SecretKey, ServiceIdentityError> {
        Ok(iroh::SecretKey::generate())
    }

    #[tokio::test]
    async fn a_locked_keyring_or_missing_identity_is_waited_for_without_touching_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        for locked in [true, false] {
            let found = wait_for_owner(
                &directory,
                Duration::from_millis(5),
                || async {
                    if locked {
                        Err(ServiceIdentityError::Locked)
                    } else {
                        Err(ServiceIdentityError::Missing)
                    }
                },
                || Some(true),
                tokio::time::sleep(Duration::from_millis(30)),
            )
            .await
            .unwrap();
            assert!(found.is_none());
            assert!(!directory.exists());
        }
    }

    #[tokio::test]
    async fn the_owner_is_taken_only_when_the_previous_one_has_ended() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let held = Instance::acquire(&directory).unwrap().unwrap();
        let stop = async {
            tokio::time::sleep(Duration::from_millis(40)).await;
        };
        let found = wait_for_owner(
            &directory,
            Duration::from_millis(5),
            || async { ready_identity() },
            || Some(true),
            stop,
        )
        .await
        .unwrap();
        assert!(found.is_none(), "the running owner was still there");
        drop(held);
        let stop = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let found = wait_for_owner(
            &directory,
            Duration::from_millis(5),
            || async { ready_identity() },
            || Some(true),
            stop,
        )
        .await
        .unwrap();
        assert!(found.is_some(), "the place was free and nobody took it");
    }

    #[tokio::test]
    async fn the_keyring_is_asked_again_until_it_is_ready_and_then_the_owner_starts() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let stop = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let found = wait_for_owner(
            &directory,
            Duration::from_millis(5),
            move || {
                let next = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    match next {
                        0 => Err(ServiceIdentityError::Unavailable),
                        1 => Err(ServiceIdentityError::Missing),
                        _ => ready_identity(),
                    }
                }
            },
            || Some(true),
            stop,
        )
        .await
        .unwrap();
        assert!(found.is_some());
        assert!(asked.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn a_transient_manager_error_neither_stops_nor_grants_ownership() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let manager_queries = AtomicUsize::new(0);
        let keyring_queries = AtomicUsize::new(0);
        let instance = wait_for_owner(
            &directory,
            Duration::from_millis(1),
            || {
                let n = keyring_queries.fetch_add(1, Ordering::SeqCst);
                std::future::ready(if n == 0 {
                    Err(ServiceIdentityError::Locked)
                } else {
                    ready_identity()
                })
            },
            || (manager_queries.fetch_add(1, Ordering::SeqCst) != 0).then_some(true),
            tokio::time::sleep(Duration::from_secs(2)),
        )
        .await
        .unwrap();
        assert!(instance.is_some());
        assert_eq!(manager_queries.load(Ordering::SeqCst), 2);
        assert_eq!(keyring_queries.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn disabling_during_a_read_does_not_grant_ownership() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let enabled = std::cell::Cell::new(true);
        let attempts = std::cell::Cell::new(0);
        let found = wait_for_owner(
            &directory,
            Duration::from_millis(1),
            || async {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt == 0 {
                    return Err(ServiceIdentityError::Missing);
                }
                enabled.set(false);
                ready_identity()
            },
            || Some(enabled.get()),
            tokio::time::sleep(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        assert!(found.is_none());
        assert!(!directory.exists());
        assert_eq!(attempts.get(), 2);
    }

    #[tokio::test]
    async fn a_unit_turned_off_while_it_waited_does_not_take_the_place() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("state");
        let held = Instance::acquire(&directory).unwrap().unwrap();
        let enabled = Arc::new(AtomicBool::new(true));
        let flag = enabled.clone();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            flag.store(false, Ordering::SeqCst);
            drop(held);
        });
        let still = {
            let enabled = enabled.clone();
            move || Some(enabled.load(Ordering::SeqCst))
        };
        let stop = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let found = wait_for_owner(
            &directory,
            Duration::from_millis(5),
            || async { ready_identity() },
            still,
            stop,
        )
        .await
        .unwrap();
        release.await.unwrap();
        assert!(found.is_none());
    }
}
