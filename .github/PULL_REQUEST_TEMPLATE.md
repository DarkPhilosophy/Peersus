## Problem and scope

Describe the user-visible problem and the evidence for its cause. Identify affected platforms and contracts.

## Change

Describe the behavior change and important tradeoffs. Separate implementation from unverified assumptions.

## Verification

List actual commands/results and runtime checks. State missing prerequisites or untested platforms explicitly. For UI changes, include narrow-layout and long-label evidence using non-sensitive data.

## Review checklist

- [ ] Related callers and API documentation match the change.
- [ ] Relevant behavior and failure-path checks ran.
- [ ] No credentials, private device data or generated build artifacts are included.
- [ ] Permission, cancellation and extension trust boundaries are preserved or explicitly explained.
- [ ] Performance claims include comparable measurements, if applicable.

Publication, signing and deployment are separate actions; this change description does not authorize them.
