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
    assert_eq!(a.options(), b.options());
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
    let bc = b.choose(b.options()[1], [5; 32], 101).unwrap();
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
