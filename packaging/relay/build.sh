#!/usr/bin/env bash
# Build the project relay: iroh-relay at the version the application uses, with one patch that serves the website from a
# folder (iroh-relay-<version>-peersus-site.patch). Built in a Debian container so it runs on the server's older glibc.
# Builds only: nothing is copied to a server, nothing is restarted. Prints the path of the built program.
set -euo pipefail
here="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(CDPATH= cd -- "$here/../.." && pwd)"
version="$(awk '/^name = "iroh-relay"$/{getline; gsub(/version = |"/, ""); print; exit}' "$root/Cargo.lock")"
patch="$here/iroh-relay-$version-peersus-site.patch"
[[ -f "$patch" ]] || { echo "No patch for iroh-relay $version: $patch" >&2; exit 1; }
work="$root/target/relay-build/iroh-relay-$version"
rm -rf -- "$work"
mkdir -p -- "$(dirname -- "$work")"
# The crate exactly as published, fetched into a throwaway folder by Cargo itself.
fetch="$root/target/relay-build/fetch"
rm -rf -- "$fetch" && mkdir -p -- "$fetch/src" && : > "$fetch/src/lib.rs"
printf '[package]\nname = "fetch"\nversion = "0.0.0"\nedition = "2024"\n[workspace]\n[dependencies]\niroh-relay = "=%s"\n' "$version" > "$fetch/Cargo.toml"
cargo fetch --manifest-path "$fetch/Cargo.toml" >/dev/null
source="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "iroh-relay-$version" | head -n 1)"
[[ -n "$source" ]] || { echo "iroh-relay $version was not fetched" >&2; exit 1; }
cp -r -- "$source" "$work"
chmod -R u+w -- "$work"
patch -d "$work" -p1 --quiet < "$patch"
engine="$(command -v podman || command -v docker)"
"$engine" run --rm -v "$work:/src:Z" -v peersus-relay-cargo:/usr/local/cargo/registry -w /src docker.io/library/rust:1-bookworm \
    cargo build --release --features server --bin iroh-relay >&2
echo "$work/target/release/iroh-relay"
