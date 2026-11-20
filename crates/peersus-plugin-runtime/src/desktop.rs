//! Shared desktop process bootstrap; only the entrypoint chooses a renderer.
use crate::{MAX_MODULE_BYTES, socket::SocketTransport};

fn execute(run: impl FnOnce(&[u8], &mut SocketTransport) -> bool) -> Result<(), ()> {
    use std::{fs::File, io::Read, path::PathBuf};
    let mut arguments = std::env::args_os().skip(1);
    let module = arguments.next().map(PathBuf::from).ok_or(())?;
    if arguments.next().is_some() {
        return Err(());
    }
    let socket = std::env::var_os("PEERSUS_PLUGIN_SOCKET")
        .map(PathBuf::from)
        .ok_or(())?;
    let data_socket = std::env::var_os("PEERSUS_PLUGIN_DATA_SOCKET")
        .map(PathBuf::from)
        .ok_or(())?;
    let token = std::env::var("PEERSUS_PLUGIN_TOKEN").map_err(|_| ())?;
    let input = File::open(module).map_err(|_| ())?;
    let mut bytes = Vec::new();
    input
        .take(MAX_MODULE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(());
    }
    let mut transport = SocketTransport::connect(&socket, data_socket, token).map_err(|_| ())?;
    run(&bytes, &mut transport).then_some(()).ok_or(())
}

pub fn run_main(run: impl FnOnce(&[u8], &mut SocketTransport) -> bool) {
    // The same reviewed, bounded categories as the owner's process. A failure
    // to start diagnostics must never keep a plugin module from running.
    let _diagnostics = peersus_diagnostics::init("plugin-worker").ok();
    if execute(run).is_ok() {
        return;
    }
    // Never print paths, bootstrap tokens, guest strings, or traps supplied by an extension.
    eprintln!("The plugin worker stopped.");
    std::process::exit(1);
}
