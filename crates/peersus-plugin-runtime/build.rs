//! The program's identity where Windows and macOS read it from the program file (see `build-support/`).
#[path = "../../build-support/program_identity.rs"]
mod program_identity;

fn main() {
    program_identity::write(&program_identity::Program {
        binary: "peersus-wasm-host",
        description: "Peersus extension host: runs one WebAssembly extension",
        helper_role: Some("wasm-host"),
    });
}
