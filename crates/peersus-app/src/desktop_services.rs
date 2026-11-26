//! Desktop platform UI, composed with the same file/HTTP authority used on Android.
use crate::{
    desktop_clipboard::DesktopClipboard,
    desktop_input::DesktopInput,
    platform_services::{
        self, Call, PlatformServices, Response, SelectedInput,
        file_http::{CredentialResponse, FileHttpServices, Interaction, UiResponse},
    },
};
use peersus_core::plugins::PluginId;
use std::{path::PathBuf, sync::Arc};
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
mod linux_credentials;

pub struct DesktopServices {
    clipboard: Arc<DesktopClipboard>,
    input: DesktopInput,
    files: FileHttpServices,
    prompts: Arc<crate::extension_prompts::ExtensionPrompts>,
    notifications: Arc<crate::notifications::Notifications>,
    dialogs: Arc<tokio::sync::Semaphore>,
}
impl DesktopServices {
    pub fn new(scratch: PathBuf, data: PathBuf) -> Arc<Self> {
        let clipboard = DesktopClipboard::new();
        let prompts = Arc::new(crate::extension_prompts::ExtensionPrompts::default());
        let notifications = Arc::new(crate::notifications::Notifications::default());
        let dialogs = Arc::new(tokio::sync::Semaphore::new(1));
        Arc::new(Self {
            files: FileHttpServices::new(
                scratch,
                Arc::new(DesktopInteraction {
                    clipboard: clipboard.clone(),
                    prompts: prompts.clone(),
                    dialogs: dialogs.clone(),
                    #[cfg(target_os = "linux")]
                    credential_lookups: Arc::new(tokio::sync::Semaphore::new(
                        linux_credentials::LOOKUP_WORKERS,
                    )),
                }),
                notifications.clone(),
            )
            .with_public_read_approvals(data),
            clipboard,
            input: DesktopInput::system(),
            prompts,
            notifications,
            dialogs,
        })
    }
}
impl PlatformServices for DesktopServices {
    fn public_read_directory(&self) -> Option<&std::path::Path> {
        self.files.public_read_directory()
    }
    fn extension_prompts(&self) -> Option<Arc<crate::extension_prompts::ExtensionPrompts>> {
        Some(self.prompts.clone())
    }
    fn notifications(&self) -> Option<Arc<crate::notifications::Notifications>> {
        Some(self.notifications.clone())
    }
    fn action_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<platform_services::ActionProgress>> {
        self.files.action_progress()
    }
    fn select_for_action(
        &self,
        caller: &PluginId,
        input: SelectedInput,
        action: &str,
    ) -> Result<u64, &'static str> {
        self.files.select_for_action(caller, input, action)
    }
    fn file_snapshot(
        &self,
        caller: &PluginId,
        file: &str,
    ) -> Option<Arc<platform_services::Snapshot>> {
        self.files.file_snapshot(caller, file)
    }
    fn save_extension_file(&self, caller: PluginId, name: String) -> UiResponse<std::fs::File> {
        let dialogs = Arc::clone(&self.dialogs);
        Box::pin(async move {
            let _permit = dialogs
                .try_acquire_owned()
                .map_err(|_| "The file picker is busy")?;
            let selected = rfd::AsyncFileDialog::new()
                .set_title(format!("Save one file from {}", caller.as_str()))
                .set_file_name(name)
                .save_file()
                .await
                .ok_or("Saving was cancelled")?;
            let path = selected.path().to_path_buf();
            tokio::task::spawn_blocking(move || crate::private_fs::new_file(0o600).open(path))
                .await
                .map_err(|_| "Could not open the selected destination")?
                .map_err(|_| "Could not create a new file at the selected destination")
        })
    }
    fn dispatch(&self, call: Call) -> Response {
        if call.service == platform_services::CLIPBOARD_SERVICE {
            self.clipboard.dispatch(call)
        } else if call.service == platform_services::INPUT_SERVICE {
            self.input.dispatch(call)
        } else if call.service == platform_services::location::SERVICE {
            Box::pin(async move {
                platform_services::location::answer(&call.method, platform_services::location::find)
                    .await
            })
        } else {
            self.files.dispatch(call)
        }
    }
    fn dispatch_input(&self, call: Call, lease: Option<platform_services::InputLease>) -> Response {
        self.input.dispatch_leased(call, lease)
    }
    fn release_peer_input(&self, caller: &PluginId, owner: &str) {
        self.input.release_peer(caller, owner);
    }
    fn select(&self, caller: &PluginId, input: SelectedInput) -> Result<u64, &'static str> {
        match input {
            SelectedInput::Text(_) => self.clipboard.select(caller, input),
            _ => self.files.select(caller, input),
        }
    }
    fn discard_selection(&self, caller: &PluginId, context: u64) {
        self.clipboard.discard_selection(caller, context);
        self.files.discard_selection(caller, context);
    }
    fn revoke(&self, caller: &PluginId) {
        self.prompts.revoke(caller);
        self.clipboard.revoke(caller);
        self.files.revoke(caller);
        self.input.revoke(caller);
    }
    fn revoke_permission(
        &self,
        caller: &PluginId,
        permission: &peersus_api::permission::Permission,
    ) {
        use crate::platform_services::*;
        match permission.name.as_str() {
            INPUT_CAPABILITY => self.input.revoke(caller),
            CLIPBOARD_READ_CAPABILITY | CLIPBOARD_WRITE_CAPABILITY | CLIPBOARD_WATCH_CAPABILITY => {
                self.clipboard.revoke(caller)
            }
            FILES_CAPABILITY | HTTP_CAPABILITY | PUBLIC_HTTP_CAPABILITY => {
                self.prompts.revoke(caller);
                self.files.revoke(caller);
            }
            _ => {}
        }
    }
    fn changes(&self, service: &str) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.clipboard.changes(service)
    }
}

struct DesktopInteraction {
    clipboard: Arc<DesktopClipboard>,
    prompts: Arc<crate::extension_prompts::ExtensionPrompts>,
    dialogs: Arc<tokio::sync::Semaphore>,
    #[cfg(target_os = "linux")]
    credential_lookups: Arc<tokio::sync::Semaphore>,
}
impl DesktopInteraction {
    fn permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, &'static str> {
        self.dialogs
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Another extension dialog is already open")
    }
}

fn entry(caller: &PluginId, origin: &reqwest::Url) -> Result<keyring::Entry, &'static str> {
    // A new, generic scope. Never borrow another extension's or the native plugin's key.
    keyring::Entry::new(
        "dev.peersus.platform-http",
        &format!("{} {}", caller.as_str(), origin.as_str()),
    )
    .map_err(|_| "The encrypted credential store is unavailable")
}
/// Read and validate the key for a check or a keyed authorization.
async fn saved_key(
    caller: PluginId,
    origin: reqwest::Url,
) -> Result<Option<Zeroizing<Vec<u8>>>, &'static str> {
    tokio::task::spawn_blocking(move || {
        let key = match entry(&caller, &origin)?.get_password() {
            Ok(key) => key,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(_) => return Err("The encrypted credential store is unavailable"),
        };
        // The rule a key is saved under is the rule it is read back under: one type holds it, and wipes what it refuses.
        let key = peersus_core::model::ExtensionSecret::try_from(key)
            .map_err(|_| "The stored API key is invalid; configure it again")?;
        Ok(Some(Zeroizing::new(key.expose().as_bytes().to_vec())))
    })
    .await
    .map_err(|_| "The credential store is unavailable")?
}
impl Interaction for DesktopInteraction {
    fn choose(
        &self,
        caller: PluginId,
        kind: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)> {
        let permit = self.permit();
        Box::pin(async move {
            let _permit = permit?;
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title(format!("Select one file for {}", caller.as_str()));
            if let Some((name, extensions)) = kind.extensions() {
                dialog = dialog.add_filter(name, extensions);
            }
            let file = dialog.pick_file().await.ok_or("Selection was cancelled")?;
            let path = file.path().to_owned();
            let name = file.file_name();
            let source = tokio::task::spawn_blocking(move || std::fs::File::open(path))
                .await
                .map_err(|_| "Selected file is unavailable")?
                .map_err(|_| "Selected file is unavailable")?;
            Ok((source, name))
        })
    }
    fn configure(&self, caller: PluginId, origin: reqwest::Url) -> UiResponse<()> {
        let prompts = self.prompts.clone();
        Box::pin(async move {
            let answer = prompts.ask(caller.clone(), origin.to_string()).await?;
            if !answer.accepted {
                return Err("Authorization was cancelled");
            }
            if answer.remove {
                return tokio::task::spawn_blocking(move || {
                    match entry(&caller, &origin)?.delete_credential() {
                        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                        Err(_) => Err("Could not remove the saved API key"),
                    }
                })
                .await
                .map_err(|_| "The credential store is unavailable")?;
            }
            let key = answer.secret.ok_or("Enter an API key")?;
            tokio::task::spawn_blocking(move || {
                let key = key.expose();
                entry(&caller, &origin)?
                    .set_password(key)
                    .map_err(|_| "Could not save the API key in the encrypted credential store")
            })
            .await
            .map_err(|_| "The credential store is unavailable")?
        })
    }
    fn credential(&self, caller: PluginId, origin: reqwest::Url) -> CredentialResponse {
        Box::pin(saved_key(caller, origin))
    }
    #[cfg(target_os = "linux")]
    fn credential_saved(&self, caller: PluginId, origin: reqwest::Url) -> UiResponse<bool> {
        Box::pin(linux_credentials::lookup(
            self.credential_lookups.clone(),
            move |deadline| linux_credentials::present(&entry(&caller, &origin)?, deadline),
        ))
    }
    fn copy(&self, link: String) -> UiResponse<()> {
        let clipboard = self.clipboard.clone();
        Box::pin(async move { clipboard.copy_result(link).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::message::Outcome;
    use serde_json::{Value, json};

    async fn invoke(host: &DesktopServices, caller: &str, service: &str, context: u64) -> Outcome {
        host.dispatch(Call {
            caller: PluginId::new(caller).unwrap(),
            service: service.into(),
            method: "selected".into(),
            args: json!({"context": context}),
        })
        .await
    }

    #[tokio::test]
    async fn file_and_text_contexts_are_distinct_and_wrong_facility_cannot_consume_them() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("synthetic.txt");
        std::fs::write(&source, b"selected file").unwrap();
        let host = DesktopServices::new(scratch.path().into(), scratch.path().into());
        let caller = PluginId::new("synthetic").unwrap();
        let text = host
            .select(&caller, SelectedInput::Text("selected text".into()))
            .unwrap();
        let file = host.select(&caller, SelectedInput::Path(source)).unwrap();
        assert_ne!(text, file);
        for (service, context) in [
            (platform_services::FILES_SERVICE, text),
            (platform_services::CLIPBOARD_SERVICE, file),
        ] {
            assert!(matches!(
                invoke(&host, "synthetic", service, context).await,
                Outcome::Error { .. }
            ));
        }
        assert!(matches!(
            invoke(&host, "other", platform_services::FILES_SERVICE, file).await,
            Outcome::Error { .. }
        ));
        let Outcome::Ok { value } = invoke(
            &host,
            "synthetic",
            platform_services::CLIPBOARD_SERVICE,
            text,
        )
        .await
        else {
            panic!("wrong-facility redemption consumed text");
        };
        assert_eq!(value["bytes"], json!("selected text".len()));
        let Outcome::Ok { value } =
            invoke(&host, "synthetic", platform_services::FILES_SERVICE, file).await
        else {
            panic!("wrong-facility redemption consumed file");
        };
        assert_eq!(value["bytes"], json!(b"selected file".len()));
        assert_eq!(value["name"], Value::String("synthetic.txt".into()));
        for (service, context) in [
            (platform_services::CLIPBOARD_SERVICE, text),
            (platform_services::FILES_SERVICE, file),
        ] {
            assert!(matches!(
                invoke(&host, "synthetic", service, context).await,
                Outcome::Error { .. }
            ));
        }
        host.revoke(&caller);
    }

    #[tokio::test]
    async fn discard_and_revoke_cover_both_facilities_without_touching_other_callers() {
        let scratch = tempfile::tempdir().unwrap();
        let host = DesktopServices::new(scratch.path().into(), scratch.path().into());
        let caller = PluginId::new("synthetic").unwrap();
        let other = PluginId::new("other").unwrap();
        for input in [
            SelectedInput::Text("selected text".into()),
            SelectedInput::Path(scratch.path().join("never-opened")),
        ] {
            let context = host.select(&caller, input).unwrap();
            host.discard_selection(&other, context);
            host.discard_selection(&caller, context);
            for service in [
                platform_services::FILES_SERVICE,
                platform_services::CLIPBOARD_SERVICE,
            ] {
                assert!(matches!(
                    invoke(&host, "synthetic", service, context).await,
                    Outcome::Error { .. }
                ));
            }
        }
        let text = host
            .select(&caller, SelectedInput::Text("one".into()))
            .unwrap();
        let file = host
            .select(
                &caller,
                SelectedInput::Path(scratch.path().join("never-opened")),
            )
            .unwrap();
        let kept = host
            .select(&other, SelectedInput::Text("two".into()))
            .unwrap();
        host.revoke(&caller);
        for (service, context) in [
            (platform_services::FILES_SERVICE, file),
            (platform_services::CLIPBOARD_SERVICE, text),
        ] {
            assert!(matches!(
                invoke(&host, "synthetic", service, context).await,
                Outcome::Error { .. }
            ));
        }
        assert!(matches!(
            invoke(&host, "other", platform_services::CLIPBOARD_SERVICE, kept).await,
            Outcome::Ok { .. }
        ));
        host.revoke(&other);
    }
}
