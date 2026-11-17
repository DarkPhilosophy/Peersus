# Maintained token-pasting dependency

Baseline: pulp 0.22.3, https://github.com/sarah-quinones/pulp/, revision `5eb07fd7b68edf0a5e19f71737d315f72a510295`. Original source and license files are retained.

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

## AArch64 intrinsic feature coverage

The pinned Rust compiler diagnoses always-inlined intrinsic calls made without their required CPU features. AArch64 delegates now call a feature-annotated inner function, leaving x86 and Wasm delegates unchanged. The outer token remains the runtime feature proof; the inner call must not be replaced with a direct intrinsic call.

Feature grouping is checked against the pinned compiler's intrinsic documentation: baseline `vqrdmulh*` stays on `Neon`; `vqrdmlah*` and `vqrdmlsh*` require `Neon_Rdm`; polynomial 64-bit stores/reinterpretation use `Neon_Aes`; nightly rounding delegates use `Neon_Frintts` rather than `Neon_Sm4`. Token construction and CPU detection are unchanged. Optional-feature calls may retain a call boundary when the caller lacks those static features; no performance improvement is claimed.

Removal: use a compatible upstream release with equivalent target-feature coverage, then revalidate the actual AArch64 Android consumer and existing desktop/Wasm consumers. Do not suppress the compiler warning instead.
