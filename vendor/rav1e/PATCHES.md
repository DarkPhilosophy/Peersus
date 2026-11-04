# Maintained token-pasting dependency

Baseline: rav1e 0.8.1, https://github.com/xiph/rav1e/, revision `38806721f718e7ae6919e72385bc305bf824ef81`. Original source and license files are retained.

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.
