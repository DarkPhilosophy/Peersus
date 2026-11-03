//! User-carried invitations bind an authenticated endpoint to a short-lived capability.
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

mod compact;

const PREFIX: &str = "peersus:1:";
const MAX_LENGTH: usize = 8192;
const LIFETIME: u64 = 300;

/// Invitation data must not be logged: the capability authorizes a pairing attempt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub address: EndpointAddr,
    pub capability: [u8; 32],
    pub expires_at: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid invitation")]
    Invalid,
    #[error("Invitation expired or outside its permitted lifetime")]
    Expired,
}

impl Invitation {
    pub fn create(address: EndpointAddr, now: u64) -> Result<Self, Error> {
        Ok(Self {
            address,
            capability: rand::random(),
            expires_at: now.checked_add(LIFETIME).ok_or(Error::Invalid)?,
        })
    }

    pub fn encode(&self) -> Result<String, Error> {
        let bytes = serde_json::to_vec(self).map_err(|_| Error::Invalid)?;
        let encoded = format!("{PREFIX}{}", hex::encode(bytes));
        if encoded.len() > MAX_LENGTH {
            return Err(Error::Invalid);
        }
        Ok(encoded)
    }

    /// Compact QR transport only; the copied/API text remains the legacy format.
    /// This conversion does not authorize a peer or extend the invitation's lifetime.
    pub fn compact_for_qr(text: &str) -> Result<String, Error> {
        compact::encode(&Self::parse(text)?)
    }

    /// Recover legacy invitation text from a compact QR. The owner still validates
    /// its expiry and consumes its one-use capability through the normal pairing path.
    pub fn expand_qr(text: &str) -> Result<String, Error> {
        compact::decode(text)?.encode()
    }

    pub fn decode(text: &str, now: u64) -> Result<Self, Error> {
        let invitation = Self::parse(text)?;
        if invitation.expires_at <= now || invitation.expires_at.saturating_sub(now) > LIFETIME {
            return Err(Error::Expired);
        }
        Ok(invitation)
    }

    fn parse(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_LENGTH {
            return Err(Error::Invalid);
        }
        let body = text.strip_prefix(PREFIX).ok_or(Error::Invalid)?;
        let bytes = hex::decode(body).map_err(|_| Error::Invalid)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn roundtrip_preserves_identity_and_capability_but_expires() {
        let address = EndpointAddr::new(SecretKey::generate().public());
        let invitation = Invitation::create(address, 100).unwrap();
        let encoded = invitation.encode().unwrap();
        let decoded = Invitation::decode(&encoded, 101).unwrap();
        assert_eq!(decoded.address, invitation.address);
        assert_eq!(decoded.capability, invitation.capability);
        assert!(matches!(
            Invitation::decode(&encoded, 400),
            Err(Error::Expired)
        ));
        assert!(matches!(
            Invitation::decode(&encoded, 99),
            Err(Error::Expired)
        ));
    }

    #[test]
    fn rejects_wrong_versions_malformed_and_oversized_inputs() {
        for text in ["peersus:2:00", "peersus:1:zz", "peersus:1:7b7d"] {
            assert!(Invitation::decode(text, 1).is_err());
        }
        assert!(Invitation::decode(&"a".repeat(MAX_LENGTH + 1), 1).is_err());
        assert!(
            Invitation::create(EndpointAddr::new(SecretKey::generate().public()), u64::MAX)
                .is_err()
        );
    }

    fn routed_invitation() -> Invitation {
        use iroh::TransportAddr;
        use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
        let address = EndpointAddr::new(SecretKey::generate().public())
            .with_ip_addr("192.0.2.12:4567".parse().unwrap())
            .with_ip_addr(SocketAddr::V6(SocketAddrV6::new(
                "fe80::1234".parse::<Ipv6Addr>().unwrap(), 4568, 0, 7)))
            .with_relay_url("https://relay.example.org/".parse().unwrap())
            .with_addrs([TransportAddr::Custom(iroh_base::CustomAddr::from_parts(77, b"custom route"))]);
        Invitation { address, capability: [7; 32], expires_at: 400 }
    }

    #[test]
    fn compact_qr_preserves_every_route_identity_capability_and_expiry_without_changing_legacy_text() {
        let invitation = routed_invitation();
        let legacy = invitation.encode().unwrap();
        let compact = Invitation::compact_for_qr(&legacy).unwrap();
        assert!(compact.len() * 2 < legacy.len(), "compact invitation did not remove the serialization overhead");
        let expanded = Invitation::expand_qr(&compact).unwrap();
        assert!(expanded == legacy, "QR conversion changed the legacy protocol");
        let decoded = Invitation::decode(&expanded, 101).unwrap();
        assert!(decoded.address == invitation.address);
        assert!(decoded.capability == invitation.capability);
        assert_eq!(decoded.expires_at, invitation.expires_at);
        assert!(matches!(Invitation::decode(&expanded, 400), Err(Error::Expired)));
        assert!(matches!(Invitation::decode(&expanded, 99), Err(Error::Expired)));
    }

    #[test]
    fn a_compact_qr_refuses_truncation_trailing_bytes_unknown_routes_and_invalid_lengths() {
        let encoded = Invitation::compact_for_qr(&routed_invitation().encode().unwrap()).unwrap();
        let body = encoded.strip_prefix("PEERSUS:Q2:").unwrap();
        let bytes = data_encoding::BASE32_NOPAD.decode(body.as_bytes()).unwrap();
        let wrap = |bytes: &[u8]| format!("PEERSUS:Q2:{}", data_encoding::BASE32_NOPAD.encode(bytes));
        for end in 0..bytes.len() {
            assert!(Invitation::expand_qr(&wrap(&bytes[..end])).is_err(), "truncated compact payload was accepted");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Invitation::expand_qr(&wrap(&trailing)).is_err());
        let mut unknown = bytes[..74].to_vec();
        unknown[72..74].copy_from_slice(&1_u16.to_be_bytes());
        unknown.push(255);
        assert!(Invitation::expand_qr(&wrap(&unknown)).is_err());
        let mut invalid_length = bytes[..74].to_vec();
        invalid_length[72..74].copy_from_slice(&1_u16.to_be_bytes());
        invalid_length.extend_from_slice(&[2, 255, 255]);
        assert!(Invitation::expand_qr(&wrap(&invalid_length)).is_err());
        assert!(Invitation::expand_qr(&format!("PEERSUS:Q2:{}", "A".repeat(MAX_LENGTH))).is_err());
    }
}
