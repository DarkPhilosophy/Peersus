//! Native Linux/macOS adapter. Results and secrets never cross the hub.
use crate::{Credential, SelectedFile, Uploader};
use peersus_api::message::{Delivery, Operation, Outcome};
use peersus_plugin::{
    frame::{self, CONTROL_LIMIT},
    link::Hello,
    supervisor::Bootstrap,
};
use rfd::{AsyncMessageDialog, MessageButtons, MessageDialogResult};
use serde_json::json;

pub const KEYRING_SERVICE: &str = "org.peersus.upload.poo.wang";
pub const KEYRING_ACCOUNT: &str = "non-owner-machine-token";

async fn message(text: &str) {
    AsyncMessageDialog::new()
        .set_title("Peersus upload")
        .set_description(text)
        .show()
        .await;
}
async fn confirm(text: &str, button: &str) -> bool {
    let response = AsyncMessageDialog::new()
        .set_title("Peersus upload — approval required")
        .set_description(text)
        .set_buttons(MessageButtons::OkCancelCustom(
            button.into(),
            "Cancel".into(),
        ))
        .show()
        .await;
    response == MessageDialogResult::Custom(button.into())
}
fn credential_setup_command(executable: &std::path::Path, flatpak: Option<&str>) -> Option<String> {
    if !executable.is_absolute() {
        return None;
    }
    let helper = executable.parent()?.join("peersus-upload-credential");
    let helper = helper.to_str()?;
    if helper.chars().any(char::is_control) {
        return None;
    }
    let quoted = format!("'{}'", helper.replace('\'', "'\\''"));
    match flatpak {
        Some(id)
            if !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) =>
        {
            Some(format!("flatpak run --command={quoted} '{id}' provision"))
        }
        Some(_) => None,
        None => Some(format!("{quoted} provision")),
    }
}

async fn flow() -> &'static str {
    #[cfg(target_os = "linux")]
    if !std::process::Command::new("zenity")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return "dialog-unavailable";
    }
    let Some(chosen) = rfd::AsyncFileDialog::new()
        .set_title("Choose one file to upload (server ceiling 250 MiB)")
        .pick_file()
        .await
    else {
        return "cancelled";
    };
    let selected = match SelectedFile::open(chosen.path()).await {
        Ok(file) => file,
        Err(error) => {
            message(&error.to_string()).await;
            return "failed";
        }
    };
    let disclosure = format!(
        "Upload {} ({} bytes) to poo.wang?\n\nThis is NOT Peersus end-to-end encrypted transfer. The server receives plaintext and can read this file. Anyone with the public /f/ link can access it, even though its visibility is called private. It will not be listed in Showcase. Requested retention: 24 hours; the server controls actual expiry. Server operational ceiling: 250 MiB; your account may allow less. Larger files use sequential bounded session parts. A private temporary snapshot preserves the exact bytes selected.\n\nUpload also authorizes reading the dedicated machine credential from your OS secret store for this one action. Use only a non-owner account token: owner tokens are fully privileged, regardless of scopes.\n\nUse the host's Cancel action to stop waiting. A timeout or cancellation after transmission may leave a server copy or uncommitted parts; no automatic retries are performed. Nothing will open or copy automatically.",
        selected.name(),
        selected.size()
    );
    if !confirm(&disclosure, "Upload plaintext file").await {
        return "cancelled";
    }
    // No keyring access, request or setup occurs at plugin startup or before approval.
    let credential = tokio::task::spawn_blocking(|| {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).ok()?;
        Credential::new(entry.get_password().ok()?).ok()
    })
    .await
    .ok()
    .flatten();
    let Some(credential) = credential else {
        let command = std::env::current_exe().ok().and_then(|executable| {
            credential_setup_command(&executable, std::env::var("FLATPAK_ID").ok().as_deref())
        });
        let setup = command.map_or_else(
            || "Run the companion credential helper shipped beside this extension, with the provision argument.".to_owned(),
            |command| format!("Keep this dialog open and run the following command in a terminal:\n\n{command}"),
        );
        message(&format!("No usable upload credential is available.\n\n{setup}\n\nExplicitly approve storage and enter a non-owner machine token at its hidden prompt. Never put a token in the command or use an owner token. Nothing was uploaded.")).await;
        return "credential-required";
    };
    let uploader = match Uploader::production() {
        Ok(value) => value,
        Err(error) => {
            message(&error.to_string()).await;
            return "failed";
        }
    };
    let link = match uploader.upload(selected, credential).await {
        Ok(link) => link,
        Err(error) => {
            message(&error.to_string()).await;
            return "failed";
        }
    };
    let mut clipboard_owner: Option<arboard::Clipboard> = None;
    loop {
        let response = AsyncMessageDialog::new().set_title("Upload completed")
            .set_description(format!("Public link (anyone with it can read the plaintext file):\n{link}\n\nCopy changes your clipboard. Open launches your browser only when selected. This link is not sent to any paired device."))
            .set_buttons(MessageButtons::YesNoCancelCustom("Copy link".into(), "Open link".into(), "Done".into())).show().await;
        match response {
            MessageDialogResult::Custom(label) if label == "Copy link" => {
                // Clipboard ownership is retained until the result dialog closes.
                if clipboard_owner.is_none() {
                    clipboard_owner = arboard::Clipboard::new().ok();
                }
                let Some(clipboard) = clipboard_owner.as_mut() else {
                    message("Clipboard unavailable; the link remains visible in this dialog.")
                        .await;
                    continue;
                };
                if clipboard.set_text(link.clone()).is_err() {
                    message("Could not copy the link.").await;
                } else {
                    message(&format!("Link copied. Keep this dialog open while pasting on clipboard systems without a manager.\n{link}")).await;
                }
            }
            MessageDialogResult::Custom(label) if label == "Open link" => {
                #[cfg(target_os = "linux")]
                let command = "xdg-open";
                #[cfg(target_os = "macos")]
                let command = "open";
                // Fixed executable + validated canonical URL; no shell or server-controlled arguments.
                match std::process::Command::new(command).arg(&link).spawn() {
                    Ok(mut child) => {
                        let _ = tokio::task::spawn_blocking(move || child.wait()).await;
                    }
                    Err(_) => {
                        message("Could not open your browser. Copy the visible link instead.").await
                    }
                }
            }
            _ => return "completed",
        }
    }
}

/// One reader owns framing, so dialog/network completion can never cancel a partial frame.
pub async fn run(bootstrap: Bootstrap) {
    let Ok(mut stream) = peersus_plugin::transport::connect(bootstrap.socket).await else {
        return;
    };
    if frame::write(
        &mut stream,
        &Hello {
            token: bootstrap.token,
        },
        CONTROL_LIMIT,
    )
    .await
    .is_err()
    {
        return;
    }
    run_stream(stream, flow).await;
}

// Injectable only inside this adapter: fixtures exercise the same framing and lifecycle
// without opening UI, reading credentials, or making requests.
async fn run_stream<F, A>(stream: peersus_plugin::transport::Stream, mut start: F)
where
    F: FnMut() -> A,
    A: Future<Output = &'static str>,
{
    let (mut reader, mut writer) = stream.into_split();
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let reading = tokio::spawn(async move {
        while let Ok(delivery) = frame::read::<Delivery>(&mut reader, CONTROL_LIMIT).await {
            if send.send(delivery).await.is_err() {
                break;
            }
        }
    });
    'calls: while let Some(delivery) = receive.recv().await {
        let Delivery::Call {
            ticket,
            caller,
            service,
            version,
            method,
            args,
        } = delivery
        else {
            continue;
        };
        if caller != "host"
            || service != "upload.file"
            || version != 1
            || method != "choose"
            || !(args.is_null() || args == json!({"peer":null}))
        {
            if frame::write(
                &mut writer,
                &Operation::Reply {
                    ticket,
                    result: Outcome::error("invalid-call", "Unsupported upload action"),
                },
                CONTROL_LIMIT,
            )
            .await
            .is_err()
            {
                break;
            }
            continue;
        }
        let action = start();
        tokio::pin!(action);
        let status = loop {
            tokio::select! {
                biased;
                // Consume queued cancellation before a simultaneously ready completion.
                delivery = receive.recv() => match delivery {
                    Some(Delivery::Cancelled { ticket: cancelled }) if cancelled == ticket => {
                        // Dropping the inline flow prevents any subsequent upload work.
                        // Native portal UI may linger, but cannot resume this dropped flow.
                        // The hub already removed this ticket: never send a late reply.
                        continue 'calls;
                    }
                    Some(Delivery::Call { ticket: other, .. }) if frame::write(&mut writer, &Operation::Reply { ticket: other, result: Outcome::error("busy", "An upload dialog is already active") }, CONTROL_LIMIT).await.is_err() => break 'calls,
                    None => break 'calls,
                    _ => {}
                },
                status = &mut action => break status,
            }
        };
        let result = match status {
            "cancelled" => Outcome::error("cancelled", "Upload cancelled"),
            "dialog-unavailable" => {
                Outcome::error("unavailable", "Native dialogs require zenity on Linux")
            }
            "failed" | "credential-required" => Outcome::error(
                "unavailable",
                "Upload was not completed; see the plugin's native dialog",
            ),
            _ => Outcome::ok(json!({"status":status})),
        };
        if frame::write(
            &mut writer,
            &Operation::Reply { ticket, result },
            CONTROL_LIMIT,
        )
        .await
        .is_err()
        {
            break;
        }
    }
    reading.abort();
}

#[cfg(test)]
mod runner_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    #[test]
    fn credential_setup_uses_the_installed_companion_not_path_lookup() {
        assert_eq!(
            credential_setup_command(
                std::path::Path::new("/opt/test plugins/upload/peersus-upload"),
                None,
            ),
            Some("'/opt/test plugins/upload/peersus-upload-credential' provision".into()),
        );
        assert_eq!(
            credential_setup_command(
                std::path::Path::new("/opt/owner's plugins/peersus-upload"),
                None,
            ),
            Some("'/opt/owner'\\''s plugins/peersus-upload-credential' provision".into()),
        );
    }

    #[test]
    fn flatpak_setup_runs_the_companion_inside_its_sandbox() {
        assert_eq!(
            credential_setup_command(
                std::path::Path::new("/app/share/peersus/plugins/upload/peersus-upload"),
                Some("dev.example.Application"),
            ),
            Some("flatpak run --command='/app/share/peersus/plugins/upload/peersus-upload-credential' 'dev.example.Application' provision".into()),
        );
    }

    #[test]
    fn credential_setup_refuses_ambiguous_or_control_character_paths() {
        for executable in ["peersus-upload", "/tmp/bad\npath/peersus-upload"] {
            assert!(credential_setup_command(std::path::Path::new(executable), None).is_none());
        }
        assert!(
            credential_setup_command(
                std::path::Path::new("/app/peersus-upload"),
                Some("invalid\napplication"),
            )
            .is_none()
        );
    }

    struct DropNotice(u64, mpsc::UnboundedSender<u64>);
    impl Drop for DropNotice {
        fn drop(&mut self) {
            let _ = self.1.send(self.0);
        }
    }

    fn call(ticket: u64) -> Delivery {
        Delivery::Call {
            ticket,
            caller: "host".into(),
            service: "upload.file".into(),
            version: 1,
            method: "choose".into(),
            args: serde_json::Value::Null,
        }
    }

    async fn pair() -> (
        peersus_plugin::transport::Stream,
        peersus_plugin::transport::Stream,
    ) {
        let directory = tempfile::tempdir().unwrap();
        peersus_plugin::transport::connected_pair(directory.path())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn six_cancellations_drop_flows_and_seventh_call_completes() {
        let (mut host, plugin) = pair().await;
        let starts = Arc::new(AtomicUsize::new(0));
        let (started, mut starting) = mpsc::unbounded_channel();
        let (dropped, mut dropping) = mpsc::unbounded_channel();
        let count = starts.clone();
        let runner = tokio::spawn(run_stream(plugin, move || {
            let number = count.fetch_add(1, Ordering::SeqCst) as u64 + 1;
            let started = started.clone();
            let notice = DropNotice(number, dropped.clone());
            async move {
                let _notice = notice;
                started.send(number).unwrap();
                if number <= 6 {
                    std::future::pending::<()>().await;
                }
                "uploaded"
            }
        }));
        timeout(Duration::from_secs(5), async {
            for ticket in 1..=6 {
                frame::write(&mut host, &call(ticket), CONTROL_LIMIT)
                    .await
                    .unwrap();
                assert_eq!(starting.recv().await, Some(ticket));
                frame::write(&mut host, &Delivery::Cancelled { ticket }, CONTROL_LIMIT)
                    .await
                    .unwrap();
                assert_eq!(dropping.recv().await, Some(ticket));
                assert!(!runner.is_finished());
            }
            frame::write(&mut host, &call(7), CONTROL_LIMIT)
                .await
                .unwrap();
            // A reply to any cancelled ticket would be the first frame instead.
            assert_eq!(
                frame::read::<Operation>(&mut host, CONTROL_LIMIT)
                    .await
                    .unwrap(),
                Operation::Reply {
                    ticket: 7,
                    result: Outcome::ok(json!({"status":"uploaded"}))
                }
            );
            assert_eq!(starts.load(Ordering::SeqCst), 7);
            drop(host);
            runner.await.unwrap();
        })
        .await
        .expect("runner must survive repeated cancellation and close on EOF");
    }

    #[tokio::test]
    async fn native_cancellation_is_not_a_successful_upload() {
        let (mut host, plugin) = pair().await;
        let runner = tokio::spawn(run_stream(plugin, || async { "cancelled" }));
        timeout(Duration::from_secs(5), async {
            frame::write(&mut host, &call(1), CONTROL_LIMIT)
                .await
                .unwrap();
            assert_eq!(
                frame::read::<Operation>(&mut host, CONTROL_LIMIT)
                    .await
                    .unwrap(),
                Operation::Reply {
                    ticket: 1,
                    result: Outcome::error("cancelled", "Upload cancelled")
                }
            );
            drop(host);
            runner.await.unwrap();
        })
        .await
        .expect("cancelled native flow must return a truthful outcome");
    }
}
