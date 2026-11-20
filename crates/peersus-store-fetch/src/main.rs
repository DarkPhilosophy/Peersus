//! `peersus-store-fetch <file-name> <limit-bytes> <output-path>`
//!
//! One bounded HTTPS GET from the store's fixed base, written to a NEW private file. The caller (the
//! application owner) must treat the bytes as untrusted and verify them itself: this program proves
//! nothing about them. It prints only a fixed word on failure, never anything it received.
#![forbid(unsafe_code)]
use peersus_store_fetch::Store;
use std::{fs::OpenOptions, io::Write, process::ExitCode};

#[cfg(feature = "private-store")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateSource {
    repository: String,
    directory: String,
    revision: String,
    token: String,
}

#[cfg(feature = "private-store")]
impl Drop for PrivateSource {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.token.zeroize();
    }
}

fn source(private: bool) -> Result<Store, &'static str> {
    if !private {
        return Store::production().map_err(|_| "address");
    }
    #[cfg(feature = "private-store")]
    {
        use std::io::Read;
        // Only an anonymous stdin pipe carries credentials; never argv, environment
        // variables or a temporary plaintext file. Bound input before parsing it.
        let mut input = zeroize::Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(16385)
            .read_to_end(&mut input)
            .map_err(|_| "configuration")?;
        if input.len() > 16384 {
            return Err("configuration");
        }
        let configuration: PrivateSource =
            serde_json::from_slice(&input).map_err(|_| "configuration")?;
        Store::github_private(
            &configuration.repository,
            &configuration.directory,
            &configuration.revision,
            &configuration.token,
        )
        .map_err(|_| "configuration")
    }
    #[cfg(not(feature = "private-store"))]
    Err("configuration")
}
fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let private = args
        .first()
        .is_some_and(|argument| argument == "--private-github");
    if private {
        args.remove(0);
    }
    let [name, limit, output] = args.as_slice() else {
        eprintln!("usage: peersus-store-fetch <file-name> <limit-bytes> <output-path>");
        return ExitCode::from(2);
    };
    let Ok(limit) = limit.parse::<usize>() else {
        return ExitCode::from(2);
    };
    // The ceiling is the application's package bound; a larger request is a bug, not a wish.
    if limit == 0 || limit > 128 * 1024 * 1024 {
        return ExitCode::from(2);
    }
    let store = match source(private) {
        Ok(store) => store,
        Err(word) => {
            eprintln!("{word}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::FAILURE,
    };
    let result = runtime.block_on(async {
        store.get(name, limit).await.map_err(|error| match error {
            peersus_store_fetch::Error::Name => "name",
            peersus_store_fetch::Error::Address => "address",
            peersus_store_fetch::Error::Network => "network",
            peersus_store_fetch::Error::Status(_) => "status",
            peersus_store_fetch::Error::TooLarge => "too-large",
        })
    });
    let body = match result {
        Ok(body) => body,
        Err(word) => {
            eprintln!("{word}");
            return ExitCode::FAILURE;
        }
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options
        .open(output)
        .and_then(|mut file| file.write_all(&body))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("write");
            ExitCode::FAILURE
        }
    }
}
