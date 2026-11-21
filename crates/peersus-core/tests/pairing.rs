use peersus_core::pairing::{PairingError, PairingSession, Role};

fn sessions() -> (PairingSession, PairingSession) {
    let a =
        PairingSession::new([7; 32], [1; 32], [2; 32], [3; 32], Role::Initiator, 100, 60).unwrap();
    let b =
        PairingSession::new([7; 32], [1; 32], [2; 32], [3; 32], Role::Responder, 100, 60).unwrap();
    (a, b)
}

#[test]
fn trust_requires_matching_choices_and_mutual_ready_proofs() {
    let (mut a, mut b) = sessions();
    let (shown_a, shown_b) = (a.options(), b.options());
    assert!(shown_a.iter().all(|number| shown_b.contains(number)));
    assert!(
        (0..3).all(|place| shown_a[place] != shown_b[place]),
        "no number is at the same place on both devices"
    );
    let number = a.options()[0];
    let ac = a.choose(number, [4; 32], 101).unwrap();
    let bc = b.choose(number, [5; 32], 101).unwrap();
    assert!(a.finish(101).is_err());
    a.receive_commitment(bc, 102).unwrap();
    b.receive_commitment(ac, 102).unwrap();
    let ar = a.reveal(103).unwrap();
    let br = b.reveal(103).unwrap();
    let ap = a.receive_reveal(br, 104).unwrap();
    let bp = b.receive_reveal(ar, 104).unwrap();
    assert!(a.finish(104).is_err());
    a.receive_ready(bp, 105).unwrap();
    b.receive_ready(ap, 105).unwrap();
    assert_eq!(a.finish(106).unwrap().peer_id(), &[2; 32]);
    assert_eq!(b.finish(106).unwrap().peer_id(), &[1; 32]);
}

#[test]
fn mismatched_choices_permanently_abort_pairing() {
    let (mut a, mut b) = sessions();
    let ac = a.choose(a.options()[0], [4; 32], 101).unwrap();
    let bc = b.choose(a.options()[1], [5; 32], 101).unwrap();
    a.receive_commitment(bc, 102).unwrap();
    b.receive_commitment(ac, 102).unwrap();
    a.reveal(103).unwrap();
    let br = b.reveal(103).unwrap();
    assert_eq!(
        a.receive_reveal(br, 104).unwrap_err(),
        PairingError::Mismatch
    );
    assert_eq!(a.finish(104).unwrap_err(), PairingError::Aborted);
}

#[test]
fn selection_is_not_revealed_before_remote_commitment() {
    let (mut a, _) = sessions();
    a.choose(a.options()[0], [4; 32], 101).unwrap();
    assert_eq!(a.reveal(102).unwrap_err(), PairingError::NotReady);
}

#[test]
fn expired_pairing_cannot_be_resumed() {
    let (mut a, _) = sessions();
    assert_eq!(
        a.choose(a.options()[0], [4; 32], 160).unwrap_err(),
        PairingError::Expired
    );
    assert_eq!(
        a.choose(a.options()[0], [4; 32], 101).unwrap_err(),
        PairingError::Aborted
    );
}

#[test]
fn a_different_session_cannot_reuse_commitments() {
    let (mut a, _) = sessions();
    let mut b =
        PairingSession::new([8; 32], [1; 32], [2; 32], [3; 32], Role::Responder, 100, 60).unwrap();
    let ac = a.choose(a.options()[0], [4; 32], 101).unwrap();
    let bc = b.choose(b.options()[0], [5; 32], 101).unwrap();
    a.receive_commitment(bc, 102).unwrap();
    b.receive_commitment(ac, 102).unwrap();
    a.reveal(103).unwrap();
    assert_eq!(
        a.receive_reveal(b.reveal(103).unwrap(), 104).unwrap_err(),
        PairingError::InvalidProof
    );
}

/// Pressing the number at the same place on both devices never pairs, whatever the session: the two orders differ at every
/// place, by one turn in some sessions and by two in others.
#[test]
fn the_same_place_on_both_devices_is_never_the_same_number() {
    let mut turns = std::collections::BTreeSet::new();
    for session in 0..=255u8 {
        let key = [session; 32];
        let new =
            |role| PairingSession::new(key, [1; 32], [2; 32], [3; 32], role, 100, 60).unwrap();
        let (mut a, mut b) = (new(Role::Initiator), new(Role::Responder));
        let (shown_a, shown_b) = (a.options(), b.options());
        let turn = (1..3)
            .find(|turn| (0..3).all(|place| shown_b[place] == shown_a[(place + turn) % 3]))
            .expect("the responder shows the initiator's numbers turned by one or two");
        turns.insert(turn);
        for place in 0..3 {
            assert_ne!(shown_a[place], shown_b[place]);
        }
        let ac = a.choose(shown_a[0], [4; 32], 101).unwrap();
        let bc = b.choose(shown_b[0], [5; 32], 101).unwrap();
        a.receive_commitment(bc, 102).unwrap();
        b.receive_commitment(ac, 102).unwrap();
        a.reveal(103).unwrap();
        assert_eq!(
            a.receive_reveal(b.reveal(103).unwrap(), 104).unwrap_err(),
            PairingError::Mismatch
        );
    }
    assert_eq!(turns.len(), 2, "both turns occur over many sessions");
}
