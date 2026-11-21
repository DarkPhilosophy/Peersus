//! Mutual human consent bound to an already authenticated transport session.
//!
//! The caller MUST authenticate the invitation (for example a QR-pinned public
//! key and fresh capability) before constructing this ceremony. Three numeric
//! choices are a consent mechanism, not sufficient standalone MITM protection.
//! Both sides commit before either reveals; all proofs bind the TLS exporter,
//! ordered identities and invitation nonce. No transfer API grants trust.
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

type HmacSha256 = Hmac<Sha256>;

/// Canonical connection role; it must not be selected by an untrusted message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Device issuing the invitation.
    Initiator,
    /// Device accepting the invitation.
    Responder,
}
impl Role {
    fn byte(self) -> u8 {
        match self {
            Self::Initiator => 0,
            Self::Responder => 1,
        }
    }
    fn other(self) -> Self {
        match self {
            Self::Initiator => Self::Responder,
            Self::Responder => Self::Initiator,
        }
    }
}

/// Commitment sent before learning the other user's choice.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Commitment(pub [u8; 32]);

/// Open a previously committed selection. Do not log this message.
#[derive(Clone, Serialize, Deserialize, Zeroize)]
pub struct Reveal {
    number: u8,
    nonce: [u8; 32],
}
impl std::fmt::Debug for Reveal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Reveal([redacted])")
    }
}
impl Drop for Reveal {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Proof that a peer validated both users' committed choices.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ReadyProof(pub [u8; 32]);

/// A successful ceremony result; only produced after validating a peer's ready proof.
#[derive(Debug)]
pub struct AuthorizedPair {
    peer_id: [u8; 32],
}
impl AuthorizedPair {
    /// Authenticated peer identity eligible to be remembered by the host.
    pub fn peer_id(&self) -> &[u8; 32] {
        &self.peer_id
    }
}

/// Pairing failures never implicitly authorize a peer.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PairingError {
    /// Invalid initial context or lifetime.
    #[error("invalid pairing context")]
    Context,
    /// User selected a number not presented by this session.
    #[error("selection is not one of the pairing options")]
    Selection,
    /// Peer has not reached the required state.
    #[error("pairing is waiting for mutual confirmation")]
    NotReady,
    /// An already consumed protocol operation was repeated.
    #[error("pairing message is out of order")]
    Order,
    /// The selected numbers differ.
    #[error("pairing choices do not match")]
    Mismatch,
    /// A commitment or ready message failed validation.
    #[error("pairing proof does not match this session")]
    InvalidProof,
    /// The ceremony deadline elapsed.
    #[error("pairing request expired")]
    Expired,
    /// Refused, failed or already finalized ceremony.
    #[error("pairing request is no longer active")]
    Aborted,
}

/// Ephemeral state, deliberately not serializable or cloneable.
pub struct PairingSession {
    key: Zeroizing<[u8; 32]>,
    binding: [u8; 32],
    peer_id: [u8; 32],
    role: Role,
    options: [u8; 3],
    expires: u64,
    last_time: u64,
    local: Option<Reveal>,
    remote_commitment: Option<Commitment>,
    revealed: bool,
    matched: bool,
    ready: bool,
    aborted: bool,
}

impl PairingSession {
    /// Construct with key material derived by the authenticated TLS exporter.
    /// `now` is a monotonic seconds counter; lifetime must be 1–300 seconds.
    pub fn new(
        exporter: [u8; 32],
        initiator: [u8; 32],
        responder: [u8; 32],
        invitation_nonce: [u8; 32],
        role: Role,
        now: u64,
        lifetime: u64,
    ) -> Result<Self, PairingError> {
        if initiator == responder || !(1..=300).contains(&lifetime) {
            return Err(PairingError::Context);
        }
        let expires = now.checked_add(lifetime).ok_or(PairingError::Context)?;
        let mut hash = Sha256::new();
        hash.update(b"peersus/pair/context/1");
        hash.update(initiator);
        hash.update(responder);
        hash.update(invitation_nonce);
        let binding: [u8; 32] = hash.finalize().into();
        let key = Zeroizing::new(exporter);
        let mut options = [0; 3];
        let mut filled = 0;
        let mut counter = 0_u32;
        while filled < options.len() {
            let bytes = proof(
                &key,
                &[b"peersus/pair/options/1", &binding, &counter.to_be_bytes()],
            )?;
            for byte in bytes {
                // Rejection sampling avoids modulo bias; candidates are unique.
                if byte < 250 {
                    let number = byte % 10;
                    if !options[..filled].contains(&number) {
                        options[filled] = number;
                        filled += 1;
                        if filled == options.len() {
                            break;
                        }
                    }
                }
            }
            counter = counter.checked_add(1).ok_or(PairingError::Context)?;
        }
        // The same three numbers on both devices, and no number at the same place on both: the responder's are turned by
        // one or two places, which of the two decided by the session like the numbers themselves. Two people who each press
        // "the first one" therefore never agree; only reading the number does. What is sent and compared is the number,
        // never its place, so a device made before this simply shows the unturned order.
        if role == Role::Responder {
            let turn = 1 + usize::from(proof(&key, &[b"peersus/pair/order/1", &binding])?[0] & 1);
            options.rotate_left(turn);
        }
        Ok(Self {
            key,
            binding,
            peer_id: match role {
                Role::Initiator => responder,
                Role::Responder => initiator,
            },
            role,
            options,
            expires,
            last_time: now,
            local: None,
            remote_commitment: None,
            revealed: false,
            matched: false,
            ready: false,
            aborted: false,
        })
    }

    /// The three choices in the order this endpoint shows them: the same numbers on both authenticated endpoints, each at a
    /// different place on the two.
    pub fn options(&self) -> [u8; 3] {
        self.options
    }

    /// Commit the user's choice using a fresh OS-random nonce supplied by the host.
    pub fn choose(
        &mut self,
        number: u8,
        nonce: [u8; 32],
        now: u64,
    ) -> Result<Commitment, PairingError> {
        self.check(now)?;
        if self.local.is_some() {
            return self.fail(PairingError::Order);
        }
        if !self.options.contains(&number) {
            return Err(PairingError::Selection);
        }
        let reveal = Reveal { number, nonce };
        let commitment = Commitment(self.commitment(self.role, &reveal)?);
        self.local = Some(reveal);
        Ok(commitment)
    }

    /// Store the other endpoint's commitment without exposing either selection.
    pub fn receive_commitment(
        &mut self,
        commitment: Commitment,
        now: u64,
    ) -> Result<(), PairingError> {
        self.check(now)?;
        if self.remote_commitment.is_some() {
            return self.fail(PairingError::Order);
        }
        self.remote_commitment = Some(commitment);
        Ok(())
    }

    /// Reveal only after both choices have been committed.
    pub fn reveal(&mut self, now: u64) -> Result<Reveal, PairingError> {
        self.check(now)?;
        if self.revealed {
            return self.fail(PairingError::Order);
        }
        if self.remote_commitment.is_none() {
            return Err(PairingError::NotReady);
        }
        let reveal = self.local.clone().ok_or(PairingError::NotReady)?;
        self.revealed = true;
        Ok(reveal)
    }

    /// Verify the remote opening and produce this endpoint's mutual-ready proof.
    pub fn receive_reveal(&mut self, reveal: Reveal, now: u64) -> Result<ReadyProof, PairingError> {
        self.check(now)?;
        if self.matched {
            return self.fail(PairingError::Order);
        }
        if !self.revealed {
            return Err(PairingError::NotReady);
        }
        let commitment = self.remote_commitment.ok_or(PairingError::NotReady)?;
        let expected = self.commitment(self.role.other(), &reveal)?;
        if !bool::from(expected.ct_eq(&commitment.0)) {
            return self.fail(PairingError::InvalidProof);
        }
        let local = self.local.as_ref().ok_or(PairingError::NotReady)?;
        if local.number != reveal.number {
            return self.fail(PairingError::Mismatch);
        }
        self.matched = true;
        Ok(ReadyProof(self.ready_proof(self.role)?))
    }

    /// Validate acknowledgment that the peer also checked both selections.
    pub fn receive_ready(&mut self, ready: ReadyProof, now: u64) -> Result<(), PairingError> {
        self.check(now)?;
        if self.ready {
            return self.fail(PairingError::Order);
        }
        if !self.matched {
            return Err(PairingError::NotReady);
        }
        let expected = self.ready_proof(self.role.other())?;
        if !bool::from(expected.ct_eq(&ready.0)) {
            return self.fail(PairingError::InvalidProof);
        }
        self.ready = true;
        Ok(())
    }

    /// Obtain authorization only after the complete mutual ceremony.
    /// A failed readiness check does not consume the pending session.
    pub fn finish(&mut self, now: u64) -> Result<AuthorizedPair, PairingError> {
        self.check(now)?;
        if !self.ready {
            return Err(PairingError::NotReady);
        }
        self.abort();
        Ok(AuthorizedPair {
            peer_id: self.peer_id,
        })
    }

    /// Refuse or cancel, irreversibly discarding sensitive ceremony state.
    pub fn abort(&mut self) {
        self.aborted = true;
        self.key.zeroize();
        self.local = None;
    }

    fn check(&mut self, now: u64) -> Result<(), PairingError> {
        if self.aborted {
            return Err(PairingError::Aborted);
        }
        if now < self.last_time {
            return self.fail(PairingError::Context);
        }
        self.last_time = now;
        if now >= self.expires {
            return self.fail(PairingError::Expired);
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: PairingError) -> Result<T, PairingError> {
        self.abort();
        Err(error)
    }

    fn commitment(&self, role: Role, reveal: &Reveal) -> Result<[u8; 32], PairingError> {
        proof(
            &self.key,
            &[
                b"peersus/pair/commit/1",
                &self.binding,
                &[role.byte()],
                &[reveal.number],
                &reveal.nonce,
            ],
        )
    }

    fn ready_proof(&self, role: Role) -> Result<[u8; 32], PairingError> {
        let local = self.local.as_ref().ok_or(PairingError::NotReady)?;
        proof(
            &self.key,
            &[
                b"peersus/pair/ready/1",
                &self.binding,
                &[role.byte()],
                &[local.number],
            ],
        )
    }
}

fn proof(key: &[u8; 32], parts: &[&[u8]]) -> Result<[u8; 32], PairingError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| PairingError::Context)?;
    for part in parts {
        mac.update(part);
    }
    Ok(mac.finalize().into_bytes().into())
}
