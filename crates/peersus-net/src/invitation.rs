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

    /// How many bytes the compact QR form has with every route (see [`Self::compact_for_qr_within`]).
    pub fn compact_size(text: &str) -> Result<usize, Error> {
        compact::size(&Self::parse(text)?)
    }

    /// The compact QR form with at most `budget` bytes of the compact form, for a code that would be drawn too small to read with every route.
    /// The routes that fit are carried, relays first, then IPv4, then IPv6; a route of a custom transport is not. What is left out is left out of
    /// this drawing alone: the invitation and what the owner holds keep every route, and a scanner expands what it reads with [`Self::expand_qr`].
    /// An error when not even one route fits.
    pub fn compact_for_qr_within(text: &str, budget: usize) -> Result<String, Error> {
        compact::text_within(&Self::parse(text)?, budget)
    }

    /// Recover legacy invitation text from a compact QR. The owner still validates
    /// its expiry and consumes its one-use capability through the normal pairing path.
    pub fn expand_qr(text: &str) -> Result<String, Error> {
        compact::decode(text)?.encode()
    }

    pub fn decode(text: &str, now: u64) -> Result<Self, Error> {
        Self::parse(text)?.live(now)
    }

    /// The invitation as bytes for an announcement over a radio: at most `budget` of them, with the routes that fit
    /// (the relays first). Whoever is in range of the radio reads the capability, where a code on a screen is read
    /// only by who sees the screen; what that changes is the caller's to weigh, not this conversion's.
    pub fn to_radio(&self, budget: usize) -> Result<Vec<u8>, Error> {
        compact::encode_within(self, budget)
    }

    /// What [`Self::to_radio`] made, as it was heard: from anyone, so checked like any other invitation.
    pub fn from_radio(bytes: &[u8], now: u64) -> Result<Self, Error> {
        compact::read(bytes)?.live(now)
    }

    /// This invitation as one that asks the device that announced it to take files, not to pair: the same device and
    /// lifetime, with a capability made from this one that cannot be turned back into it. The device that announced
    /// decides what each of the two admits; one that knows nothing of files this way refuses this one.
    pub fn for_files(&self) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"peersus nearby files v1\0");
        hash.update(self.capability);
        Self {
            address: self.address.clone(),
            capability: hash.finalize().into(),
            expires_at: self.expires_at,
        }
    }

    fn live(self, now: u64) -> Result<Self, Error> {
        if self.expires_at <= now || self.expires_at.saturating_sub(now) > LIFETIME {
            return Err(Error::Expired);
        }
        Ok(self)
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
                "fe80::1234".parse::<Ipv6Addr>().unwrap(),
                4568,
                0,
                7,
            )))
            .with_relay_url("https://relay.example.org/".parse().unwrap())
            .with_addrs([TransportAddr::Custom(iroh_base::CustomAddr::from_parts(
                77,
                b"custom route",
            ))]);
        Invitation {
            address,
            capability: [7; 32],
            expires_at: 400,
        }
    }

    #[test]
    fn compact_qr_preserves_every_route_identity_capability_and_expiry_without_changing_legacy_text()
     {
        let invitation = routed_invitation();
        let legacy = invitation.encode().unwrap();
        let compact = Invitation::compact_for_qr(&legacy).unwrap();
        assert!(
            compact.len() * 2 < legacy.len(),
            "compact invitation did not remove the serialization overhead"
        );
        let expanded = Invitation::expand_qr(&compact).unwrap();
        assert!(
            expanded == legacy,
            "QR conversion changed the legacy protocol"
        );
        let decoded = Invitation::decode(&expanded, 101).unwrap();
        assert!(decoded.address == invitation.address);
        assert!(decoded.capability == invitation.capability);
        assert_eq!(decoded.expires_at, invitation.expires_at);
        assert!(matches!(
            Invitation::decode(&expanded, 400),
            Err(Error::Expired)
        ));
        assert!(matches!(
            Invitation::decode(&expanded, 99),
            Err(Error::Expired)
        ));
    }

    #[test]
    fn a_compact_qr_refuses_truncation_trailing_bytes_unknown_routes_and_invalid_lengths() {
        let encoded = Invitation::compact_for_qr(&routed_invitation().encode().unwrap()).unwrap();
        let body = encoded.strip_prefix("PEERSUS:Q2:").unwrap();
        let bytes = data_encoding::BASE32_NOPAD.decode(body.as_bytes()).unwrap();
        let wrap =
            |bytes: &[u8]| format!("PEERSUS:Q2:{}", data_encoding::BASE32_NOPAD.encode(bytes));
        for end in 0..bytes.len() {
            assert!(
                Invitation::expand_qr(&wrap(&bytes[..end])).is_err(),
                "truncated compact payload was accepted"
            );
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

    #[test]
    fn the_budgeted_qr_form_is_the_compact_one_with_fewer_routes_relays_first_and_reads_back_like_it()
     {
        use iroh::TransportAddr;
        let invitation = routed_invitation();
        let legacy = invitation.encode().unwrap();
        let top = Invitation::compact_size(&legacy).unwrap();
        // With room for every route it is the compact form as it was, byte for byte.
        assert_eq!(
            Invitation::compact_for_qr_within(&legacy, top)
                .unwrap()
                .len(),
            Invitation::compact_for_qr(&legacy).unwrap().len() - custom_route_text_len(&invitation)
        );
        // Room for the header and the relay alone: the relay is what is drawn, and it reads back as an invitation of the same device.
        let relay_only = 74 + 3 + "https://relay.example.org/".len();
        let text = Invitation::compact_for_qr_within(&legacy, relay_only).unwrap();
        assert!(text.len() < Invitation::compact_for_qr(&legacy).unwrap().len());
        let read = Invitation::decode(&Invitation::expand_qr(&text).unwrap(), 101).unwrap();
        assert!(read.address.id == invitation.address.id);
        assert!(read.capability == invitation.capability);
        assert_eq!(read.expires_at, invitation.expires_at);
        assert_eq!(read.address.addrs.len(), 1);
        assert!(matches!(
            read.address.addrs.iter().next(),
            Some(TransportAddr::Relay(_))
        ));
        // No room for any route: refused, as the radio form refuses it. Text that is not an invitation has no size.
        assert!(Invitation::compact_for_qr_within(&legacy, 74).is_err());
        assert!(Invitation::compact_size("hello").is_err());
        assert!(Invitation::compact_for_qr_within("hello", 500).is_err());
    }

    /// The characters the custom route adds to the compact text, which a budgeted text never carries.
    fn custom_route_text_len(invitation: &Invitation) -> usize {
        let without = Invitation {
            address: EndpointAddr::new(invitation.address.id).with_addrs(
                invitation
                    .address
                    .addrs
                    .iter()
                    .filter(|address| !matches!(address, iroh::TransportAddr::Custom(_)))
                    .cloned(),
            ),
            capability: invitation.capability,
            expires_at: invitation.expires_at,
        };
        Invitation::compact_for_qr(&invitation.encode().unwrap())
            .unwrap()
            .len()
            - Invitation::compact_for_qr(&without.encode().unwrap())
                .unwrap()
                .len()
    }

    #[test]
    fn the_radio_form_carries_the_routes_that_fit_relays_first_and_is_checked_as_any_invitation() {
        use iroh::TransportAddr;
        let invitation = routed_invitation();
        // Room for everything but the custom route, which is never carried.
        let all = invitation.to_radio(200).unwrap();
        assert!(all.len() <= 200);
        let heard = Invitation::from_radio(&all, 101).unwrap();
        assert!(heard.address.id == invitation.address.id);
        assert!(heard.capability == invitation.capability);
        assert_eq!(heard.expires_at, invitation.expires_at);
        assert_eq!(heard.address.addrs.len(), 3);
        assert!(
            !heard
                .address
                .addrs
                .iter()
                .any(|address| matches!(address, TransportAddr::Custom(_)))
        );
        // Room for the header and the relay alone: the relay is what is kept.
        let relay_only = 74 + 3 + "https://relay.example.org/".len();
        let least = invitation.to_radio(relay_only).unwrap();
        let heard = Invitation::from_radio(&least, 101).unwrap();
        assert_eq!(heard.address.addrs.len(), 1);
        assert!(heard.address.relay_urls().next().is_some());
        // No room for any route: refused, not announced as an identity nobody can reach.
        assert!(invitation.to_radio(74).is_err());
        // Heard from anyone: expiry, truncation and trailing bytes are refused.
        assert!(matches!(
            Invitation::from_radio(&all, 400),
            Err(Error::Expired)
        ));
        for end in 0..all.len() {
            assert!(Invitation::from_radio(&all[..end], 101).is_err());
        }
        let mut trailing = all.clone();
        trailing.push(0);
        assert!(Invitation::from_radio(&trailing, 101).is_err());
    }
}
