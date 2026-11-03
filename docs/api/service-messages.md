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
