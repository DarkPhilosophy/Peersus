//! Shared host invitation authority with bounded unauthenticated work.
//!
//! EACH KIND ITS OWN GATE. A code shown to pair a device (Peers), a code shown to hand over files (Send) and a pairing
//! capability announced to devices nearby are different things, so each has its own gate: showing or announcing the
//! one never revokes another, and a capability that opens the files gate can never start a pairing. Which gate
//! admitted a connection is reported with it ([`Purpose`]), decided by the HOST from the capability that was
//! presented, never by anything the connecting device says about itself.
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

/// What an invitation announced to devices nearby is for. Two different things: a device announces the one or the
/// other, and what it announced for the one never admits for the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Announced {
    /// To be paired with: the capability as announced begins a pairing attempt.
    Pairing,
    /// To be sent files without pairing: the capability made from the announced one ([`Invitation::for_files`])
    /// admits to offer files and to nothing else.
    Files,
}

/// A pairing capability that is out, and the one before it: still valid until it is used or runs out, so that a device
/// that took it a moment before the next was made is not refused because the network took its time. Never more than
/// two, and each lives five minutes at most.
#[derive(Default)]
struct Out {
    current: Option<Admission>,
    older: Option<Admission>,
}

impl Out {
    fn put(&mut self, gate: Option<Admission>) {
        self.older = self.current.take();
        self.current = gate;
    }

    fn is_none(&self) -> bool {
        self.current.is_none() && self.older.is_none()
    }

    fn consume(&mut self, capability: &[u8; 32], now: Instant) -> bool {
        self.current
            .as_mut()
            .is_some_and(|gate| gate.consume(capability, now))
            || self
                .older
                .as_mut()
                .is_some_and(|gate| gate.consume(capability, now))
    }
}

struct State {
    /// The pairing code shown on this device's screen.
    pairing: Out,
    /// What is announced to devices nearby (see [`Invitations::create_nearby`]): the capability to pair by, or the
    /// one for files made from what was announced ([`Invitation::for_files`]). Each has its own gate.
    nearby: Out,
    nearby_files: Out,
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
                pairing: Out::default(),
                nearby: Out::default(),
                nearby_files: Out::default(),
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
        state.pairing.put(gate);
        Ok(invitation)
    }

    /// A capability to announce to devices nearby, for ONE thing (`announced`). Each of the two has its own gate, kept
    /// as the displayed code's is (the one before stays valid until used or run out), so that announcing never takes
    /// away a code a person is showing on the screen, nor the other way.
    ///
    /// To pair: what it admits is a pairing ATTEMPT like any other, and the ceremony follows. For files: only the
    /// capability made from it is admitted, to OFFER files to this device and to nothing else
    /// ([`Purpose::NearbyFiles`]); the capability as announced opens no gate at all, so a device that says it takes
    /// files cannot be paired with by what it announced.
    pub fn create_nearby(
        &self,
        endpoint: &Endpoint,
        now: u64,
        announced: Announced,
    ) -> Result<Invitation, invitation::Error> {
        let invitation = Invitation::create(endpoint.addr(), now)?;
        let mut state = self.state.lock().map_err(|_| invitation::Error::Invalid)?;
        match announced {
            Announced::Pairing => {
                let gate = Admission::new(&invitation, now, Instant::now());
                state.nearby.put(gate);
            }
            Announced::Files => {
                let gate = Admission::new(&invitation.for_files(), now, Instant::now());
                state.nearby_files.put(gate);
            }
        }
        Ok(invitation)
    }

    /// Whether what was last made for devices nearby for `announced` could still admit one: not used, not run out,
    /// not revoked. Being used makes the next announcement a new one.
    pub fn nearby_live(&self, announced: Announced) -> bool {
        self.state.lock().is_ok_and(|state| {
            let out = match announced {
                Announced::Pairing => &state.nearby,
                Announced::Files => &state.nearby_files,
            };
            out.current
                .as_ref()
                .is_some_and(|gate| gate.is_live(Instant::now()))
        })
    }

    /// Take back what was announced to devices nearby, for either thing: the person no longer shows the page it was
    /// announced for.
    pub fn withdraw_nearby(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.nearby = Out::default();
            state.nearby_files = Out::default();
        }
    }

    #[cfg(test)]
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

    /// Revoke all: the network changed, so no old code may admit anyone.
    pub fn revoke(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pairing = Out::default();
            state.nearby = Out::default();
            state.nearby_files = Out::default();
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

    /// Take back the files code of `generation` while nobody has used it: the person chose another way to send what it was for. Only that
    /// code, and only if it could still admit a device: one that was scanned belongs to the device that scanned it, and one that ran out or was
    /// replaced is nothing to take back. Decided under the lock a scan consumes the code under, so a code is either scanned or taken back and
    /// never both. Returns whether it was taken back.
    pub fn withdraw_files(&self, generation: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let unused = state.generation == generation
            && state
                .files
                .as_ref()
                .is_some_and(|gate| gate.is_live(Instant::now()));
        if unused {
            state.files = None;
        }
        unused
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
                    && state.nearby.is_none()
                    && state.nearby_files.is_none()
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
                if state.pairing.consume(capability, now) || state.nearby.consume(capability, now) {
                    Some(Purpose::Pairing)
                } else if state.nearby_files.consume(capability, now) {
                    Some(Purpose::NearbyFiles)
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
    async fn what_is_announced_nearby_has_its_own_gate_is_used_once_and_goes_when_taken_back() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        assert!(!gate.nearby_live(Announced::Pairing));
        // Shown on the screen, then announced, then announced again: none of the three takes another away.
        let shown = gate.create(&b, 100).unwrap();
        let before = gate.create_nearby(&b, 100, Announced::Pairing).unwrap();
        let announced = gate.create_nearby(&b, 100, Announced::Pairing).unwrap();
        assert!(gate.nearby_live(Announced::Pairing));
        assert!(!gate.nearby_live(Announced::Files));
        assert_eq!(
            scan(&a, &b, &gate, announced.clone()).await,
            Some(Purpose::Pairing)
        );
        assert!(
            !gate.nearby_live(Announced::Pairing),
            "a used capability is still out"
        );
        assert_eq!(scan(&a, &b, &gate, announced).await, None, "used twice");
        assert_eq!(
            scan(&a, &b, &gate, before).await,
            Some(Purpose::Pairing),
            "the one announced before was revoked by the next"
        );
        assert_eq!(
            scan(&a, &b, &gate, shown).await,
            Some(Purpose::Pairing),
            "announcing revoked the code on the screen"
        );
        // Taken back: it admits no one, and the code on the screen is not touched.
        let shown = gate.create(&b, 100).unwrap();
        let announced = gate.create_nearby(&b, 100, Announced::Pairing).unwrap();
        gate.withdraw_nearby();
        assert!(!gate.nearby_live(Announced::Pairing));
        assert_eq!(scan(&a, &b, &gate, announced).await, None);
        assert_eq!(scan(&a, &b, &gate, shown).await, Some(Purpose::Pairing));
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn what_is_announced_for_files_admits_by_the_capability_made_from_it_and_never_pairs() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        let announced = gate.create_nearby(&b, 100, Announced::Files).unwrap();
        assert!(gate.nearby_live(Announced::Files));
        assert!(!gate.nearby_live(Announced::Pairing));
        // To take files and to be paired with are two things: what was announced for files pairs with nobody.
        assert_ne!(announced.for_files().capability, announced.capability);
        assert_eq!(scan(&a, &b, &gate, announced.clone()).await, None);
        assert_eq!(
            scan(&a, &b, &gate, announced.for_files()).await,
            Some(Purpose::NearbyFiles)
        );
        assert!(
            !gate.nearby_live(Announced::Files),
            "what was used for files is announced on"
        );
        assert_eq!(
            scan(&a, &b, &gate, announced.for_files()).await,
            None,
            "used twice"
        );
        // And the other way: what was announced to pair by takes no files.
        let pairs = gate.create_nearby(&b, 100, Announced::Pairing).unwrap();
        assert_eq!(scan(&a, &b, &gate, pairs.for_files()).await, None);
        assert_eq!(scan(&a, &b, &gate, pairs).await, Some(Purpose::Pairing));
        // Made from the one for files again, it is nothing this device knows.
        let announced = gate.create_nearby(&b, 100, Announced::Files).unwrap();
        assert_eq!(
            scan(&a, &b, &gate, announced.for_files().for_files()).await,
            None
        );
        // A code shown on the screen has no capability for files beside it.
        let shown = gate.create(&b, 100).unwrap();
        assert_eq!(scan(&a, &b, &gate, shown.for_files()).await, None);
        // Taken back, it admits no one.
        gate.withdraw_nearby();
        assert_eq!(scan(&a, &b, &gate, announced.for_files()).await, None);
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

    #[tokio::test]
    async fn an_unused_files_code_is_taken_back_alone_and_a_scanned_or_older_one_is_not() {
        let (a, b) = two_hosts().await;
        let gate = Invitations::default();
        assert!(
            !gate.withdraw_files(0),
            "a code was taken back before any was made"
        );
        let (first, first_generation) = gate.create_for_files(&b, 100).unwrap();
        let pairing = gate.create(&b, 100).unwrap();
        let (second, second_generation) = gate.create_for_files(&b, 100).unwrap();
        assert!(
            !gate.withdraw_files(first_generation),
            "the generation of a replaced code took back the code that replaced it"
        );
        assert_eq!(gate.files_code(), Some((second_generation, true)));
        assert!(
            gate.withdraw_files(second_generation),
            "an unused code was not taken back"
        );
        assert_eq!(
            gate.files_code(),
            None,
            "a code that was taken back was still reported"
        );
        assert!(
            !gate.withdraw_files(second_generation),
            "a code was taken back twice"
        );
        for code in [first, second] {
            assert_eq!(
                scan(&a, &b, &gate, code).await,
                None,
                "a code that was taken back or replaced admitted a device"
            );
        }
        // A code that was scanned belongs to the device that scanned it.
        let (third, third_generation) = gate.create_for_files(&b, 100).unwrap();
        assert_eq!(scan(&a, &b, &gate, third).await, Some(Purpose::Files));
        assert!(
            !gate.withdraw_files(third_generation),
            "a scanned code was taken back"
        );
        assert_eq!(gate.files_code(), Some((third_generation, false)));
        assert_eq!(
            scan(&a, &b, &gate, pairing).await,
            Some(Purpose::Pairing),
            "taking back a files code took the pairing code with it"
        );
    }
}
