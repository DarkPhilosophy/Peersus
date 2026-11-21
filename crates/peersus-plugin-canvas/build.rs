#[path = "../../build-support/program_identity.rs"]
mod program_identity;

fn main() {
    program_identity::write(&program_identity::Program {
        binary: "peersus-wasm-canvas-host",
        description: "Peersus canvas host: runs one WebAssembly extension with a renderer",
        helper_role: Some("wasm-canvas-host"),
    });
}
