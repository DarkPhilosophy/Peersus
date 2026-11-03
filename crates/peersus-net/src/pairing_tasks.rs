//! Host-owned pairing work. CLI and GUI submit choices to the same coordinator.
use crate::{ceremony, handshake::Admitted, peers::Peers};
use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use peersus_core::{
    model::PairingPrompt,
    pairing::{AuthorizedPair, Role},
};
use std::{collections::BTreeMap, time::Instant};
use tokio::{
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinSet},
};

const MAX_PENDING: usize = 8;

struct Pending {
    connection: Connection,
    choice: Option<oneshot::Sender<u8>>,
    options: Option<[u8; 3]>,
    started: Instant,
    name: String,
    abort: AbortHandle,
}

struct Finished {
    id: String,
    connection: Connection,
    address: EndpointAddr,
    name: String,
    result: Result<AuthorizedPair, ceremony::Error>,
}

pub enum Event {
    Prompt(PairingPrompt),
    Paired {
        request: String,
        connection: Connection,
    },
    Failed {
        request: String,
        reason: String,
    },
}

pub struct PairingTasks {
    local: EndpointId,
    pending: BTreeMap<String, Pending>,
    tasks: JoinSet<Finished>,
    prompts_tx: mpsc::Sender<(String, [u8; 3])>,
    prompts_rx: mpsc::Receiver<(String, [u8; 3])>,
}

impl PairingTasks {
    pub fn new(local: EndpointId) -> Self {
        let (prompts_tx, prompts_rx) = mpsc::channel(MAX_PENDING);
        Self {
            local,
            pending: BTreeMap::new(),
            tasks: JoinSet::new(),
            prompts_tx,
            prompts_rx,
        }
    }

    /// Admission must already have authenticated the single-use invitation.
    pub fn start(&mut self, admitted: Admitted, role: Role) -> Result<String, &'static str> {
        let address = admitted.address.clone();
        // The ceremony is for a code shown to pair, and for nothing else: a connection admitted to carry files is never
        // turned into a pairing, here or by anything that calls this later.
        if admitted.purpose != crate::handshake::Purpose::Pairing {
            admitted.connection.close(1_u32.into(), b"not a pairing");
            return Err("Not a pairing");
        }
        if self.pending.len() >= MAX_PENDING || admitted.connection.remote_id() != address.id {
            admitted
                .connection
                .close(1_u32.into(), b"pairing unavailable");
            return Err("Pairing unavailable");
        }
        let id = hex::encode(rand::random::<[u8; 16]>());
        let (choice_tx, choice_rx) = oneshot::channel();
        let connection = admitted.connection.clone();
        let name = admitted.name.clone();
        let task_id = id.clone();
        let local = self.local;
        let prompts = self.prompts_tx.clone();
        let abort = self.tasks.spawn(async move {
            let (prompt_tx, prompt_rx) = oneshot::channel();
            let (result, ()) = tokio::join!(
                ceremony::run(
                    &admitted.connection,
                    local,
                    role,
                    admitted.nonce,
                    prompt_tx,
                    choice_rx
                ),
                async {
                    if let Ok(options) = prompt_rx.await {
                        let _ = prompts.send((task_id.clone(), options)).await;
                    }
                }
            );
            Finished {
                id: task_id,
                connection: admitted.connection,
                address,
                name: admitted.name,
                result,
            }
        });
        self.pending.insert(
            id.clone(),
            Pending {
                connection,
                choice: Some(choice_tx),
                options: None,
                started: Instant::now(),
                name,
                abort,
            },
        );
        Ok(id)
    }

    pub fn choose(&mut self, request: &str, number: u8) -> Result<(), &'static str> {
        let pending = self
            .pending
            .get_mut(request)
            .ok_or("Pairing request expired")?;
        if !pending
            .options
            .is_some_and(|options| options.contains(&number))
        {
            return Err("Choose one of the displayed numbers");
        }
        pending
            .choice
            .take()
            .ok_or("Choice already submitted")?
            .send(number)
            .map_err(|_| "Pairing request expired")
    }

    pub fn reject(&mut self, request: &str) -> bool {
        if let Some(pending) = self.pending.remove(request) {
            pending.connection.close(1_u32.into(), b"pairing refused");
            pending.abort.abort();
            true
        } else {
            false
        }
    }

    pub fn prompts(&self) -> Vec<PairingPrompt> {
        self.pending
            .iter()
            .filter_map(|(id, pending)| {
                pending.options.map(|options| PairingPrompt {
                    id: id.clone(),
                    name: pending.name.clone(),
                    options,
                    selected: pending.choice.is_none(),
                    remaining_seconds: 120_u64.saturating_sub(pending.started.elapsed().as_secs()),
                })
            })
            .collect()
    }

    /// Drives progress without blocking the caller's command/event select loop.
    /// Only completed mutual consent reaches persistent storage.
    pub async fn next(&mut self, peers: &mut Peers, now: u64) -> Event {
        loop {
            tokio::select! {
                Some((id, options)) = self.prompts_rx.recv() => {
                    if let Some(pending) = self.pending.get_mut(&id) {
                        pending.options = Some(options);
                        return Event::Prompt(PairingPrompt {
                            id, name: pending.name.clone(), options,
                            selected: pending.choice.is_none(),
                            remaining_seconds: 120_u64.saturating_sub(pending.started.elapsed().as_secs()),
                        });
                    }
                }
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    let finished = match result {
                        Ok(finished) => finished,
                        Err(error) => {
                            let request = self.pending.iter()
                                .find(|(_, pending)| pending.abort.id() == error.id())
                                .map(|(id, _)| id.clone());
                            if let Some(request) = request {
                                self.reject(&request);
                                return Event::Failed {
                                    request,
                                    reason: "Pairing task stopped unexpectedly".to_owned(),
                                };
                            }
                            continue;
                        }
                    };
                    if self.pending.remove(&finished.id).is_none() {
                        finished.connection.close(1_u32.into(), b"pairing cancelled");
                        continue;
                    }
                    let result = match finished.result {
                        Ok(authorization) => peers.remember(authorization, finished.address, finished.name, now)
                            .map_err(|_| "Could not save paired device".to_owned()),
                        Err(_) => Err("Pairing was refused, expired or did not match".to_owned()),
                    };
                    return match result {
                        Ok(()) => Event::Paired { request: finished.id, connection: finished.connection },
                        Err(reason) => {
                            finished.connection.close(1_u32.into(), b"pairing failed");
                            Event::Failed { request: finished.id, reason }
                        }
                    };
                }
            }
        }
    }
}

impl Drop for PairingTasks {
    fn drop(&mut self) {
        for pending in self.pending.values() {
            pending.connection.close(1_u32.into(), b"host stopped");
        }
        self.tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;
    use std::time::Duration;

    async fn refused_pairing(cancel: bool) {
        let directory = tempfile::tempdir().unwrap();
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
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
        let nonce = rand::random();
        let mut tasks_a = PairingTasks::new(a.id());
        let mut tasks_b = PairingTasks::new(b.id());
        let id_a = tasks_a
            .start(
                Admitted {
                    connection: left.unwrap(),
                    nonce,
                    name: "B".into(),
                    address: b.addr(),
                    purpose: crate::handshake::Purpose::Pairing,
                },
                Role::Initiator,
            )
            .unwrap();
        let id_b = tasks_b
            .start(
                Admitted {
                    connection: right.unwrap(),
                    nonce,
                    name: "A".into(),
                    address: a.addr(),
                    purpose: crate::handshake::Purpose::Pairing,
                },
                Role::Responder,
            )
            .unwrap();
        let path_a = directory.path().join("a.json");
        let path_b = directory.path().join("b.json");
        let mut peers_a = Peers::open(&path_a).unwrap();
        let mut peers_b = Peers::open(&path_b).unwrap();
        let (prompt_a, prompt_b) = tokio::join!(
            tasks_a.next(&mut peers_a, 100),
            tasks_b.next(&mut peers_b, 100)
        );
        let (Event::Prompt(prompt_a), Event::Prompt(prompt_b)) = (prompt_a, prompt_b) else {
            panic!("expected consent requests");
        };
        tasks_b.choose(&id_b, prompt_b.options[1]).unwrap();
        if cancel {
            assert!(tasks_a.reject(&id_a));
            assert!(!tasks_a.reject(&id_a));
            assert!(tasks_a.prompts().is_empty());
        } else {
            tasks_a.choose(&id_a, prompt_a.options[0]).unwrap();
            let failed =
                tokio::time::timeout(Duration::from_secs(5), tasks_a.next(&mut peers_a, 101))
                    .await
                    .unwrap();
            assert!(matches!(failed, Event::Failed { .. }));
        }
        let failed = tokio::time::timeout(Duration::from_secs(5), tasks_b.next(&mut peers_b, 101))
            .await
            .unwrap();
        assert!(matches!(failed, Event::Failed { .. }));
        assert_eq!(Peers::open(path_a).unwrap().entries().count(), 0);
        assert_eq!(Peers::open(path_b).unwrap().entries().count(), 0);
        assert!(tasks_b.prompts().is_empty());
        drop(tasks_a);
        drop(tasks_b);
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn rejecting_pending_consent_cancels_both_without_persistence() {
        refused_pairing(true).await;
    }

    #[tokio::test]
    async fn mismatched_choices_never_reach_peer_storage() {
        refused_pairing(false).await;
    }

    #[tokio::test]
    async fn a_connection_admitted_for_files_is_never_started_as_a_pairing() {
        use iroh::SecretKey;
        use peersus_core::config::NetworkSettings;
        let policy = NetworkSettings {
            bind: Some("127.0.0.1".parse().unwrap()),
            allow_relay: false,
            ..NetworkSettings::default()
        };
        let a = crate::transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let b = crate::transport::bind(SecretKey::generate(), &policy)
            .await
            .unwrap();
        let (left, _right) = tokio::join!(a.connect(b.addr(), crate::transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let mut tasks = PairingTasks::new(a.id());
        let admitted = Admitted {
            connection: left.unwrap(),
            nonce: rand::random(),
            name: "B".into(),
            address: b.addr(),
            purpose: crate::handshake::Purpose::Files,
        };
        assert_eq!(tasks.start(admitted, Role::Initiator), Err("Not a pairing"));
        assert!(
            tasks.prompts().is_empty(),
            "a files connection reached the pairing ceremony"
        );
        a.close().await;
        b.close().await;
    }
}
