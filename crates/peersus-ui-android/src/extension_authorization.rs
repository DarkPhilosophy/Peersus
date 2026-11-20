//! Encrypted storage adapter. User interaction belongs to the common Freya UI.
use peersus_core::plugins::PluginId;
use zeroize::Zeroizing;

pub async fn access(
    caller: PluginId,
    origin: reqwest::Url,
    operation: i32,
    secret: Zeroizing<Vec<u8>>,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    tokio::task::spawn_blocking(move || {
        crate::bridge::extension_credential_access(
            caller.as_str(),
            origin.as_str(),
            operation,
            secret.as_slice(),
        )
    })
    .await
    .map_err(|_| "The credential store is unavailable")?
}
