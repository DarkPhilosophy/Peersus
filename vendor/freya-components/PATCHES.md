# Maintained token-pasting dependency

Baseline: freya-components 0.5.0-rc.13, https://github.com/marc2332/freya, revision `f796f8ff46f43864aa0258a9187fd73990a8a8c8`. Original source and license files are retained.

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

# Input role and re-press hook

Local change in `src/input.rs`: an optional explicit accessibility role (`Input::role`, default unchanged), so a field can say it is a search, address or secret field independently of masking; `InputPressedAgain`, an optional global-context callback run when an already focused input is pressed again (the application's Android adapter provides it to reopen a soft keyboard the person dismissed while the field kept focus; without it nothing changes); and an edit menu opened by holding a press on any input (Select all, Copy, Cut, Paste; a masked input offers only Select all and Paste). Each menu entry runs the same edit as its keyboard shortcut through the input's single edit path, so validation and masking rules are identical. Labels default to English; `InputEditLabels` in the global contexts translates them.

Removal: drop when upstream exposes an input role and a re-press or keyboard-request hook.
