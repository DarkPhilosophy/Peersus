//! Single-owner admission gate: a QR capability permits exactly one ceremony.
use crate::invitation::Invitation;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub struct Admission {
    digest: Option<[u8; 32]>,
    deadline: Instant,
}

impl Admission {
    pub fn new(invitation: &Invitation, now_unix: u64, now: Instant) -> Option<Self> {
        let seconds = invitation.expires_at.checked_sub(now_unix)?;
        if seconds == 0 || seconds > 300 {
            return None;
        }
        Some(Self {
            digest: Some(Sha256::digest(invitation.capability).into()),
            deadline: now.checked_add(Duration::from_secs(seconds))?,
        })
    }

    /// Invalid attempts do not revoke another user's live invitation.
    /// The host separately bounds concurrent handshakes and request frequency.
    pub fn consume(&mut self, capability: &[u8; 32], now: Instant) -> bool {
        use subtle::ConstantTimeEq;
        if now >= self.deadline {
            self.digest = None;
            return false;
        }
        let Some(expected) = self.digest else {
            return false;
        };
        let supplied: [u8; 32] = Sha256::digest(capability).into();
        if !bool::from(expected.ct_eq(&supplied)) {
            return false;
        }
        self.digest = None;
        true
    }

    /// Whether this capability could still admit a device at `now`: not used, not expired. Read-only: asking never consumes it,
    /// and an expired one is reported as not live without being changed, so that this can be asked as often as a screen draws.
    pub fn is_live(&self, now: Instant) -> bool {
        self.digest.is_some() && now < self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{EndpointAddr, SecretKey};

    #[test]
    fn admission_is_single_use_and_monotonically_expires() {
        let invitation =
            Invitation::create(EndpointAddr::new(SecretKey::generate().public()), 100).unwrap();
        let now = Instant::now();
        let mut gate = Admission::new(&invitation, 100, now).unwrap();
        assert!(gate.consume(&invitation.capability, now));
        assert!(!gate.consume(&invitation.capability, now));
        let mut gate = Admission::new(&invitation, 100, now).unwrap();
        assert!(!gate.consume(&invitation.capability, now + Duration::from_secs(300)));
    }

    #[test]
    fn failed_attempt_does_not_revoke_another_users_invitation() {
        let invitation =
            Invitation::create(EndpointAddr::new(SecretKey::generate().public()), 100).unwrap();
        let now = Instant::now();
        let mut gate = Admission::new(&invitation, 100, now).unwrap();
        let mut wrong = invitation.capability;
        wrong[0] ^= 1;
        assert!(!gate.consume(&wrong, now));
        assert!(gate.consume(&invitation.capability, now));
        assert!(!gate.consume(&invitation.capability, now));
        assert!(Admission::new(&invitation, 400, now).is_none());
    }

    #[test]
    fn liveness_is_read_without_consuming_and_ends_by_use_or_by_time() {
        let invitation =
            Invitation::create(EndpointAddr::new(SecretKey::generate().public()), 100).unwrap();
        let now = Instant::now();
        let mut gate = Admission::new(&invitation, 100, now).unwrap();
        // Asking any number of times changes nothing: the code still admits its one device.
        assert!(gate.is_live(now));
        assert!(gate.is_live(now));
        assert!(gate.consume(&invitation.capability, now), "asking whether it was live used it up");
        assert!(!gate.is_live(now), "a used code was still reported live");
        // By time, and without anyone trying it.
        let fresh = Admission::new(&invitation, 100, now).unwrap();
        assert!(fresh.is_live(now + Duration::from_secs(299)));
        assert!(!fresh.is_live(now + Duration::from_secs(300)), "an expired code was still reported live");
    }
}
