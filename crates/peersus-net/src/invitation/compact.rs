//! Versioned QR representation, separate from the legacy text/API invitation.
//!
//! Header: public identity (32 bytes), one-use capability (32), expiry (u64 BE),
//! route count (u16 BE). Routes: 0 = IPv4 + port; 1 = IPv6 + port + flow + scope;
//! 2 = UTF-8 relay URL; 3 = custom transport id + opaque bytes. Variable fields
//! have u16 BE lengths. Base32 without padding keeps QR alphanumeric mode.
//! No discovery dependency is introduced and no route is silently discarded.

use super::{Error, Invitation, MAX_LENGTH};
use data_encoding::BASE32_NOPAD;
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

const PREFIX: &str = "PEERSUS:Q2:";
const MAX_RAW: usize = (MAX_LENGTH - PREFIX.len()) * 5 / 8;

fn append(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_RAW.saturating_sub(output.len()) {
        return Err(Error::Invalid);
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn sized(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    let length = u16::try_from(bytes.len()).map_err(|_| Error::Invalid)?;
    append(output, &length.to_be_bytes())?;
    append(output, bytes)
}

pub(super) fn encode(invitation: &Invitation) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(128);
    append(&mut bytes, invitation.address.id.as_bytes())?;
    append(&mut bytes, &invitation.capability)?;
    append(&mut bytes, &invitation.expires_at.to_be_bytes())?;
    let routes = u16::try_from(invitation.address.addrs.len()).map_err(|_| Error::Invalid)?;
    append(&mut bytes, &routes.to_be_bytes())?;
    for route in &invitation.address.addrs {
        match route {
            TransportAddr::Ip(SocketAddr::V4(address)) => {
                append(&mut bytes, &[0])?;
                append(&mut bytes, &address.ip().octets())?;
                append(&mut bytes, &address.port().to_be_bytes())?;
            }
            TransportAddr::Ip(SocketAddr::V6(address)) => {
                append(&mut bytes, &[1])?;
                append(&mut bytes, &address.ip().octets())?;
                append(&mut bytes, &address.port().to_be_bytes())?;
                append(&mut bytes, &address.flowinfo().to_be_bytes())?;
                append(&mut bytes, &address.scope_id().to_be_bytes())?;
            }
            TransportAddr::Relay(url) => {
                append(&mut bytes, &[2])?;
                sized(&mut bytes, url.as_str().as_bytes())?;
            }
            TransportAddr::Custom(address) => {
                append(&mut bytes, &[3])?;
                append(&mut bytes, &address.id().to_be_bytes())?;
                sized(&mut bytes, address.data())?;
            }
            _ => return Err(Error::Invalid),
        }
    }
    Ok(format!("{PREFIX}{}", BASE32_NOPAD.encode(&bytes)))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let (head, tail) = self.0.split_at_checked(length).ok_or(Error::Invalid)?;
        self.0 = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        self.take(N)?.try_into().map_err(|_| Error::Invalid)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn sized(&mut self) -> Result<&'a [u8], Error> {
        let length = usize::from(self.u16()?);
        self.take(length)
    }
}

pub(super) fn decode(text: &str) -> Result<Invitation, Error> {
    if text.len() > MAX_LENGTH {
        return Err(Error::Invalid);
    }
    let normalized = text.to_ascii_uppercase();
    let body = normalized.strip_prefix(PREFIX).ok_or(Error::Invalid)?;
    let bytes = BASE32_NOPAD
        .decode(body.as_bytes())
        .map_err(|_| Error::Invalid)?;
    let mut reader = Reader(&bytes);
    let id = EndpointId::from_bytes(&reader.array()?).map_err(|_| Error::Invalid)?;
    let capability = reader.array()?;
    let expires_at = u64::from_be_bytes(reader.array()?);
    let routes = reader.u16()?;
    let mut address = EndpointAddr::new(id);
    for _ in 0..routes {
        let route = match reader.array::<1>()?[0] {
            0 => {
                let ip = Ipv4Addr::from(reader.array::<4>()?);
                TransportAddr::Ip(SocketAddr::new(ip.into(), reader.u16()?))
            }
            1 => {
                let ip = Ipv6Addr::from(reader.array::<16>()?);
                let port = reader.u16()?;
                let flow = u32::from_be_bytes(reader.array()?);
                let scope = u32::from_be_bytes(reader.array()?);
                TransportAddr::Ip(SocketAddrV6::new(ip, port, flow, scope).into())
            }
            2 => {
                let url = std::str::from_utf8(reader.sized()?).map_err(|_| Error::Invalid)?;
                TransportAddr::Relay(url.parse().map_err(|_| Error::Invalid)?)
            }
            3 => {
                let id = u64::from_be_bytes(reader.array()?);
                TransportAddr::Custom(iroh_base::CustomAddr::from_parts(id, reader.sized()?))
            }
            _ => return Err(Error::Invalid),
        };
        if !address.addrs.insert(route) {
            return Err(Error::Invalid);
        }
    }
    if !reader.0.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(Invitation {
        address,
        capability,
        expires_at,
    })
}
