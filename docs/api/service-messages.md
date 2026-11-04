# Plugin service messages

Implementation: `crates/peersus-plugin-api/src/message.rs` and `hub.rs`. These messages are independent of a specific GUI, transport backend or programming language. Process framing and bootstrap authentication belong to the host layer, not to this JSON schema.

## Participant operations

`Operation` is tagged by `op`, uses kebab-case variant names and rejects unknown fields. A participant may send:

| `op` | Fields | Meaning |
| --- | --- | --- |
| `call` | `id`, `provider`, `service`, `version`, `method`, `args`, `timeout_ms` | Call an exported method |
| `reply` | `ticket`, `result` | Answer a call delivered to this participant |
| `cancel` | `id` | Withdraw this participant's pending call |
| `subscribe` | `provider`, `service`, `version`, `event` | Subscribe to a declared event |
| `unsubscribe` | `provider`, `service`, `version`, `event` | Remove a subscription |
| `publish` | `service`, `version`, `event`, `data` | Publish an event this participant exports |

`id` and `ticket` are unsigned 64-bit integers; service versions and `timeout_ms` are unsigned 32-bit integers. The hub applies manifest, permission, lifecycle and timeout rules in addition to deserialization. Syntactically valid JSON is not necessarily an admitted call.

Example of withdrawing one's own call:

```json
{"op":"cancel","id":1}
```

A sender cannot add `caller` or `owner` to claim another identity: these fields are not accepted on operations.

## Hub deliveries

`Delivery` is tagged by `type`, also kebab-case with unknown fields rejected:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `call` | `ticket`, `caller`, `service`, `version`, `method`, `args` | A routed call; answer using this ticket |
| `result` | `id`, `result` | Outcome for the caller's own request ID |
| `cancelled` | `ticket` | The delivered call was cancelled or expired |
| `event` | `provider`, `service`, `version`, `event`, `data` | An admitted subscription event |

The hub stamps `caller` from the channel that submitted the request. The participant does not get to supply that identity. A ticket identifies the routed call at the hub, not the caller's local request ID.

## Outcomes

`Outcome` is tagged by `status`:

```json
{"status":"ok","value":null}
```

```json
{"status":"error","code":"example-error","message":"Synthetic example only"}
```

The example error code is illustrative, not an enumerated protocol error. Use the actual service's documented codes when handling a method result.

## Bounds and implementation guidance

One serialized control message is limited to **48 KiB** (`MAX_PAYLOAD`), including JSON overhead. The size check is on the serialized representation, not just the application value. Large data belongs on the host's separately admitted data path, not in oversized control messages.

Service versions, declared methods/events and permissions are validated by the shared hub. Keep concrete platform and plugin behavior behind these contracts; adding a service must not require recognizing a plugin display name in the application core. Cancellation is part of the lifecycle: an extension must not treat a cancelled or expired call as continuing permission to cause a late side effect.

## Android file-selection and HTTPS host services

Implementation: `crates/peersus-app/src/platform_services.rs` and `crates/peersus-ui-android/src/extension_services.rs`. These are application-owned services, not provider-specific upload logic. A manifest must import the exact service/version/methods, and the user must separately grant the host capability. Installing or starting the extension does not grant either capability.

| Provider / service / version | Methods | Host capability |
| --- | --- | --- |
| `host` / `peersus.files` / `1` | `choose`, `selected`, `release` | `files.select` |
| `host` / `peersus.http` / `1` | `configure`, `authorize`, `request`, `release` | `network.http` |

Arguments and successful `Outcome.value` objects:

| Method | Arguments | Successful value |
| --- | --- | --- |
| `peersus.files.choose` | Optional `max_bytes`: positive integer, at most 250 MiB | `file`: opaque handle; `name`, `bytes`, `sha256`: selected-file metadata |
| `peersus.files.selected` | `context`: nonzero host-issued action context; optional `max_bytes` as above | Same snapshot metadata as `choose`, using the existing Send selection without opening another picker |
| `peersus.files.release` | `file`: this caller's selection handle | `{"released":true}` |
| `peersus.http.configure` | `origin`: HTTPS origin, with the same restrictions as `authorize` | `{"configured":true}` only after a successful save or removal; not an upload authorization or credential-presence assertion |
| `peersus.http.authorize` | `file`: this caller's selection; `origin`: HTTPS origin with no user information, query, fragment or non-root path | `authorization`: opaque handle, only after explicit file/destination consent and credential entry or separately requested saved-credential use |
| `peersus.http.request` | `authorization`, `method`, `path`, and the bounded options below | For 2xx: `status` and parsed `json`; otherwise only `status` |
| `peersus.http.release` | `authorization`: this caller's authorization handle | `{"released":true}` |

File selection uses the Android document picker or the trusted Send selection and a private immutable snapshot, not ambient filesystem access or a path passed by the guest. Selected-action contexts are random, caller/generation scoped, expiring and single-use; revocation invalidates them. Redeeming a context does not consume the original Send selection. There are at most two live snapshots per caller and four globally. Releasing a file also invalidates its associated authorizations. Authorization binds one snapshot, caller generation and origin; credential bytes are not returned to the guest.

Configuration is separate from file selection and upload consent. Its native dialog can save or remove the origin-scoped encrypted API key and show whether a record exists without decrypting it. Successful writes close the dialog with explicit native confirmation; storage failures keep it open with an error. Cancel returns `cancelled`, never a successful save. Configuration creates neither a file snapshot nor an HTTPS authorization; each upload still needs explicit file/destination consent.

HTTPS request options:

- `method` is `GET`, `POST`, `PUT` or `DELETE`. Resolving `path` must stay within the approved HTTPS origin; user information and fragments are refused.
- `bearer: true` asks the host to add the approved credential as a sensitive bearer header. Guest-provided headers are limited to `content-type` and `x-upload-session`, at most eight entries.
- At most one body may be supplied: `json`, `part` or `multipart`. Serialized JSON is at most 64 KiB, still subject to the smaller overall control-message bound. File parts use `offset` and `length` within the approved snapshot and are bounded to 6 MiB.
- `multipart` also requires a `field` containing 1–64 ASCII letters, digits, `_` or `-`. Optional `fields` contains at most eight string values, each at most 1024 bytes, with keys at most 64 bytes. The requested file-field name is retained.
- Optional `capture_secrets` is an array of at most four JSON pointers, each at most 256 characters. The named response strings are kept in the host and replaced with `{"secret":"opaque-handle"}`. These authorization-scoped references can be used in subsequent JSON bodies or permitted headers; they cannot be borrowed by another authorization. Response capture is explicit, not a claim that arbitrary unknown provider fields are automatically classified as secrets.
- Optional `present_link` is a JSON pointer to a returned link, at most 256 characters. The selected URL must stay on the approved origin, contain no user information or fragment, and be at most 2048 characters. The host displays it in a selectable native result dialog; it does not automatically copy to the clipboard or send it to a peer.

The client uses Android platform certificate verification, no ambient proxy, no redirects and no automatic retry, with a 15-second connection timeout and a 120-second request timeout. Successful JSON responses are bounded to 32 KiB; known credential/session-secret echoes are refused. At most 64 captured secrets are retained per authorization, each bounded to 16 KiB.

Picker or consent cancellation returns the service error code `cancelled`; other adapter refusals use `platform-refused`. Hub-level admission failures still follow the common permission/lifecycle rules. Releasing or revoking an authorization does not undo a request already accepted by an external service: inspect any external result before retrying after an interruption.

Ordinary broker calls remain bounded to 30 seconds. The authenticated application can invoke an explicitly declared interactive action up to its declared bound of 600 seconds. Separately, only the live host can configure bounded interactive deadlines for its own exported picker/consent/HTTP methods; an extension cannot extend those deadlines through a wire operation or by changing its manifest.
