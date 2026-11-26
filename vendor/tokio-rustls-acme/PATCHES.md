# Maintained time dependency

Baseline: tokio-rustls-acme 0.9.1, upstream `https://github.com/n0-computer/tokio-rustls-acme`, revision `0b7f8c8d194514b4d6a70d162e6d3dabda1faec1`. Original MIT and Apache-2.0 license files are retained. Imported from the published crates.io source on 2026-11-19.

Change: the certificate state (`src/state.rs`) reads a certificate's validity as `time::OffsetDateTime` (x509-parser's own `ASN1Time::to_datetime`) instead of converting it to `chrono`, and computes the renewal wait with `time::Duration`. `time` was already a dependency; `chrono` is removed from the manifests. The renewal moment is unchanged: one third of the validity before it ends, never in the past.

Purpose: remove the unmaintained `chrono` (RUSTSEC-2026-0543) without suppressing the advisory. Only the relay server used by `peersus-net`'s tests (`iroh-relay` with `server`) depends on this crate; the application does not ship it.

Removal: drop this patch when a released `tokio-rustls-acme` (or the `iroh-relay` that uses it) no longer depends on `chrono`. Run the `peersus-net` relay tests and the strict advisory audit after removal.
