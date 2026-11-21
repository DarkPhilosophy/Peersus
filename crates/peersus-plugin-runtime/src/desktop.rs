//! Shared desktop process bootstrap; only the entrypoint chooses a renderer.
use crate::{MAX_MODULE_BYTES, RunError, socket::SocketTransport};
use peersus_plugin::supervisor::WorkerExit;

fn execute(
    run: impl FnOnce(&[u8], &mut SocketTransport) -> Result<(), RunError>,
) -> Result<(), WorkerExit> {
    use std::{fs::File, io::Read, path::PathBuf};
    let mut arguments = std::env::args_os().skip(1);
    let module = arguments
        .next()
        .map(PathBuf::from)
        .ok_or(WorkerExit::Start)?;
    if arguments.next().is_some() {
        return Err(WorkerExit::Start);
    }
    let socket = std::env::var_os("PEERSUS_PLUGIN_SOCKET")
        .map(PathBuf::from)
        .ok_or(WorkerExit::Start)?;
    let data_socket = std::env::var_os("PEERSUS_PLUGIN_DATA_SOCKET")
        .map(PathBuf::from)
        .ok_or(WorkerExit::Start)?;
    let token = std::env::var("PEERSUS_PLUGIN_TOKEN").map_err(|_| WorkerExit::Start)?;
    let input = File::open(module).map_err(|_| WorkerExit::Start)?;
    let mut bytes = Vec::new();
    input
        .take(MAX_MODULE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WorkerExit::Start)?;
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(WorkerExit::Start);
    }
    let mut transport =
        SocketTransport::connect(&socket, data_socket, token).map_err(|_| WorkerExit::Start)?;
    run(&bytes, &mut transport).map_err(|error| stop_reason(&error))
}

/// The closed category of why the guest stopped running, from the interpreter's trap code alone.
pub fn stop_reason(error: &RunError) -> WorkerExit {
    use wasmi::TrapCode;
    match error.as_trap_code() {
        Some(TrapCode::OutOfFuel) => WorkerExit::Fuel,
        Some(TrapCode::GrowthOperationLimited | TrapCode::OutOfSystemMemory) => WorkerExit::Memory,
        Some(_) => WorkerExit::Trap,
        None => WorkerExit::Host,
    }
}

pub fn run_main(run: impl FnOnce(&[u8], &mut SocketTransport) -> Result<(), RunError>) {
    // The same reviewed, bounded categories as the owner's process. A failure
    // to start diagnostics must never keep a plugin module from running.
    let _diagnostics = peersus_diagnostics::init("plugin-worker").ok();
    let Err(stopped) = execute(run) else {
        return;
    };
    // Never print paths, bootstrap tokens, guest strings, or traps supplied by an extension: the
    // owner reads the closed category from the exit code and writes it to its own log.
    eprintln!("The plugin worker stopped: {}.", stopped.word());
    std::process::exit(stopped.code());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trap_maps_to_its_closed_category_and_back_through_the_exit_code() {
        use wasmi::TrapCode;
        for (trap, expected) in [
            (TrapCode::OutOfFuel, WorkerExit::Fuel),
            (TrapCode::GrowthOperationLimited, WorkerExit::Memory),
            (TrapCode::UnreachableCodeReached, WorkerExit::Trap),
            (TrapCode::StackOverflow, WorkerExit::Trap),
        ] {
            let reason = stop_reason(&RunError::from(trap));
            assert_eq!(reason, expected);
            assert_eq!(WorkerExit::from_code(reason.code()), Some(expected));
        }
        assert_eq!(
            stop_reason(&RunError::new("host refused")),
            WorkerExit::Host
        );
        assert_eq!(WorkerExit::from_code(0), None);
    }
}
