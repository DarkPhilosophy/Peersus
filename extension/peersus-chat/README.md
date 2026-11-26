# Chat

Chat is an optional Peersus extension for conversations between paired devices. It has no message server or relay mailbox. History, attachments, and the outbox are stored only on the participating devices.

## Start a conversation

Open **Chat** from the extension page, or enable **Add Chat to navigation** in its settings. Choose a paired device for a direct conversation, or create a group with at least two other devices. Chat sends the same group ID and roster directly to each member. The roster is fixed; create a new group to change it. For everyone to reply to everyone, each pair of members needs to be paired and grant the extension access.

Your messages appear on the right; received messages appear on the left. Send text, attach a photo, audio file, or other file from the device picker, or react to a message with an emoji. Use **Save file…** to export a received attachment to a location you choose. An exported copy is no longer controlled by Chat.

## Delivery and privacy

- **Not sent · queued:** Chat keeps a copy on this device and retries when the recipient is reachable.
- **Sent:** The recipient's extension confirmed it saved the message. A network connection alone is not confirmation.
- **Seen:** The message was actually visible in the recipient's foreground conversation. An off-screen row or a background app does not send a seen receipt.
- **Delivery unconfirmed:** Chat cannot prove whether the recipient saved the message. In a group, each member has an independent delivery status.

An unsent message can be removed altogether, including from the outbox. Deleting a sent message leaves a **Message deleted** marker. You can delete only your copy, or also ask the recipient to remove theirs. **Clear here…** clears your history; **Clear here and ask others…** also sends requests for the history already shared. **Delete everything here…** removes the conversation and prunes pending deliveries and requests. It cannot recall a copy already saved by another device.

Receiving devices decide independently whether to honor remote message deletion and remote chat clearing. Both settings are off by default. Requests wait locally while a device is offline; if a recipient declines, Chat reports that their copy was kept. Removing the extension removes its local private storage, not other devices' data or files you exported.

## Build and packaging

The guest and its tests are in `wasm/`; **English** and **Română** are supported. `submission.toml` uses the Peersus source-build and packaging contract to build the WASM archives for Linux, Android, macOS, and Windows, and the optional Linux native-library and process archives. WASM is the store format. The native forms are for trusted local use and are not sandboxes.
