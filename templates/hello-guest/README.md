# Hello guest: a starting point for an extension

The smallest extension that does something: it answers one call, `hello.greeting` `greet`, and nothing else. It asks the
application for nothing, so installing it grants nothing.

## What is here

| File | What it is |
| --- | --- |
| `plugin.json` | What the extension is and what it offers or asks for. The application shows this before anyone installs it. |
| `src/lib.rs` | The guest. It exchanges JSON messages with the application and has no other door. |
| `Cargo.toml` | A standalone crate that builds to WebAssembly. Keep `[workspace]` and the release profile. |

## Build it

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
```

The module is `target/wasm32-unknown-unknown/release/hello_guest.wasm`, the file `plugin.json` names as `program`.

## Make it yours

1. Change the names in `Cargo.toml` and `plugin.json`, and `program` to match the new file name.
2. Add methods to `exports` in `plugin.json` and a branch for each in `src/lib.rs`.
3. Declare in `imports` everything you want from the application, such as `peersus.config` or `peersus.peers`. Anything
   not declared there is refused, and anything that needs the person's consent asks for it at install time.
4. Leave `platforms` out unless your extension really needs one system. Without it the extension installs everywhere; with
   it, installing on any other system is refused. A guest that only exchanges messages needs no restriction.

## What a guest can and cannot do

It cannot open files, sockets or windows, read the clock, or call the system. It can answer calls, ask the application
for the services it declared, and move bytes only through connections the application issues. Messages are at most 48 KiB.
Reading this crate's `receive` and `send` loop is the whole contract.

The full message formats are in `docs/api/service-messages.md`, the manifest in `docs/api/manifests.md` and the binary
interface in `docs/api/module-abi.md`.

## Check it

`cargo xtask test` builds this template and runs it through the same interpreter the application uses: it must start,
answer `greet`, refuse an unknown method, and say nothing before it is called. A template that stops working is a
broken promise to every author who starts from it.
