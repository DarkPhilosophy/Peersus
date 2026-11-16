//! Explicit provisioning only. No credential is accepted through argv, environment or settings.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use peersus_upload::Credential;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use peersus_upload::desktop::{KEYRING_ACCOUNT, KEYRING_SERVICE};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::{self, BufRead, IsTerminal, Read, Write};

fn main() {
    if run().is_err() {
        eprintln!(
            "Credential was not stored. Check that an OS secret service is available and enter a valid non-owner token. No credential values are logged."
        );
        std::process::exit(1);
    }
}

/// The desktop uploader whose credential this stores exists for Linux and macOS only.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run() -> Result<(), ()> {
    eprintln!("This uploader does not run on this system.");
    Err(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run() -> Result<(), ()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("provision") || args.next().is_some() {
        eprintln!(
            "Usage: peersus-upload-credential provision\nRun interactively; never place credentials in command arguments or environment variables."
        );
        return Err(());
    }
    if !io::stdin().is_terminal() {
        return Err(());
    }
    eprintln!(
        "This stores/replaces the dedicated poo.wang machine token in your OS secret store. Only provision a token from a NON-OWNER account. Server token scopes are not enforced; an owner token grants full administrative access. Never give this command an owner token. Upload still requires choosing a file and explicit plaintext-upload consent each time.\nType STORE NON-OWNER to approve storage/replacement:"
    );
    let mut approval = String::new();
    io::stdin()
        .lock()
        .take(64)
        .read_line(&mut approval)
        .map_err(|_| ())?;
    if approval.trim() != "STORE NON-OWNER" {
        return Err(());
    }
    eprint!("Non-owner machine token (hidden): ");
    io::stderr().flush().map_err(|_| ())?;
    let token = rpassword::read_password().map_err(|_| ())?;
    let credential = Credential::new(token).map_err(|_| ())?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|_| ())?;
    credential.store(&entry).map_err(|_| ())?;
    eprintln!("Credential stored in the OS secret store. No network request was made.");
    Ok(())
}
