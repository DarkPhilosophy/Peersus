## What does this extension do?

Replace this guidance with the extension name and stable ID, the problem it solves, and what users can do after installing it. Start with its purpose, not CI or packaging.

## How is it used?

Describe a concrete example: where the action appears, what the user selects or configures, and what result they see. For updates, explain the behavior that changes.

## Platforms and execution

| Platform | Execution variant | Implemented behavior and limitations |
| --- | --- | --- |
| Specify a supported platform | Native process / WASM | Describe implemented behavior and limitations; do not imply unbuilt platforms work. |

All variants must share one stable ID. Link the extension README with installation, configuration and platform prerequisites.

## Permissions and data handling

Explain each requested capability: which files, clipboard content, devices or network destinations are accessible, what requires user approval, and where data is sent or stored. Identify external providers and public-link behavior. Never include credentials or private test data.

## Configuration and compatibility

List required programs, account or provider setup, default behavior, cancellation, and host/SDK requirements. For updates, explain migration and incompatible behavior.

## Verification and known limitations

Give a command or evidence link and result for each test you ran. List the exact desktop/Android/macOS devices or emulator, OS version and extension build exercised. Mark untested support or provider paths as **UNTESTED**; a package being built does not prove it runs on a device.

## Resource impact

State the expected startup, idle, timer and per-event cost and why it is bounded. Link the current-head validation run and copy its **PASS**, **REGRESSION**, or **INCONCLUSIVE** verdict. The report compares all package sizes and (using the exact same Linux harness and three runs per version) reviewed WASM startup/idle CPU, window main-thread time, helper CPU, RSS and PSS. It does not exercise native builds, permissioned actions, providers or device behavior.

| Metric / method | Baseline | After | Delta | Verdict |
| --- | --- | --- | --- | --- |
| | | | | |

Explain each **REVIEW** or **REJECT** row. An approved exception for **REJECT** requires a maintainer to add `impact:accepted` and rerun. An **INCONCLUSIVE** report never means pass; name what is missing. A first release has no published predecessor and can proceed only after all three submitted runs complete.

<!-- peersus-benchmark:start -->
<!-- CI replaces this section with measurements for the current head. Keep your own notes outside it. -->
<!-- peersus-benchmark:end -->

## Submission scope

Identify the single source directory, expected artifacts, ownership record and license. CI/build success is not independent maintainer approval or catalog publication.

- [ ] The manifest declares a licence-and-notice agreement (`agreements` with at least one licence page and one notice page) that passes `python3 scripts/check-extension-manifest.py <extension-folder>` against the trusted SDK; the same check refuses a `languages/<tag>.po` that is not that language and prints notes for words a translation leaves in English.
