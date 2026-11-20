//! Versioned QR representation, separate from the legacy text/API invitation, and the same fields as bare bytes for
//! an announcement over a radio.
//!
//! Header: public identity (32 bytes), one-use capability (32), expiry (u64 BE),
//! route count (u16 BE). Routes: 0 = IPv4 + port; 1 = IPv6 + port + flow + scope;
//! 2 = UTF-8 relay URL; 3 = custom transport id + opaque bytes. Variable fields
//! have u16 BE lengths. Base32 without padding keeps QR alphanumeric mode.
//! No discovery dependency is introduced and the QR form discards no route silently. The radio form has room for
//! little, says so, and carries the routes that fit (see [`encode_within`]).

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

fn route(output: &mut Vec<u8>, route: &TransportAddr) -> Result<(), Error> {
    match route {
        TransportAddr::Ip(SocketAddr::V4(address)) => {
            append(output, &[0])?;
            append(output, &address.ip().octets())?;
            append(output, &address.port().to_be_bytes())
        }
        TransportAddr::Ip(SocketAddr::V6(address)) => {
            append(output, &[1])?;
            append(output, &address.ip().octets())?;
            append(output, &address.port().to_be_bytes())?;
            append(output, &address.flowinfo().to_be_bytes())?;
            append(output, &address.scope_id().to_be_bytes())
        }
        TransportAddr::Relay(url) => {
            append(output, &[2])?;
            sized(output, url.as_str().as_bytes())
        }
        TransportAddr::Custom(address) => {
            append(output, &[3])?;
            append(output, &address.id().to_be_bytes())?;
            sized(output, address.data())
        }
        _ => Err(Error::Invalid),
    }
}

fn header(invitation: &Invitation, routes: u16) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::with_capacity(128);
    append(&mut bytes, invitation.address.id.as_bytes())?;
    append(&mut bytes, &invitation.capability)?;
    append(&mut bytes, &invitation.expires_at.to_be_bytes())?;
    append(&mut bytes, &routes.to_be_bytes())?;
    Ok(bytes)
}

/// The compact bytes with every route: what a QR carries when it has room for all of them.
fn all_routes(invitation: &Invitation) -> Result<Vec<u8>, Error> {
    let routes = u16::try_from(invitation.address.addrs.len()).map_err(|_| Error::Invalid)?;
    let mut bytes = header(invitation, routes)?;
    for address in &invitation.address.addrs {
        route(&mut bytes, address)?;
    }
    Ok(bytes)
}

pub(super) fn encode(invitation: &Invitation) -> Result<String, Error> {
    Ok(format!(
        "{PREFIX}{}",
        BASE32_NOPAD.encode(&all_routes(invitation)?)
    ))
}

/// How many bytes the compact form has with every route: the most a budget for [`text_within`] can ask for.
pub(super) fn size(invitation: &Invitation) -> Result<usize, Error> {
    Ok(all_routes(invitation)?.len())
}

/// The QR text of the compact form with at most `budget` bytes of it, for a code that would be drawn too small to read with every route: the
/// routes that fit are carried, in the order of [`encode_within`] (relays, then IPv4, then IPv6), and the rest are left out of THIS drawing
/// only; the invitation itself, and what the owner holds, still have them all. It reads back through [`decode`] like any other.
pub(super) fn text_within(invitation: &Invitation, budget: usize) -> Result<String, Error> {
    Ok(format!(
        "{PREFIX}{}",
        BASE32_NOPAD.encode(&encode_within(invitation, budget)?)
    ))
}

/// The invitation in at most `budget` bytes, for an announcement over a radio, which has room for little. The routes
/// that fit are carried, in the order a device that hears it is most likely to reach: the relays, then IPv4, then
/// IPv6. A route of a custom transport is not carried. An invitation of which no route fits is refused: nothing here
/// finds a device by its identity alone.
pub(super) fn encode_within(invitation: &Invitation, budget: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = header(invitation, 0)?;
    let counted_at = bytes.len() - 2;
    let order = |address: &&TransportAddr| match address {
        TransportAddr::Relay(_) => 0,
        TransportAddr::Ip(SocketAddr::V4(_)) => 1,
        TransportAddr::Ip(SocketAddr::V6(_)) => 2,
        _ => 3,
    };
    let mut addresses: Vec<&TransportAddr> = invitation.address.addrs.iter().collect();
    addresses.sort_by_key(order);
    let mut carried = 0_u16;
    for address in addresses.into_iter().filter(|address| order(address) < 3) {
        let mut one = Vec::with_capacity(32);
        route(&mut one, address)?;
        if bytes.len() + one.len() <= budget {
            bytes.extend_from_slice(&one);
            carried += 1;
        }
    }
    if carried == 0 || bytes.len() > budget {
        return Err(Error::Invalid);
    }
    bytes[counted_at..counted_at + 2].copy_from_slice(&carried.to_be_bytes());
    Ok(bytes)
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
    read(&bytes)
}

/// The invitation the bytes of either form are, whoever made them: every length is checked and nothing may follow.
pub(super) fn read(bytes: &[u8]) -> Result<Invitation, Error> {
    if bytes.len() > MAX_RAW {
        return Err(Error::Invalid);
    }
    let mut reader = Reader(bytes);
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
