//! The program's identity where Windows and macOS read it from the program file (see `build-support/`).
#[path = "../../build-support/program_identity.rs"]
mod program_identity;

fn main() {
    program_identity::write(&program_identity::Program {
        binary: "peersus-store-fetch",
        description: "Peersus store fetch: the signed extension list and packages",
        helper_role: Some("store-fetch"),
    });
}
