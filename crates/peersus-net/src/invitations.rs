//! Shared host invitation authority with bounded unauthenticated work.
//!
//! TWO GATES, NEVER ONE. A code shown to pair a device (Peers) and a code shown to hand over files (Send) are different
//! things with different consequences, so each has its own gate: showing the one never revokes the other, and a capability
//! that opens the files gate can never start a pairing. Which gate admitted a connection is reported with it ([`Purpose`]),
//! decided by the HOST from the capability that was presented, never by anything the connecting device says about itself.
use crate::{
    admission::Admission,
    handshake,
    invitation::{self, Invitation},
};
use iroh::{Endpoint, endpoint::Connection};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

pub use crate::handshake::Purpose;

struct State {
    pairing: Option<Admission>,
    /// The pairing code before the current one, still valid until it is used or runs out (see [`Invitations::create`]).
    pairing_older: Option<Admission>,
    files: Option<Admission>,
    files_purpose: Purpose,
    /// How many files codes were made. The files code in `files` is the one of this number.
    generation: u64,
    window: Instant,
    attempts: u8,
}

pub struct Invitations {
    state: Mutex<State>,
    handshakes: Semaphore,
}

impl Default for Invitations {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                pairing: None,
                pairing_older: None,
                files: None,
                files_purpose: Purpose::Files,
                generation: 0,
                window: Instant::now(),
                attempts: 0,
            }),
            handshakes: Semaphore::new(8),
        }
    }
}

impl Invitations {
    /// A new displayed pairing code takes the place of the current one, and the current one is not revoked at once: it stays valid until it is used
    /// or runs out, as the OLDER one, so that a device that scanned it a moment before this one was made (a code is made anew a little before it
    /// runs out, for this) is not refused because the network took its time. Never more than two: the older one of those that were
    /// already two is let go of, and each lives five minutes at most.
    pub fn create(&self, endpoint: &Endpoint, now: u64) -> Result<Invitation, invitation::Error> {
        let invitation = Invitation::create(endpoint.addr(), now)?;
        let gate = Admission::new(&invitation, now, Instant::now());
        let mut state = self.state.lock().map_err(|_| invitation::Error::Invalid)?;
        state.pairing_older = state.pairing.take();
        state.pairing = gate;
        Ok(invitation)
    }

    /// A new displayed files code revokes the previous, still-unused files code, and only that one. Returns the code and the
    /// generation it is: a number that is higher for every files code made, which an admission carries back, so that what was
    /// kept for one code is never given to a device that was admitted with another.
    pub fn create_for_files(
        &self,
        endpoint: &Endpoint,
        now: u64,
    ) -> Result<(Invitation, u64), invitation::Error> {
        self.create_for_file_selection(endpoint, now, 16)
    }

    pub(crate) fn create_for_file_selection(
        &self,
        endpoint: &Endpoint,
        now: u64,
        files: usize,
    ) -> Result<(Invitation, u64), invitation::Error> {
        let purpose = if files <= 16 {
            Purpose::Files
        } else {
            Purpose::FileSelection {
                files: u32::try_from(files).map_err(|_| invitation::Error::Invalid)?,
            }
        };
        let invitation = Invitation::create(endpoint.addr(), now)?;
        let gate = Admission::new(&invitation, now, Instant::now());
        let mut state = self.state.lock().map_err(|_| invitation::Error::Invalid)?;
        state.generation = state.generation.wrapping_add(1);
        state.files = gate;
        state.files_purpose = purpose;
        Ok((invitation, state.generation))
    }

    /// Revoke both: the network changed, so no old code may admit anyone.
    pub fn revoke(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pairing = None;
            state.pairing_older = None;
            state.files = None;
        }
    }

    /// Let go of the files code if it can admit no one any more (it was scanned, ran out or was revoked), so that a screen that
    /// still draws it as "scanned" is told it is gone and asks for files again. A code that is still live is kept, and the pairing
    /// code is never touched: this is about the files code alone. Returns whether one was let go of.
    pub fn retire_files_if_spent(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let spent = state
            .files
            .as_ref()
            .is_some_and(|gate| !gate.is_live(Instant::now()));
        if spent {
            state.files = None;
        }
        spent
    }

    /// The files code on show, for a screen to draw from: its generation and whether it could still admit a device. `None` before
    /// any was made, or when the network changed and it was revoked. The code itself is never here, only what a screen needs to know
    /// about it: the capability is a bearer, and a screen that holds it can be made to show it.
    pub fn files_code(&self) -> Option<(u64, bool)> {
        let state = self.state.lock().ok()?;
        Some((
            state.generation,
            state.files.as_ref()?.is_live(Instant::now()),
        ))
    }

    pub async fn accept(
        &self,
        connection: Connection,
        name: String,
    ) -> Result<(handshake::Admitted, Purpose, u64), handshake::Error> {
        let permit = self.handshakes.try_acquire();
        let admitted = {
            let Ok(mut state) = self.state.lock() else {
                connection.close(1_u32.into(), b"invitation unavailable");
                return Err(handshake::Error::Refused);
            };
            if state.window.elapsed() >= Duration::from_secs(10) {
                state.window = Instant::now();
                state.attempts = 0;
            }
            if permit.is_err()
                || (state.pairing.is_none()
                    && state.pairing_older.is_none()
                    && state.files.is_none())
                || state.attempts >= 32
            {
                false
            } else {
                state.attempts += 1;
                true
            }
        };
        if !admitted {
            connection.close(1_u32.into(), b"invitation unavailable");
            return Err(handshake::Error::Refused);
        }
        // Which gate consumed the capability is told to the other side by the handshake. Both gates are tried under ONE
        // lock, so a capability is consumed once and by one gate only.
        // The generation of the files gate that consumed the capability is read UNDER THE SAME LOCK as the consuming, so that
        // it names exactly the code that was used: a code shown later is another generation, however late this finishes.
        let mut generation = 0;
        let result = handshake::accept_with(
            connection,
            |capability| {
                let mut state = self.state.lock().ok()?;
                let now = Instant::now();
                if state
                    .pairing
                    .as_mut()
                    .is_some_and(|gate| gate.consume(capability, now))
                    || state
                        .pairing_older
                        .as_mut()
                        .is_some_and(|gate| gate.consume(capability, now))
                {
                    Some(Purpose::Pairing)
                } else if state
                    .files
                    .as_mut()
                    .is_some_and(|gate| gate.consume(capability, now))
                {
                    generation = state.generation;
                    Some(state.files_purpose)
                } else {
                    None
                }
            },
            name,
        )
        .await;
        drop(permit);
        let admitted = result?;
        let purpose = admitted.purpose;
        Ok((admitted, purpose, generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport;
    use iroh::SecretKey;
    use peersus_core::config::NetworkSettings;
    use std::sync::Arc;

    #[tokio::test]
    async fn silent_connection_does_not_hold_the_invitation_gate() {
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
        let gate = Arc::new(Invitations::default());
        let invitation = gate.create(&b, 100).unwrap();
        let (silent, incoming) = tokio::join!(a.connect(b.addr(), transport::ALPN), async {
            b.accept().await.unwrap().await
        });
        let silent = silent.unwrap();
        let incoming = incoming.unwrap();
        let task_gate = Arc::clone(&gate);
        let stalled = tokio::spawn(async move { task_gate.accept(incoming, "B".into()).await });
        let (left, right) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(handshake::join(&a, invitation, "A".into()), async {
                gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                    .await
            })
        })
        .await
        .unwrap();
        left.unwrap().connection.close(0_u32.into(), b"done");
        right.unwrap().0.connection.close(0_u32.into(), b"done");
        stalled.abort();
        assert!(matches!(stalled.await, Err(error) if error.is_cancelled()));
        silent.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn replacement_revocation_and_replay_cannot_admit_old_codes() {
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
        let gate = Invitations::default();
        let old = gate.create(&b, 100).unwrap();
        let current = gate.create(&b, 100).unwrap();
        // The code before the current one still admits, once (a device that scanned it as the new one was made must not be refused because
        // of the time the network took); the current one admits once; and neither admits a second time (replay).
        for (invitation, allowed) in [
            (old.clone(), true),
            (old, false),
            (current.clone(), true),
            (current, false),
        ] {
            let (left, right) = tokio::join!(handshake::join(&a, invitation, "A".into()), async {
                gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                    .await
            });
            assert_eq!(left.is_ok(), allowed);
            assert_eq!(right.is_ok(), allowed);
            if let Ok(admitted) = left {
                admitted.connection.close(0_u32.into(), b"done");
            }
            if let Ok((admitted, purpose, _)) = right {
                assert_eq!(purpose, Purpose::Pairing);
                admitted.connection.close(0_u32.into(), b"done");
            }
        }
        let revoked = gate.create(&b, 100).unwrap();
        gate.revoke();
        let (left, right) = tokio::join!(handshake::join(&a, revoked, "A".into()), async {
            gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                .await
        });
        assert!(left.is_err());
        assert!(right.is_err());
        a.close().await;
        b.close().await;
    }

    async fn two_hosts() -> (iroh::Endpoint, iroh::Endpoint) {
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
        (a, b)
    }

    /// One scan of `invitation` by `a` on the host `b`: what `b` admitted it as, if it did.
    async fn scan(
        a: &iroh::Endpoint,
        b: &iroh::Endpoint,
        gate: &Invitations,
        invitation: Invitation,
    ) -> Option<Purpose> {
        let (left, right) = tokio::join!(handshake::join(a, invitation, "A".into()), async {
            gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                .await
        });
        if let Ok(admitted) = left {
            admitted.connection.close(0_u32.into(), b"done");
        }
        right.ok().map(|(admitted, purpose, _)| {
            admitted.connection.close(0_u32.into(), b"done");
            purpose
        })
    }

    #[tokio::test]
    async fn a_code_for_files_is_admitted_as_files_and_never_as_pairing() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        let files = gate.create_for_files(&b, 100).unwrap().0;
        assert_eq!(scan(&a, &b, &gate, files).await, Some(Purpose::Files));
        let pairing = gate.create(&b, 100).unwrap();
        assert_eq!(scan(&a, &b, &gate, pairing).await, Some(Purpose::Pairing));
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn showing_a_code_for_files_does_not_revoke_the_code_shown_to_pair_and_the_other_way_round()
     {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        // Pair code shown first, then a files code: the pair code must still admit, as pairing.
        let pairing = gate.create(&b, 100).unwrap();
        let files = gate.create_for_files(&b, 100).unwrap().0;
        assert_eq!(
            scan(&a, &b, &gate, pairing).await,
            Some(Purpose::Pairing),
            "a files code revoked the pair code"
        );
        assert_eq!(
            scan(&a, &b, &gate, files).await,
            Some(Purpose::Files),
            "the pair code revoked the files code"
        );
        // And the other order: a files code, then a pair code.
        let files = gate.create_for_files(&b, 100).unwrap().0;
        let pairing = gate.create(&b, 100).unwrap();
        assert_eq!(
            scan(&a, &b, &gate, files).await,
            Some(Purpose::Files),
            "a pair code revoked the files code"
        );
        assert_eq!(scan(&a, &b, &gate, pairing).await, Some(Purpose::Pairing));
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn a_new_files_code_replaces_only_the_previous_files_code_and_each_is_single_use() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        let old = gate.create_for_files(&b, 100).unwrap().0;
        let current = gate.create_for_files(&b, 100).unwrap().0;
        assert_eq!(
            scan(&a, &b, &gate, old).await,
            None,
            "a replaced files code still admitted"
        );
        assert_eq!(
            scan(&a, &b, &gate, current.clone()).await,
            Some(Purpose::Files)
        );
        assert_eq!(
            scan(&a, &b, &gate, current).await,
            None,
            "a files code admitted twice"
        );
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn the_pairing_code_before_the_current_one_still_admits_and_never_more_than_two_are_held()
    {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        // A device scans the code a moment before a new one is made: it must not be refused for the time the network took.
        let first = gate.create(&b, 100).unwrap();
        let second = gate.create(&b, 100).unwrap();
        assert_eq!(
            scan(&a, &b, &gate, first.clone()).await,
            Some(Purpose::Pairing),
            "the code before the current one was refused"
        );
        assert_eq!(
            scan(&a, &b, &gate, first).await,
            None,
            "a pairing code admitted twice"
        );
        assert_eq!(
            scan(&a, &b, &gate, second.clone()).await,
            Some(Purpose::Pairing),
            "the current code was refused"
        );
        assert_eq!(
            scan(&a, &b, &gate, second).await,
            None,
            "a pairing code admitted twice"
        );
        // Three codes: the oldest is let go of, so a code made long ago does not stay open for ever.
        let one = gate.create(&b, 100).unwrap();
        let two = gate.create(&b, 100).unwrap();
        let three = gate.create(&b, 100).unwrap();
        assert_eq!(
            scan(&a, &b, &gate, one).await,
            None,
            "a third code left the oldest open"
        );
        assert_eq!(scan(&a, &b, &gate, two).await, Some(Purpose::Pairing));
        assert_eq!(scan(&a, &b, &gate, three).await, Some(Purpose::Pairing));
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn revoking_removes_both_gates() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        let files = gate.create_for_files(&b, 100).unwrap();
        let pairing = gate.create(&b, 100).unwrap();
        gate.revoke();
        assert_eq!(scan(&a, &b, &gate, files.0).await, None);
        assert_eq!(scan(&a, &b, &gate, pairing).await, None);
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn an_admission_carries_the_generation_of_the_code_it_used_even_when_a_newer_code_was_made_before_it_finished()
     {
        let (a, b) = two_hosts().await;
        let gate = Arc::new(Invitations::default());
        let (first, first_generation) = gate.create_for_files(&b, 100).unwrap();
        // The scan of the FIRST code starts and is admitted by the gate; before its caller has looked at the result, a
        // second files code is made, which is what an owner does when a new code is asked for in between.
        let task_gate = Arc::clone(&gate);
        let admitted = tokio::spawn({
            let b = b.clone();
            async move {
                task_gate
                    .accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                    .await
            }
        });
        let joined = handshake::join(&a, first, "A".into()).await.unwrap();
        let (second, second_generation) = gate.create_for_files(&b, 100).unwrap();
        let (admitted_with, purpose, generation) = admitted.await.unwrap().unwrap();
        assert_eq!(purpose, Purpose::Files);
        assert_eq!(
            generation, first_generation,
            "the admission was given the generation of a code it did not use"
        );
        assert_ne!(
            generation, second_generation,
            "two files codes share a generation"
        );
        assert!(second_generation > first_generation);
        // The newer code is still usable, and is admitted with ITS generation.
        let (late, late_generation) = (second, second_generation);
        let (left, right) = tokio::join!(handshake::join(&a, late, "A".into()), async {
            gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                .await
        });
        let (admitted_again, _, again) = right.unwrap();
        assert_eq!(again, late_generation);
        for connection in [
            joined.connection,
            admitted_with.connection,
            left.unwrap().connection,
            admitted_again.connection,
        ] {
            connection.close(0_u32.into(), b"done");
        }
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn the_files_code_status_follows_the_code_through_scan_replacement_and_revocation() {
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
        let gate = Invitations::default();
        assert_eq!(
            gate.files_code(),
            None,
            "a code was reported before any was made"
        );
        let (first, first_generation) = gate.create_for_files(&b, 100).unwrap();
        assert_eq!(gate.files_code(), Some((first_generation, true)));
        // A PAIRING code is another gate: making one says nothing about the files code.
        let _ = gate.create(&b, 100).unwrap();
        assert_eq!(
            gate.files_code(),
            Some((first_generation, true)),
            "a pairing code changed the files code status"
        );
        // The scan: the same generation, no longer live.
        let (left, right) = tokio::join!(handshake::join(&a, first, "A".into()), async {
            gate.accept(b.accept().await.unwrap().await.unwrap(), "B".into())
                .await
        });
        let (admitted, purpose, _) = right.unwrap();
        assert_eq!(purpose, Purpose::Files);
        assert_eq!(
            gate.files_code(),
            Some((first_generation, false)),
            "a scanned code was still reported live"
        );
        // A new code is a new generation and live again.
        let (_second, second_generation) = gate.create_for_files(&b, 100).unwrap();
        assert!(second_generation > first_generation);
        assert_eq!(gate.files_code(), Some((second_generation, true)));
        // The network changed: nothing is on show.
        gate.revoke();
        assert_eq!(gate.files_code(), None, "a revoked code was still reported");
        left.unwrap().connection.close(0_u32.into(), b"done");
        admitted.connection.close(0_u32.into(), b"done");
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn a_spent_files_code_is_retired_alone_and_a_live_one_and_the_pairing_code_are_kept() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        assert!(
            !gate.retire_files_if_spent(),
            "something was retired when no code was made"
        );
        let (live, generation) = gate.create_for_files(&b, 100).unwrap();
        let pairing = gate.create(&b, 100).unwrap();
        assert!(
            !gate.retire_files_if_spent(),
            "a files code that can still admit someone was retired"
        );
        assert_eq!(gate.files_code(), Some((generation, true)));
        // Scan it: it is spent now, still the same generation.
        assert_eq!(scan(&a, &b, &gate, live).await, Some(Purpose::Files));
        assert_eq!(gate.files_code(), Some((generation, false)));
        assert!(gate.retire_files_if_spent(), "a spent files code was kept");
        assert_eq!(gate.files_code(), None, "a retired code was still reported");
        // The pairing code is not the files code's business.
        assert_eq!(
            scan(&a, &b, &gate, pairing).await,
            Some(Purpose::Pairing),
            "retiring the files code took the pairing code with it"
        );
    }
}
