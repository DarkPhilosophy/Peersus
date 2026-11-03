//! Typed stream preambles keep file offers separate from agnostic plugin traffic.
//! Routing is not authorization: the host must still admit the peer and service.
use crate::{transfer::Offer, transport};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    File { offer: Offer },
    Plugin { service: String, version: u32 },
    /// A device telling this one which profile picture it holds. It carries nothing: the digest and
    /// the picture come on the stream itself, under `profile_exchange`, so that what the stream is
    /// for is one word and the rest is judged where it is read.
    Profile {},
}

impl Request {
    pub fn validate(&self) -> Result<(), transport::Error> {
        match self {
            Self::File { offer } => offer.validate().map_err(|_| transport::Error::Invalid),
            Self::Profile {} => Ok(()),
            Self::Plugin { service, version } => {
                if *version == 0
                    || service.is_empty()
                    || service.len() > 128
                    || !service
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                {
                    Err(transport::Error::Invalid)
                } else {
                    Ok(())
                }
            }
        }
    }
}

pub async fn send(stream: &mut SendStream, request: &Request) -> Result<(), transport::Error> {
    request.validate()?;
    transport::send(stream, request).await
}

pub async fn receive(stream: &mut RecvStream) -> Result<Request, transport::Error> {
    let request: Request = transport::receive(stream).await?;
    request.validate()?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;

    #[tokio::test]
    async fn file_and_plugin_streams_route_independently_on_one_connection() {
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
        };
        let a = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, right) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let left = left.unwrap();
        let right = right.unwrap();
        let send_all = async {
            for request in [
                Request::Plugin {
                    service: "example.service".into(),
                    version: 1,
                },
                Request::File {
                    offer: Offer {
                        id: "a".repeat(32),
                        name: "example.txt".into(),
                        bytes: 3,
                        folder: None,
                    },
                },
            ] {
                let (mut tx, mut rx) = left.open_bi().await.unwrap();
                send(&mut tx, &request).await.unwrap();
                tx.finish().unwrap();
                assert!(transport::receive::<bool>(&mut rx).await.unwrap());
            }
        };
        let route_all = async {
            for plugin in [true, false] {
                let (mut tx, mut rx) = right.accept_bi().await.unwrap();
                let request = receive(&mut rx).await.unwrap();
                match request {
                    Request::Plugin { service, version } => {
                        assert!(plugin);
                        assert_eq!(service, "example.service");
                        assert_eq!(version, 1);
                    }
                    Request::File { offer } => {
                        assert!(!plugin);
                        assert_eq!(offer.bytes, 3);
                    }
                    Request::Profile {} => unreachable!("this test sends no profile stream"),
                }
                transport::send(&mut tx, &true).await.unwrap();
                tx.finish().unwrap();
            }
        };
        tokio::join!(send_all, route_all);
        left.close(0_u32.into(), b"done");
        right.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[test]
    fn a_profile_request_is_one_word_and_nothing_can_ride_along_with_it() {
        let request: Request = serde_json::from_str(r#"{"kind":"profile"}"#).unwrap();
        assert!(matches!(request, Request::Profile {}));
        request.validate().unwrap();
        assert_eq!(serde_json::to_string(&Request::Profile {}).unwrap(), r#"{"kind":"profile"}"#);
        for bad in [
            r#"{"kind":"profile","digest":"00"}"#,
            r#"{"kind":"profile","x":1}"#,
            r#"{"kind":"Profile"}"#,
            r#"{"kind":"profiles"}"#,
        ] {
            assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn invalid_routes_and_offers_are_rejected_before_dispatch() {
        for (service, version) in [("", 1), ("example.service", 0), ("../example", 1)] {
            assert!(
                Request::Plugin {
                    service: service.into(),
                    version
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Request::File {
                offer: Offer {
                    id: "a".repeat(32),
                    name: "../outside".into(),
                    bytes: 0,
                    folder: None,
                }
            }
            .validate()
            .is_err()
        );
        assert!(serde_json::from_str::<Request>(r#"{"kind":"unknown"}"#).is_err());
    }
}
