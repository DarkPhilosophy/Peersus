# Security

## Development status

This project is in active development. There is no published supported-version matrix or guarantee of security backports. Use the current development revision when reproducing a report, and state the exact revision and build flavor. Do not use a development build as the only copy of important data.

## Report a vulnerability privately

Use [Security → Report a vulnerability](https://github.com/axxa-dev/Peersus/security/advisories/new) to submit a private report. If that option is unavailable, request a private reporting channel without disclosing details or private data in a public issue.

A useful report includes affected revision/platform, prerequisites, a minimal reproduction, expected and observed behavior, and impact. Use synthetic data. Never attach credentials, pairing invitations, relay tokens, device identity material, private filenames, file contents or unredacted logs. Do not test against another person's devices or a production service without authorization.

## Trust boundaries

- Pairing and encrypted transport do not make the remote device trustworthy. Verify the intended device and review file offers and grants.
- Local IPC is same-user control, not a remotely exposed API. A compromised account can control its own local application.
- Plugin declarations describe requested capability; they are not grants. Global and per-device permission checks remain the owner's responsibility.
- A catalog signature proves approval of particular bytes, not that an extension is harmless. Native execution and its actual OS isolation must be assessed separately from provenance.
- Unsigned or rollback catalogs must not be silently trusted. The application fails closed when store trust is not configured.
- An extension UI label, archive filename, metadata author or website listing is untrusted input, not execution authority.
- Debug packages and local unsigned artifacts are not verified public releases. Android Play and community builds have different native-extension policies.

## Sensitive material

Relay access tokens belong in the dedicated secure-storage flow, not preferences, command-line arguments, issues or screenshots. Signing seeds belong outside the checkout in protected maintainer storage; a public verification key is not a signing key. Do not add production secrets to fixtures, documentation or CI logs.

## Changes that need special scrutiny

Review archive extraction, symlinks, executable/helper resolution, process bootstrap identity, native ABI handling, grants, late/cancelled operations, transfer destination handling and catalog rollback protection together with their callers and regression tests. Never weaken a security check merely to make an installation, test or catalog build succeed.
