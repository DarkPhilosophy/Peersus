//! Where a person says a paired device can be found: an IP address or a name, with a port or without one.
//!
//! This is what is TYPED, so it is judged strictly before anything is stored or looked up. A name is kept as a name and not as the address it
//! resolves to today, because that is what makes a name worth typing: it is looked up again at every connection, so a device whose address
//! changes is found by the same words.
//!
//! What it is NOT: proof that the device is there, or that it is the right one. Who answers is decided by the key the connection proves, never
//! by what was typed, so a wrong or hostile address can only fail to connect.
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

/// The longest name accepted: the limit of a DNS name.
pub const MAX_HOST: usize = 253;

/// The host part of a typed address.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    /// An IP address, written as such.
    Ip(IpAddr),
    /// A name that is looked up at each connection: letters, digits and dashes in labels separated by dots.
    Name(String),
}

/// A typed address: where to look, and the port when the person gave one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedAddress {
    pub host: Host,
    /// `None` when none was typed: the port the device was last known at is used then, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Why what was typed is not an address.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error("nothing was typed")]
    Empty,
    #[error("that is not an IP address or a name")]
    NotAnAddress,
    #[error("the port must be a number from 1 to 65535")]
    Port,
    #[error("that address cannot be reached from another device")]
    NotRoutable,
}

impl TypedAddress {
    /// Read what a person typed: `ip`, `ip:port`, `[ipv6]:port`, `name` or `name:port`. Surrounding spaces are not part of it, and a scheme,
    /// a path or anything else that makes it a link and not an address is refused, so that what is kept is only ever a place.
    pub fn parse(typed: &str) -> Result<Self, AddressError> {
        let typed = typed.trim();
        if typed.is_empty() {
            return Err(AddressError::Empty);
        }
        if typed.len() > MAX_HOST + 7 || typed.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(AddressError::NotAnAddress);
        }
        // A link, a path or an account is not a place: refused BEFORE the port is read, or a pasted link would be told its port is wrong.
        if typed.contains(['/', '\\', '@', '?', '#']) {
            return Err(AddressError::NotAnAddress);
        }
        // `ip:port` or `[ipv6]:port` as one socket address.
        if let Ok(socket) = typed.parse::<SocketAddr>() {
            return Self::checked(Host::Ip(socket.ip()), Some(socket.port()));
        }
        if let Ok(ip) = typed
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
        {
            return Self::checked(Host::Ip(ip), None);
        }
        // `name` or `name:port`: the port is the part after the last colon, and only when there is exactly one colon (a name has none).
        let (name, port) = match typed.rsplit_once(':') {
            Some((name, port)) if !name.contains(':') => (
                name,
                Some(
                    port.parse::<u16>()
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or(AddressError::Port)?,
                ),
            ),
            Some(_) => return Err(AddressError::NotAnAddress),
            None => (typed, None),
        };
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if !valid_name(&name) {
            return Err(AddressError::NotAnAddress);
        }
        Self::checked(Host::Name(name), port)
    }

    fn checked(host: Host, port: Option<u16>) -> Result<Self, AddressError> {
        if port == Some(0) {
            return Err(AddressError::Port);
        }
        if let Host::Ip(ip) = &host
            && (ip.is_unspecified() || ip.is_multicast())
        {
            return Err(AddressError::NotRoutable);
        }
        Ok(Self { host, port })
    }

    /// The address as a person reads it, for showing it back.
    pub fn shown(&self) -> String {
        let host = match &self.host {
            Host::Ip(IpAddr::V6(ip)) if self.port.is_some() => format!("[{ip}]"),
            Host::Ip(ip) => ip.to_string(),
            Host::Name(name) => name.clone(),
        };
        match self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }
}

/// A DNS name: labels of letters, digits and dashes (not at either end), separated by dots, at least one dot or a single label, bounded.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_HOST
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        // A name that is only digits and dots is a malformed IP address, not a name.
        && !name.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> Host {
        Host::Ip(text.parse().unwrap())
    }

    #[test]
    fn every_way_of_writing_where_a_device_is_reads_as_the_place_it_means() {
        for (typed, host, port) in [
            ("192.168.1.20", ip("192.168.1.20"), None),
            ("192.168.1.20:5000", ip("192.168.1.20"), Some(5000)),
            ("  192.168.1.20:5000  ", ip("192.168.1.20"), Some(5000)),
            ("2001:db8::1", ip("2001:db8::1"), None),
            ("[2001:db8::1]", ip("2001:db8::1"), None),
            ("[2001:db8::1]:4433", ip("2001:db8::1"), Some(4433)),
            (
                "phone.example.org",
                Host::Name("phone.example.org".into()),
                None,
            ),
            (
                "Phone.Example.ORG:6000",
                Host::Name("phone.example.org".into()),
                Some(6000),
            ),
            (
                "phone.example.org.",
                Host::Name("phone.example.org".into()),
                None,
            ),
            ("my-phone", Host::Name("my-phone".into()), None),
        ] {
            let read =
                TypedAddress::parse(typed).unwrap_or_else(|error| panic!("{typed:?}: {error}"));
            assert_eq!((read.host, read.port), (host, port), "{typed:?}");
        }
    }

    #[test]
    fn what_is_not_a_place_is_refused_and_says_which_kind_of_wrong_it_is() {
        for (typed, wrong) in [
            ("", AddressError::Empty),
            ("   ", AddressError::Empty),
            ("0.0.0.0", AddressError::NotRoutable),
            ("224.0.0.1:5000", AddressError::NotRoutable),
            ("[::]:5000", AddressError::NotRoutable),
            ("phone.example.org:0", AddressError::Port),
            ("phone.example.org:70000", AddressError::Port),
            ("phone.example.org:abc", AddressError::Port),
            ("phone.example.org:", AddressError::Port),
            ("https://phone.example.org", AddressError::NotAnAddress),
            ("phone.example.org/path", AddressError::NotAnAddress),
            ("phone example.org", AddressError::NotAnAddress),
            ("user@phone.example.org", AddressError::NotAnAddress),
            ("-phone.example.org", AddressError::NotAnAddress),
            ("phone-.example.org", AddressError::NotAnAddress),
            ("phone..example.org", AddressError::NotAnAddress),
            ("a:b:c", AddressError::NotAnAddress),
            ("999.999.999.999", AddressError::NotAnAddress),
            ("1.2.3", AddressError::NotAnAddress),
            ("bad\nname", AddressError::NotAnAddress),
            ("bell\u{7}", AddressError::NotAnAddress),
        ] {
            assert_eq!(TypedAddress::parse(typed), Err(wrong), "{typed:?}");
        }
        // Past the limit of a name.
        assert_eq!(
            TypedAddress::parse(&format!("{}.org", "a".repeat(254))),
            Err(AddressError::NotAnAddress)
        );
        assert_eq!(
            TypedAddress::parse(&format!("{}.org", "a".repeat(64))),
            Err(AddressError::NotAnAddress),
            "a label past 63"
        );
    }

    #[test]
    fn an_address_is_shown_back_the_way_it_can_be_typed_again() {
        for typed in [
            "192.168.1.20",
            "192.168.1.20:5000",
            "[2001:db8::1]:4433",
            "phone.example.org",
            "phone.example.org:6000",
        ] {
            let read = TypedAddress::parse(typed).unwrap();
            assert_eq!(read.shown(), typed);
            assert_eq!(TypedAddress::parse(&read.shown()).unwrap(), read, "{typed}");
        }
    }

    #[test]
    fn a_saved_address_reads_back_and_nothing_rides_along() {
        let read = TypedAddress::parse("phone.example.org:6000").unwrap();
        let json = serde_json::to_string(&read).unwrap();
        assert_eq!(serde_json::from_str::<TypedAddress>(&json).unwrap(), read);
        assert!(
            serde_json::from_str::<TypedAddress>(r#"{"host":{"name":"a.b"},"port":5,"x":1}"#)
                .is_err()
        );
        // No port is not written at all, so a file from before has the same shape.
        assert!(
            !serde_json::to_string(&TypedAddress::parse("phone.example.org").unwrap())
                .unwrap()
                .contains("port")
        );
    }
}
