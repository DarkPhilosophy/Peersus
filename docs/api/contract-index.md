# Index of the extension contract

Every public item declared at the top of a module of `peersus-api`, the crate an extension and the application share, with the first sentence of what its own documentation says. It is a way in, not the reference: the item's documentation in the source, or `cargo doc -p peersus-api --no-deps --open`, is the contract.

The list below is generated from the source by `python3 scripts/gen-docs.py`; the gate refuses it when it is stale. Do not edit it by hand.

<!-- CONTRACT-START -->
### `agreement` ([source](../../crates/peersus-api/src/agreement.rs))

- const `MAX_PAGES`: Most pages in one agreement
- const `MAX_BLOCKS`: Most blocks on one page
- const `MAX_ITEMS`: Most list items, and most table rows, in one block
- const `MAX_COLUMNS`: Most columns of a table
- const `MAX_LINKS`: Most links in the list of one page
- const `MAX_TITLE`: Longest title of a page, in characters
- const `MAX_SUMMARY`: Longest `explains` or `changes`, in characters
- const `MAX_TEXT`: Longest text of one block, list item or link label, in characters
- const `MAX_CELL`: Longest text of one table cell, in characters
- const `MAX_URL`: Longest address of a link
- const `MAX_BYTES`: Largest agreement, as JSON bytes
- enum `Kind`: What a page is
- enum `Tone`: How strongly a callout speaks
- struct `Link`: One address to read
- enum `Block`: One piece of a page
- struct `Page`: One page of the agreement
- struct `Agreements`: The whole agreement of an extension
- fn `address`: An address a person is shown and may open

### `canvas` ([source](../../crates/peersus-api/src/canvas.rs))

- const `MAX_CANVAS_ID_BYTES`
- const `MAX_CANVAS_A11Y`
- const `MAX_CANVAS_DIMENSION`
- const `MAX_FRAME_BYTES`
- const `MAX_CANVAS_FPS`
- const `MAX_CANVAS_BYTES_PER_SECOND`
- const `MAX_CANVASES_PER_EXTENSION`
- const `MAX_CANVAS_BYTES_PER_EXTENSION`: Total frame storage retained by the owner, and separately by a window, for one extension
- const `MAX_CANVAS_SCALE`: Highest supported device-pixels/logical-pixel ratio, in thousandths
- const `SERVICE`: Canvas host service, version 1
- const `OPEN`
- const `CANVAS_DATA_KIND`: `data_open` kind for an application-issued canvas claim
- const `PRESENT_OK`
- const `PRESENT_INVALID`
- const `PRESENT_LIMITED`
- const `PRESENT_STREAM_FAILED`: Streaming failed; the helper closed this handle
- fn `valid_id`
- enum `CanvasRole`
- struct `CanvasRect`: Canvas-local logical coordinates in thousandths of a pixel; windows clip to the canvas
- struct `CanvasA11y`: One semantic overlay over the raster
- struct `CanvasPalette`: The frontend's theme tokens as straight sRGB RGBA bytes
- const `FRAME_MAGIC`
- const `FRAME_PREFIX_BYTES`: Read these bytes first, then use [`FrameHeader::encoded_len`] to read only the remaining header
- const `MAX_FRAME_HEADER_BYTES`
- const `RGBA8_PREMUL_SRGB`
- enum `FrameError`
- struct `FrameLayout`: A checked RGBA pixel layout
- struct `FrameHeader`: One `PSF1` header, followed on the wire by exactly `layout.byte_len()` bytes of RGBA8 premultiplied sRGB

### `chord` ([source](../../crates/peersus-api/src/chord.rs))

- const `MAX_CHORD`: Longest written chord (`"ctrl+alt+shift+super+escape"` is 28 bytes)
- const `MAX_FUNCTION_KEY`: Function keys every keyboard has
- struct `Modifiers`: The keys held with the chord's key
- enum `ChordKey`: The one key of a chord, by what is printed on it rather than by a code of some system
- struct `Chord`: Modifiers and one key

### `contributions` ([source](../../crates/peersus-api/src/contributions.rs))

- const `MAX_PAGES`
- const `MAX_ACTIONS`
- const `MAX_SETTINGS`
- const `MAX_CHOICES`
- const `MAX_LABEL`
- const `MAX_HELP`
- const `MAX_TEXT_SETTING`
- enum `Placement`: Where a page is offered
- enum `Scope`: What a setting or grant applies to
- struct `Page`: A page whose content is produced by an exported method, never by the frontend
- enum `Surface`: A frontend-neutral page renderer
- struct `InputCapabilities`: Input modes implemented by the receiving platform adapter
- const `MAX_POINTER_BATCH`: Largest batch of pointer input one call carries
- const `MAX_TYPED_TEXT`: Longest text one typed input carries, in characters
- enum `Button`: A mouse button, by its role rather than by a number of some system
- enum `Key`: A key that is not text, by its role
- enum `PointerInput`: Portable pointer or keyboard input, already interpreted by its sender
- struct `Source`: An exported service method addressed by data
- enum `ActionInput`: The trusted frontend input required by an action, independent of its renderer
- struct `Action`: A user-invocable command backed by one exported method
- const `MAX_ACTION_TIMEOUT_MS`
- fn `default_action_timeout`
- enum `Kind`: Typed value description
- struct `PublicRead`: One anonymous HTTPS origin and path subtree that the person can enable in Settings
- struct `Setting`
- struct `Input`: An extension that transports platform pointer input to a paired desktop
- struct `Contributions`: Everything one plugin adds to the product surface

### `elements` ([source](../../crates/peersus-api/src/elements.rs))

- const `MAX_NODES`: Most elements in one tree
- const `MAX_DEPTH`: Deepest nesting of rows and columns
- const `MAX_TEXT`: Longest text of one element, in characters
- const `MAX_FIELD`: Longest text a field holds or sends, in characters
- const `MAX_BYTES`: Largest tree, as JSON bytes
- const `MAX_SHAPES`: Most shapes of one drawing
- const `MAX_PATH`: Longest outline of all the shapes of one drawing together, in characters
- const `MAX_DEVICES`: Most devices one [`Node::Devices`] lists
- const `MAX_DEVICE_STATUSES`: Most statuses one device of a [`Node::Devices`] shows
- const `MAX_LAYER_SHAPES`: Most shapes of one kept layer ([`Keep`])
- const `MAX_LAYER_PATH`: Longest outline of all the shapes of one kept layer together, in characters
- const `MAX_LAYER_ID`: Longest id of a kept layer, in characters
- const `MAX_KEPT_LAYERS`: Most layers one extension keeps at once
- const `MAX_KEPT_PATH`: Longest outline of all the layers one extension keeps together, in characters
- const `MAX_FORGET`: Most ids one `forget` names
- const `MAX_PLACED`: Most kept layers one drawing places
- const `MAX_PLACED_SCALE`: Largest `scale` of a placed layer, in millionths
- const `MAX_PLACED_AT`: Farthest a placed layer's `at` may lie from the drawing's corner, in thousandths of a pixel, either way, on either axis
- const `PAGE_FEATURES_KEY`: The field of `peersus.config` `get` that lists the optional page features the application offers, by the names below
- const `FEATURE_PINCH`: A page feature
- const `FEATURE_LAYERS`: A page feature
- const `FEATURE_FOLLOW`: A page feature
- const `FEATURE_TEXT_SIZE`: A page feature
- const `FEATURE_ROTATE`: A page feature
- const `FEATURE_CANVAS`: A page feature
- const `PAGE_FEATURES`: Optional base features
- struct `Shape`: One shape of a [`Node::Drawing`]
- fn `layer_id`: Whether `id` names a kept layer
- struct `Placed`: A kept layer placed on a [`Node::Drawing`]
- struct `Keep`: The arguments of `peersus.drawings` `keep`
- struct `Forget`: The arguments of `peersus.drawings` `forget`
- enum `Tone`: How a text is set
- enum `Length`: A bounded layout measurement
- enum `Colour`: Semantic colours follow the person's theme without prescribing a widget or a use case
- struct `Follow`: Preview gestures on a clipped [`Node::Canvas`]'s most recent bitmap, without moving the viewport
- struct `Style`: Generic box layout, shared by every extension
- enum `PointerPhase`
- enum `Observe`: Additional raw observations a container requests
- enum `Presence`: How a device is reachable
- struct `Pick`: Which devices of a [`Node::Devices`] may be chosen
- struct `StatusMatch`: A status, as `peersus.status` names it
- struct `Show`: What each device of a [`Node::Devices`] shows beside its picture and name, which are always shown
- enum `Refusal`: Why a device of a [`Node::Devices`] cannot be chosen
- struct `DeviceEntry`: One device of a [`Node::Devices`], as the APPLICATION fills it in from what it knows
- enum `Node`: One element
- enum `Event`: What a person did on a page, sent to the page's source as `{"event"

### `error` ([source](../../crates/peersus-api/src/error.rs))

- enum `Error`
- type `Result`

### `language` ([source](../../crates/peersus-api/src/language.rs))

- const `MAX_TAG`: The longest language tag, in bytes
- const `MAX_FILE_BYTES`: The largest language file read, in bytes
- const `MAX_MESSAGES`: The most messages one file may hold
- const `MAX_TRANSLATION_CHARS`: The longest translation of one message, in characters
- const `MAX_NAME_CHARS`: The longest name a language gives itself, in characters
- const `MAX_FORMS`: The most plural forms a language may have
- struct `Language`: A language, by its tag
- struct `BadTag`: A language tag that is not one
- fn `placeholders`: The `{name}` placeholders of a text, in order, each once
- struct `PluralRule`: How a language picks the form of a word for a count
- struct `Catalog`: A language file, read
- struct `ParseError`: Why a file could not be read, and on which line (from 1)
- struct `Problem`: Where a translation does not fit the English text it translates
- fn `quote`: The PO string of `text`, quoted and escaped

### `manifest` ([source](../../crates/peersus-api/src/manifest.rs))

- const `MANIFEST_VERSION`: Version of the portable manifest protocol (not a Rust ABI)
- const `MAX_PARTICIPANTS`
- const `MAX_MEMBERS`
- const `MAX_DESCRIPTION`: Longest description, in characters, of a capability, an export or one of its members
- const `MAX_FILES`: Most files a manifest may list
- const `DEFAULT_PROGRAM`: What the program of a package is called when its manifest does not say
- const `MAX_PROGRAM_NAME`: Longest program file name
- const `EXECUTION_ABI_VERSION`: Version of the module host ABI shared by native-library and WASM executors
- enum `Execution`: The executor an extension author must explicitly select for every package
- struct `Export`: Named service export with an exact version
- struct `Member`: One method or event of an export
- struct `Import`: Methods and events a participant depends on, from one *named* provider at one exact version
- struct `Manifest`: What a plugin may ask of the user, by name, and what each name means

### `message` ([source](../../crates/peersus-api/src/message.rs))

- const `MAX_PAYLOAD`: Upper bound for one serialized control message
- enum `Operation`: What a participant asks the hub to do
- enum `Delivery`: What the hub delivers to a participant
- enum `Outcome`
- fn `bounded`: Serialized size check without keeping the encoded copy

### `name` ([source](../../crates/peersus-api/src/name.rs))

- const `MAX_NAME`
- fn `plain`: Whether every byte is one an identifier is written in
- fn `valid`: ASCII alphanumerics plus `.`, `_` and `-`; non-empty and bounded
- fn `text`: Human-readable text

### `notification` ([source](../../crates/peersus-api/src/notification.rs))

- const `MAX_TEXT`: Longest text of a notification, in characters
- const `MAX_LABEL`: Longest words on a button, in characters
- const `MAX_BUTTONS`: Most buttons on one notification
- enum `Reach`: How far a notification reaches
- struct `Button`: One answer the person may give
- struct `Notification`: What a sender asks the application to show
- enum `Answer`: What became of a notification that waited, as its sender is told

### `package` ([source](../../crates/peersus-api/src/package.rs))

- const `MAX_NAME_CHARS`: Longest display name, in characters
- const `MAX_DESCRIPTION_CHARS`: Longest description, in characters
- const `MAX_CHANGELOG_CHARS`: Longest changelog, in characters
- const `MAX_VERSION_BYTES`: Longest version text, in bytes
- struct `Version`: A version
- struct `Package`: What a plugin says about itself for people deciding whether to install or update it
- fn `public_link`: Links are display metadata, never a fetch instruction or an authority
- struct `Split`: The result of reading a manifest file that may carry package metadata
- enum `SplitError`: Why a file could not be split
- fn `split`: Take the `package` key out of a manifest file's JSON, leaving the rest as it was

### `permission` ([source](../../crates/peersus-api/src/permission.rs))

- struct `Permission`: Whose permission it is, and what it is called

### `platform` ([source](../../crates/peersus-api/src/platform.rs))

- enum `OperatingSystem`
- enum `Architecture`
- struct `HostPlatform`
<!-- CONTRACT-END -->
