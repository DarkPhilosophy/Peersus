# Local application control

Implementation: `crates/peersus-app/src/ipc.rs`. This is the local frontend-to-owner protocol, **not** the plugin protocol and not a public network listener.

## Connection and identity

The launcher creates and owns the local socket and its cleanup. On Unix, both accepting and connecting sides check the socket peer UID against the effective UID. Same-user authorization means another process running as that user is within the trust boundary; the socket is not protection against a compromised user account.

## Framing

Each frame is a four-byte unsigned **big-endian** byte length followed by that many UTF-8 JSON bytes. Zero-length and oversized frames are refused before allocating the payload. Requests are bounded by `REQUEST_LIMIT` in the implementation, derived from the maximum profile picture plus command overhead. Replies are bounded to 4 MiB.

The request discriminator is `operation`. Its values are Rust variant names, with their case preserved:

```json
{"operation":"Snapshot"}
```

```json
{"operation":"Lease"}
```

A command request is `{"operation":"Command","command":...}`. The command's shape comes from `peersus_core::model::Command`; do not infer it from the CLI spelling. Unknown request fields are rejected. Boxing the command in Rust does not add a wrapper to its JSON representation.

The response has `result` and `snapshot` fields. `result` is the serde representation of `Result<Option<Reply>, String>`; snapshot and lease requests return `Ok(null)`, while a command returns a reply or an error. A response example showing only the result portion is:

```json
{"Ok":null}
```

This is **not** a complete response: a real response also includes the complete `snapshot` object.

## Leases, concurrency and deadlines

A lease keeps a window attached while its connection remains open. It receives its initial response, then does not require periodic client messages. EOF, connection failure or owner shutdown releases it.

The implementation allows 16 ordinary concurrent requests and up to 8 leases. Lease handling adds room for ordinary requests rather than permanently consuming their capacity. Request reading and response writing have 15-second deadlines. The client wraps the exchange in a 15-second deadline; that deadline does not prove that a submitted command did not execute.

**Timeout or disconnect after sending a command means the outcome may be unknown.** Query current state before deciding whether a retry is safe. Do not turn transport retries into duplicate installations, transfers or settings changes.

## Preferred client interface

Rust consumers should use the existing IPC client and `cli::Control` adapter instead of implementing a second framing stack. In-process consumers submit the same `Command` values through `runtime::Client` and observe application snapshots. Neither path bypasses application validation.

## Relay policy settings

The shared Network editor submits `patch-settings` with the existing `SettingsPatch.relay_policy` field. Direct connection priority is not a reorderable row. A complete local request, using an illustrative project address, is:

```json
{
  "operation": "Command",
  "command": {
    "command": "patch-settings",
    "patch": {
      "bind": {"mode": "automatic"},
      "relay_policy": {
        "targets": [
          {"kind": "project", "url": "https://relay.example.org"},
          {"kind": "public"}
        ],
        "disabled": [],
        "direct_wait_ms": 3000,
        "failover_wait_ms": 5000,
        "retry_primary_ms": 60000
      }
    }
  }
}
```

Check `BindChange` in `peersus-core/src/config.rs` when implementing a client; enum shapes are part of the typed contract. Target kinds are `project`, `custom` (both with `url`) and `public` (the n0 pool, without a URL). List order is priority. An empty list disables relays; at most eight targets, one custom credential-bearing target, one public pool, and no duplicate URLs are accepted. Addresses must pass the existing HTTPS relay URL validator; credentials belong in the separate relay-token command, never in a URL or settings.

The optional `disabled` array contains zero-based positions in `targets`, for example `"disabled": [1]` retains the second fallback but skips it. Missing or empty means every listed target is enabled. Disabled entries retain their addresses and priority; they still count toward the size and duplicate limits, and out-of-range positions are rejected. When reordering, move the disabled flag with its target. An empty list or all targets disabled means direct-only routing. Diagnostics refer to the original configured positions, not a compressed list of enabled targets.

Clients sending `disabled` require an owner that understands this field. Upgrade and restart an older desktop owner before using the new editor; building or replacing the UI executable does not upgrade an already-running owner. Older clients may omit `disabled` and retain their previous all-enabled list semantics with the new owner.

Timing ranges are 100–30,000 ms for the initial direct-path opportunity, 500–60,000 ms for tier failure, and 5,000–3,600,000 ms for retrying the preferred tier. These are deadlines, not guarantees of connectivity. A preferred-tier retry does not preempt an active fallback transfer. The public pool may select a server within that pool; the ordering applies to tiers rather than individual n0 servers.

New settings use the project-then-public default. Old settings without `relay_policy` retain their prior public/custom/direct-only selection; they do not gain a project relay during migration. Legacy `relay` patches remain accepted for existing clients and explicitly select their legacy mode. Do not combine legacy `relay` and `relay_policy` fields in a new client. The GUI submits the complete ordered policy and uses automatic binding when enabling relays, because a fixed local bind and relay routing are incompatible.

The former project default is migrated to the current project relay address; user-edited project addresses are preserved. Turning all fallbacks off keeps their saved order and addresses, so enabling one later restores it in place.

`snapshot.settings.network.relay_policy` describes configured policy, not proof that a server is reachable. `snapshot.relay` separately reports sanitized live home-relay diagnostics: `{"active_target":1,"health":"connected"}` means zero-based fallback position 1 is connected. Health values are `disabled`, `connecting`, `connected`, `unavailable` and `refused`; disabled has a null target. An absent/null diagnostic is an older owner, not evidence that relays are disabled. Health changes publish snapshots even when the active tier does not change. The shared Network editor shows the state and one-based fallback position. No raw server errors, addresses or credentials are added to diagnostics.

Home-relay health is not an individual peer's selected traffic path. See [the transport limitations](../architecture.md#transfers-and-relay-configuration): relay signalling and encrypted fallback traffic can expose connection metadata, and home-relay ordering is not a peer-relay firewall.
