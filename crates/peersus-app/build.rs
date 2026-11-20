//! The program's identity where Windows and macOS read it from the program file (see `build-support/`).
#[path = "../../build-support/program_identity.rs"]
mod program_identity;

fn main() {
    program_identity::write(&program_identity::Program {
        binary: "peersus",
        description: "Peersus core: devices, transfers, settings and extensions",
        helper_role: Some("core"),
    });
}
