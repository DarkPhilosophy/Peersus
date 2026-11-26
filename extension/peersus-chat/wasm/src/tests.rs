/// Writes the pages Chat itself renders (a conversation with voice messages and a thread, a recording under way, a stopped
/// recording) as JSON into `$CHAT_PAGE_DUMP`, for the application's headless window to draw (`peersus-ui`'s
/// `draws_extension_pages_from_files`). Run by hand: `CHAT_PAGE_DUMP=dir cargo test dump_pages -- --ignored`.
#[test]
#[ignore]
fn dump_pages() {
    use crate::ui::{Context, Draft, Features, Media, Page, Thumbs, VOICE_NAME};
    use peersus_api::{
        audio::Levels,
        elements::Event,
        extension_data::{Sealed, WallTime},
    };
    use std::collections::BTreeMap;
    let Some(dir) = std::env::var_os("CHAT_PAGE_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (token(1), token(2));
    let (mut left, mut right) = (Engine::default(), Engine::default());
    left.set_route(b.clone(), Route::Ready);
    right.set_route(a.clone(), Route::Ready);
    let wall = Some(WallTime {
        unix_minute: 29_000_000,
        utc_offset_minutes: 120,
    });
    (left.wall, right.wall) = (wall, wall);
    let voice = |seconds: u32, wave: &str| Body {
        kind: "audio".into(),
        text: None,
        attachments: vec![Attachment {
            name: VOICE_NAME.into(),
            media_type: "audio/wav".into(),
            bytes: 44 + 32_000 * seconds,
            sha256: token(u64::from(seconds) + 40),
            wave: Some(wave.into()),
        }],
        reply: None,
        sent: None,
    };
    let mut n = 0;
    let mut from_right = |right: &mut Engine, left: &mut Engine, body: Body| {
        n += 1;
        right
            .queue(RoomKey::Direct(a.clone()), token(4), body, None)
            .ok();
        send(right, left, &a, &b, n * 60_000, Policy::default());
    };
    from_right(&mut right, &mut left, body("Protocol Omega is in effect!"));
    let root = right.data.rows.values().next().unwrap().target();
    from_right(
        &mut right,
        &mut left,
        Body {
            reply: Some(root.clone()),
            ..body("What is that again?")
        },
    );
    from_right(&mut right, &mut left, body("voice from them"));
    left.queue(
        RoomKey::Direct(b.clone()),
        token(3),
        body("We'll discuss at the mission brief."),
        None,
    )
    .unwrap();
    left.queue(
        RoomKey::Direct(b.clone()),
        token(3),
        voice(12, "00135789abcdeedcba98765432123456789aa000"),
        Some("asset/mine".into()),
    )
    .unwrap();
    // The message from them is a voice message as this device holds it.
    for row in left.data.rows.values_mut() {
        if row.body.as_ref().and_then(|b| b.text.as_deref()) == Some("voice from them") {
            row.body = Some(voice(5, "0000002479bcdeb97a8cfedb9752000000000000"));
            row.asset = Some("asset/theirs".into());
        }
    }
    let (directory, thumbs) = (BTreeMap::new(), Thumbs::default());
    let context = Context {
        engine: &left,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            listen: true,
            record: true,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    let room = RoomKey::Direct(b.clone());
    for (name, width) in [("phone", 335_000), ("narrow", 412_000), ("wide", 1_100_000)] {
        let page = |state: &str| -> Page {
            let mut page = Page::default();
            let _ = page.event(
                context,
                Event::Size {
                    id: "width".into(),
                    x: 0,
                    y: 0,
                    width,
                    height: 900_000,
                },
            );
            page.open(room.clone());
            match state {
                "recording" => {
                    page.recording = Some((room.clone(), 0));
                    page.live = Some(Levels {
                        elapsed_ms: 3_400,
                        levels: (0..34).map(|i| ((i * 37) % 255) as u8).collect(),
                        bands: (0..32u32).map(|i| (40 + (i * 53) % 200) as u8).collect(),
                    });
                }
                "stopped" => {
                    page.draft = Some(Draft {
                        selected: Media::Audio,
                        media_type: Some("audio/wav"),
                        name: VOICE_NAME.into(),
                        stage: Sealed {
                            stage: 1,
                            bytes: 44 + 32_000 * 3,
                            sha256: token(9),
                        },
                        expires: u64::MAX,
                        wave: Some("000000000024689bdfffdb97653468abcb975300".into()),
                    });
                }
                _ => {}
            }
            page
        };
        for state in ["conversation", "recording", "stopped"] {
            let value = page(state).render(context, true);
            std::fs::write(
                dir.join(format!("chat-{state}-{name}.json")),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        }
    }
}

use super::{core::*, wire::*};
use peersus_api::extension_data::ScopeId;

fn token(n: u64) -> ScopeId {
    ScopeId::try_from(format!("{n:064x}")).unwrap()
}
fn body(text: &str) -> Body {
    Body::text(text.to_owned())
}
fn reserve(engine: &mut Engine, peer: &ScopeId, now: u64) -> Option<Offer> {
    engine.next(peer, now, token(750)).unwrap()
}
fn send(
    from: &mut Engine,
    to: &mut Engine,
    destination: &ScopeId,
    source: &ScopeId,
    now: u64,
    policy: Policy,
) -> Offer {
    let offer = reserve(from, destination, now).unwrap();
    let answer = to
        .receive(
            source,
            offer.packet.clone(),
            &offer.clear,
            now,
            policy,
            None,
        )
        .unwrap();
    from.answer(destination, offer.key.clone(), answer, now)
        .unwrap();
    offer
}

#[test]
fn a_message_queued_while_away_shows_when_it_was_written_and_a_wrong_clock_moves_nothing_far() {
    use peersus_api::extension_data::WallTime;
    let wall = |minute| WallTime {
        unix_minute: minute,
        utc_offset_minutes: 120,
    };
    let (a, b) = (token(1), token(2));
    let room = RoomKey::Direct(b.clone());
    let mut left = Engine::default();
    let mut right = Engine::default();
    left.set_route(b.clone(), Route::Ready);
    // Written at minute 29_000_000, delivered ten hours later.
    left.wall = Some(wall(29_000_000));
    left.queue(room.clone(), token(3), body("written early"), None)
        .unwrap();
    right.wall = Some(wall(29_000_600));
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    let shown = |engine: &Engine, text: &str| {
        engine
            .data
            .rows
            .values()
            .find(|row| row.body.as_ref().and_then(|b| b.text.as_deref()) == Some(text))
            .unwrap()
            .at
    };
    assert_eq!(shown(&left, "written early"), Some(wall(29_000_000)));
    assert_eq!(shown(&right, "written early"), Some(wall(29_000_000)));
    // A writer whose clock is a day ahead does not make a message from the future: it is shown when it arrived.
    left.wall = Some(wall(29_002_040));
    left.queue(room.clone(), token(3), body("from a fast clock"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 2, Policy::default());
    assert_eq!(shown(&right, "from a fast clock"), Some(wall(29_000_600)));
    // Without the date and time here, nothing is shown, whatever the writer said.
    right.wall = None;
    left.queue(room, token(3), body("no clock here"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 3, Policy::default());
    assert_eq!(shown(&right, "no clock here"), None);
}

#[test]
fn offline_queue_remains_after_restart_then_peer_ack_sets_sent_and_seen() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.queue(room.clone(), token(3), body("from the local outbox"), None)
        .unwrap();
    let bytes = serde_json::to_vec(&left.data).unwrap();
    let mut left = Engine::new(serde_json::from_slice(&bytes).unwrap());
    assert!(reserve(&mut left, &b, 0).is_none());
    let key = left.data.rows.values().next().unwrap().key.clone();
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Queued
    );
    left.set_route(b.clone(), Route::Ready);
    let mut right = Engine::default();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Sent
    );
    right.set_route(a.clone(), Route::Ready);
    let row = right
        .data
        .rows
        .values()
        .find(|row| row.key == key)
        .unwrap()
        .storage_key();
    right
        .observe_rows(&RoomKey::Direct(a.clone()), &[row], true, true)
        .unwrap();
    send(&mut right, &mut left, &a, &b, 2, Policy::default());
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Seen
    );
}

#[test]
fn duplicates_and_reordered_frames_do_not_resurrect_history() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    let first = left
        .queue(room.clone(), token(3), body("hello"), None)
        .unwrap();
    let original = send(&mut left, &mut right, &b, &a, 1, Policy::default());
    assert_eq!(
        right
            .receive(&a, original.packet.clone(), &[], 2, Policy::default(), None)
            .unwrap(),
        Verdict::Applied
    );
    assert_eq!(right.data.rows.len(), 1);
    left.delete(&room, &first, Delete::Everyone).unwrap();
    send(
        &mut left,
        &mut right,
        &b,
        &a,
        200,
        Policy {
            delete: true,
            clear: false,
        },
    );
    let restored = serde_json::from_slice(&serde_json::to_vec(&right.data).unwrap()).unwrap();
    let mut right = Engine::new(restored);
    assert_eq!(
        right
            .receive(&a, original.packet, &[], 201, Policy::default(), None)
            .unwrap(),
        Verdict::Stale
    );
    assert!(right.data.rows.values().next().unwrap().body.is_none());
}

#[test]
fn an_unsent_message_has_only_total_delete_and_never_dispatches() {
    let b = token(2);
    let room = RoomKey::Direct(b.clone());
    let mut chat = Engine::default();
    let key = chat
        .queue(room.clone(), token(3), body("queued"), None)
        .unwrap();
    assert_eq!(chat.options(&room, &key), &[Delete::Unsent]);
    assert!(chat.delete(&room, &key, Delete::Everyone).is_err());
    chat.delete(&room, &key, Delete::Unsent).unwrap();
    chat.set_route(b.clone(), Route::Ready);
    assert!(reserve(&mut chat, &b, 1).is_none());
    assert!(chat.data.rows.is_empty());
}

#[test]
fn a_receiver_may_decline_deletion_and_sender_retains_local_only_indicator() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    let id = left
        .queue(room.clone(), token(3), body("retain the remote copy"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    assert_eq!(left.options(&room, &id), &[Delete::Local, Delete::Everyone]);
    left.delete(&room, &id, Delete::Everyone).unwrap();
    let denied = send(&mut left, &mut right, &b, &a, 2, Policy::default());
    assert!(left.data.rows.values().next().unwrap().body.is_none());
    assert_eq!(
        left.data.rows.values().next().unwrap().removed[&b],
        Removal::LocalOnly
    );
    assert!(right.data.rows.values().next().unwrap().body.is_some());
    assert_eq!(
        right
            .receive(
                &a,
                denied.packet,
                &[],
                3,
                Policy {
                    delete: true,
                    clear: true
                },
                None
            )
            .unwrap(),
        Verdict::Declined
    );
}

#[test]
fn deleting_locally_does_not_emit_a_remote_request() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    let id = left
        .queue(room.clone(), token(3), body("local only"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    left.delete(&room, &id, Delete::Local).unwrap();
    assert!(reserve(&mut left, &b, 2).is_none());
    assert!(left.data.rows.values().next().unwrap().body.is_none());
    assert!(right.data.rows.values().next().unwrap().body.is_some());
}

#[test]
fn local_clear_emits_nothing_and_remote_clear_only_removes_named_shared_rows() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    let reverse = RoomKey::Direct(a.clone());
    left.set_route(b.clone(), Route::Ready);
    left.queue(room.clone(), token(3), body("keep remotely"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    left.clear(&room, false, false).unwrap();
    assert!(reserve(&mut left, &b, 2).is_none());
    assert_eq!(right.data.rows.len(), 1);
    left.queue(room.clone(), token(3), body("clear on both"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 3, Policy::default());
    right
        .queue(
            reverse,
            token(4),
            body("the requester never received this"),
            None,
        )
        .unwrap();
    left.clear(&room, true, false).unwrap();
    send(
        &mut left,
        &mut right,
        &b,
        &a,
        4,
        Policy {
            delete: false,
            clear: true,
        },
    );
    assert_eq!(right.data.rows.len(), 2);
    assert_eq!(left.data.rooms[&room].cleared[&b], Removal::Applied);
}

#[test]
fn total_conversation_purge_prunes_messages_and_existing_remote_requests() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    let id = left
        .queue(room.clone(), token(3), body("already sent"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    left.delete(&room, &id, Delete::Everyone).unwrap();
    left.queue(room.clone(), token(3), body("never deliver"), None)
        .unwrap();
    left.clear(&room, false, true).unwrap();
    assert!(left.data.rooms.is_empty());
    assert!(left.data.rows.is_empty());
    assert!(left.data.jobs.is_empty());
    assert!(reserve(&mut left, &b, 2).is_none());
}

#[test]
fn malformed_oversized_and_flooded_peer_input_is_not_stored() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    assert!(
        left.queue(room.clone(), token(3), body(&"x".repeat(1001)), None)
            .is_err()
    );
    left.set_route(b.clone(), Route::Ready);
    left.queue(room.clone(), token(3), body("one"), None)
        .unwrap();
    let offer = reserve(&mut left, &b, 1).unwrap();
    let mut wire = encode(&offer.packet, &[]).unwrap();
    wire.extend([1]);
    assert!(decode(&wire).is_err());
    for _ in 0..12 {
        let _ = right.receive(&a, offer.packet.clone(), &[], 1, Policy::default(), None);
    }
    assert_eq!(
        right.receive(&a, offer.packet, &[], 1, Policy::default(), None),
        Err(Fault::Rate)
    );
    assert_eq!(right.data.rows.len(), 1);
}

#[test]
fn group_definition_and_message_reach_two_recipients_independently() {
    let (a, b, c) = (token(1), token(2), token(3));
    let (sa, sb, sc, scope) = (token(11), token(12), token(13), token(50));
    let mut left = Engine::default();
    let mut middle = Engine::default();
    let mut right = Engine::default();
    let group = Group {
        id: scope.clone(),
        title: "Workshop".into(),
        creator: sa.clone(),
        roster: vec![
            Member {
                id: sa.clone(),
                label: "First".into(),
            },
            Member {
                id: sb.clone(),
                label: "Second".into(),
            },
            Member {
                id: sc.clone(),
                label: "Third".into(),
            },
        ],
    };
    left.set_group_routes(
        scope.clone(),
        sa.clone(),
        [(b.clone(), sb.clone()), (c.clone(), sc.clone())].into(),
    );
    middle.set_group_routes(
        scope.clone(),
        sb.clone(),
        [(a.clone(), sa.clone()), (c.clone(), sc.clone())].into(),
    );
    right.set_group_routes(
        scope.clone(),
        sc.clone(),
        [(a.clone(), sa.clone()), (b.clone(), sb.clone())].into(),
    );
    let room = left.create_group(group.clone(), sa).unwrap();
    let id = left
        .queue(room.clone(), token(101), body("one group message"), None)
        .unwrap();
    left.set_route(b.clone(), Route::Ready);
    left.set_route(c.clone(), Route::Offline);
    send(&mut left, &mut middle, &b, &a, 1, Policy::default());
    send(&mut left, &mut middle, &b, &a, 2, Policy::default());
    assert!(middle.data.rooms[&room].group.as_ref() == Some(&group));
    assert!(reserve(&mut left, &c, 3).is_none());
    let source = &left.data.rows.values().next().unwrap().delivery;
    assert_eq!(source[&sb], Delivery::Sent);
    assert_eq!(source[&sc], Delivery::Queued);
    left.set_route(c.clone(), Route::Ready);
    send(&mut left, &mut right, &c, &a, 4, Policy::default());
    send(&mut left, &mut right, &c, &a, 5, Policy::default());
    assert!(right.data.rooms[&room].group.as_ref() == Some(&group));
    assert_eq!(right.data.rows.values().next().unwrap().key, id);
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&sc],
        Delivery::Sent
    );
}

#[test]
fn only_a_foreground_visible_row_gets_an_automatic_seen_receipt() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(a.clone());
    left.set_route(b.clone(), Route::Ready);
    left.queue(RoomKey::Direct(b.clone()), token(3), body("visible"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    left.queue(
        RoomKey::Direct(b.clone()),
        token(3),
        body("outside viewport"),
        None,
    )
    .unwrap();
    send(&mut left, &mut right, &b, &a, 2, Policy::default());
    let key = right
        .data
        .rows
        .values()
        .find(|row| row.body.as_ref().and_then(|b| b.text.as_deref()) == Some("visible"))
        .unwrap()
        .storage_key();
    right
        .observe_rows(&room, std::slice::from_ref(&key), false, true)
        .unwrap();
    right
        .observe_rows(&room, std::slice::from_ref(&key), true, false)
        .unwrap();
    assert!(right.data.jobs.is_empty());
    right
        .observe_rows(
            &RoomKey::Direct(b.clone()),
            std::slice::from_ref(&key),
            true,
            true,
        )
        .unwrap();
    assert!(right.data.jobs.is_empty());
    right.observe_rows(&room, &[key], true, true).unwrap();
    assert_eq!(right.data.jobs.len(), 1);
    right.set_route(a.clone(), Route::Ready);
    send(&mut right, &mut left, &a, &b, 3, Policy::default());
    assert_eq!(
        left.data
            .rows
            .values()
            .filter(|row| row.delivery[&b] == Delivery::Seen)
            .count(),
        1
    );
}

#[test]
fn reactions_replace_only_the_actors_previous_emoji() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    right.set_route(a.clone(), Route::Ready);
    left.queue(room.clone(), token(3), body("react to this"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    let key_left = left.data.rows.values().next().unwrap().storage_key();
    let key_right = right.data.rows.values().next().unwrap().storage_key();
    left.react(&room, &key_left, Some("👍".into())).unwrap();
    send(&mut left, &mut right, &b, &a, 2, Policy::default());
    right
        .react(&RoomKey::Direct(a.clone()), &key_right, Some("❤️".into()))
        .unwrap();
    send(&mut right, &mut left, &a, &b, 3, Policy::default());
    left.react(&room, &key_left, None).unwrap();
    send(&mut left, &mut right, &b, &a, 4, Policy::default());
    let r = &right.data.rows.values().next().unwrap().reactions;
    assert_eq!(r["self"].emoji.as_deref(), Some("❤️"));
    assert_eq!(r["other"].emoji, None);
}

#[test]
fn a_declined_remote_clear_does_not_destroy_the_recipients_copy() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    left.queue(room.clone(), token(3), body("Keep the other copy"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());
    left.clear(&room, true, false).unwrap();
    send(
        &mut left,
        &mut right,
        &b,
        &a,
        2,
        Policy {
            delete: true,
            clear: false,
        },
    );
    assert!(left.data.rows.is_empty());
    assert_eq!(left.data.rooms[&room].cleared[&b], Removal::LocalOnly);
    assert_eq!(right.data.rows.len(), 1);
}

#[test]
fn an_attachment_is_not_delivered_by_its_header_or_chunk_acknowledgements() {
    let (a, b) = (token(1), token(2));
    let mut left = Engine::default();
    let mut right = Engine::default();
    let room = RoomKey::Direct(b.clone());
    left.set_route(b.clone(), Route::Ready);
    let file = Body {
        kind: "file".into(),
        text: None,
        attachments: vec![Attachment {
            name: "sample.txt".into(),
            media_type: "application/octet-stream".into(),
            bytes: 4,
            sha256: token(14),
            wave: None,
        }],
        reply: None,
        sent: None,
    };
    let id = left
        .queue(
            room.clone(),
            token(60),
            file.clone(),
            Some("assets/local".into()),
        )
        .unwrap();
    let first = send(&mut left, &mut right, &b, &a, 1, Policy::default());
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Unconfirmed
    );
    let second = reserve(&mut left, &b, 2).unwrap();
    let frame = encode(&second.packet, b"data").unwrap();
    assert!(matches!(
        decode(&frame).unwrap(),
        (
            Packet::Mutation {
                change: Op::Chunk { offset: 0, len: 4 },
                ..
            },
            b"data"
        )
    ));
    assert!(right.data.rows.is_empty());
    left.answer(&b, first.key.clone(), Verdict::Offset(4), 2)
        .unwrap();
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Unconfirmed
    );
    let third = reserve(&mut left, &b, 3).unwrap();
    assert!(matches!(
        third.packet,
        Packet::Mutation {
            change: Op::Finish,
            ..
        }
    ));
    let result = right
        .finish_attachment(
            &a,
            first.key.clone(),
            Scope::Direct,
            crate::core::IncomingMessage {
                id,
                order: 1,
                body: file,
            },
            3,
            "assets/remote".into(),
        )
        .unwrap();
    assert_eq!(result, Verdict::Applied);
    left.answer(&b, first.key, result, 3).unwrap();
    assert_eq!(
        left.data.rows.values().next().unwrap().delivery[&b],
        Delivery::Sent
    );
}

#[test]
fn a_grant_withdrawn_after_an_attempt_requires_an_explicit_retry() {
    let b = token(2);
    let mut engine = Engine::default();
    engine.set_route(b.clone(), Route::Ready);
    engine
        .queue(
            RoomKey::Direct(b.clone()),
            token(3),
            body("attempted"),
            None,
        )
        .unwrap();
    let attempt = reserve(&mut engine, &b, 1).unwrap();
    engine.hold_revoked(|_| false);
    let encoded = serde_json::to_vec(&engine.data).unwrap();
    let mut engine = Engine::new(serde_json::from_slice(&encoded).unwrap());
    engine.set_route(b.clone(), Route::Ready);
    assert!(reserve(&mut engine, &b, 2).is_none());
    engine.retry(&b);
    assert_eq!(reserve(&mut engine, &b, 3).unwrap().key, attempt.key);
}

#[test]
fn a_message_keeps_its_id_and_observer_while_its_menu_opens_over_it() {
    use crate::ui::{Context, Effect, Features, Page};
    use peersus_api::elements::{Event, Node, Observe};
    use std::collections::{BTreeMap, BTreeSet};

    fn inspect(node: &Node, ids: &mut BTreeSet<String>, buttons: &mut BTreeMap<String, String>) {
        let id = match node {
            Node::Box { id, .. } => id.as_ref(),
            Node::Button { id, label } => {
                buttons.insert(label.clone(), id.clone());
                Some(id)
            }
            Node::Interval { id, .. } | Node::Devices { id, .. } | Node::Field { id, .. } => {
                Some(id)
            }
            _ => None,
        };
        if let Some(id) = id {
            assert!(ids.insert(id.clone()), "duplicate element id: {id}");
        }
        match node {
            Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                for child in children {
                    inspect(child, ids, buttons);
                }
            }
            Node::Interval { child, .. } => inspect(child, ids, buttons),
            _ => {}
        }
    }
    fn observer(node: &Node) -> Option<&str> {
        match node {
            Node::Box { id, observe, .. } if observe.contains(&Observe::Visibility) => {
                assert!(observe.contains(&Observe::Press), "a message is pressed");
                id.as_deref()
            }
            Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                children.iter().find_map(observer)
            }
            Node::Interval { child, .. } => observer(child),
            _ => None,
        }
    }
    fn controls(page: &mut Page, context: Context<'_>) -> (Node, BTreeMap<String, String>) {
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let mut buttons = BTreeMap::new();
        inspect(&tree, &mut BTreeSet::new(), &mut buttons);
        (tree, buttons)
    }
    let press = |id: &str| Event::Press { id: id.to_owned() };
    let hold = |id: &str| Event::Hold { id: id.to_owned() };

    let (sender, recipient) = (token(1), token(2));
    let mut remote = Engine::default();
    remote.set_route(recipient.clone(), Route::Ready);
    remote
        .queue(
            RoomKey::Direct(recipient.clone()),
            token(3),
            body("hello"),
            None,
        )
        .unwrap();
    let mut engine = Engine::default();
    send(
        &mut remote,
        &mut engine,
        &recipient,
        &sender,
        1,
        Policy::default(),
    );
    let room = RoomKey::Direct(sender);
    let row = engine.data.rows.values().next().unwrap().storage_key();
    engine.react(&room, &row, Some("❤️".into())).unwrap();
    let directory = BTreeMap::new();
    let thumbs = crate::ui::Thumbs::default();
    let context = Context {
        engine: &engine,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    let mut page = Page::default();
    page.open(room.clone());
    let (first, _) = controls(&mut page, context);
    let message = observer(&first).unwrap().to_owned();

    // A tap on a message that is no photo opens nothing; a long press opens its menu over it.
    assert!(page.event(context, press(&message)).is_none());
    let (_, buttons) = controls(&mut page, context);
    assert!(!buttons.contains_key("Details"));
    assert!(page.event(context, hold(&message)).is_none());
    let (open, menu) = controls(&mut page, context);
    assert_eq!(
        observer(&open),
        Some(message.as_str()),
        "the message stays where it is behind the menu"
    );
    assert!(menu.contains_key("Details"));
    // My reaction is marked in the menu, and choosing it again takes it back.
    assert!(matches!(
        page.event(context, Event::Press { id: menu["[❤️]"].clone() }),
        Some(Effect::React(which, key, None)) if which == room && key == row
    ));
    let (closed, buttons) = controls(&mut page, context);
    assert!(!buttons.contains_key("Details"));
    assert_eq!(observer(&closed), Some(message.as_str()));

    page.go_inbox();
    controls(&mut page, context);
    page.open(room.clone());
    let (reopened, _) = controls(&mut page, context);
    assert_eq!(observer(&reopened), Some(message.as_str()));
    assert!(matches!(
        page.event(context, Event::Visibility { id: message, visible: true, foreground: true }),
        Some(Effect::Seen(which, key)) if which == room && key == row
    ));
}

#[test]
fn photo_pictures_are_kept_within_the_application_s_bytes_and_shown_within_the_bubble() {
    use crate::ui::{THUMB_EDGE, Thumb, Thumbs, shown};
    let mut thumbs = Thumbs::default();
    // A photo that could not be made is not kept, and takes no room.
    thumbs.made("assets/a/failed".into(), None);
    assert!(thumbs.get("assets/a/failed").is_none());
    let picture = |edge: u32| (edge as usize).pow(2) * 4;
    let reserved = picture(crate::ui::VIEW_EDGE) + picture(THUMB_EDGE);
    let room = (peersus_api::pictures::MAX_KEPT_BYTES - reserved) / picture(THUMB_EDGE);
    let mut gone = Vec::new();
    for n in 0..40 {
        // Room is made before each keep, so the application is never asked to keep past its bytes.
        gone.extend(thumbs.make_room());
        let id = thumbs.id();
        thumbs.made(
            format!("assets/a/{n}"),
            Some(Thumb {
                id,
                width: THUMB_EDGE as u16,
                height: THUMB_EDGE as u16,
            }),
        );
        let kept = (0..=n)
            .filter(|k| thumbs.get(&format!("assets/a/{k}")).is_some())
            .count();
        // With the opened photo and the chosen one beside them.
        assert!(kept * picture(THUMB_EDGE) + reserved <= peersus_api::pictures::MAX_KEPT_BYTES);
    }
    // The oldest were let go of, by the ids the application keeps them under, and the newest are still shown.
    assert_eq!(gone.len(), 40 - room);
    assert_eq!(gone.first().map(String::as_str), Some("photo/1"));
    assert!(thumbs.get("assets/a/0").is_none());
    assert_eq!(
        thumbs.get("assets/a/39").map(|t| t.id.as_str()),
        Some("photo/40")
    );
    assert_eq!(
        shown(&Thumb {
            id: "p".into(),
            width: 256,
            height: 192
        }),
        (220, 165)
    );
    assert_eq!(
        shown(&Thumb {
            id: "p".into(),
            width: 100,
            height: 256
        }),
        (100, 256)
    );
    assert_eq!(
        shown(&Thumb {
            id: "p".into(),
            width: 40,
            height: 30
        }),
        (40, 30)
    );
}

#[test]
fn a_group_message_s_menu_and_details_show_its_writer_s_profile_and_offer_to_write_to_them_alone() {
    use crate::ui::{Context, Effect, Features, Page, Peer, Thumbs};
    use peersus_api::elements::{Event, Node, Observe};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Seen {
        message: Option<String>,
        texts: Vec<String>,
        avatars: Vec<(String, u16)>,
        buttons: BTreeMap<String, String>,
    }
    fn walk(node: &Node, seen: &mut Seen) {
        match node {
            Node::Box {
                id,
                observe,
                children,
                ..
            } => {
                if observe.contains(&Observe::Visibility) && seen.message.is_none() {
                    seen.message.clone_from(id);
                }
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Column { children } | Node::Row { children } => {
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Interval { child, .. } => walk(child, seen),
            Node::Text { text, .. } => seen.texts.push(text.clone()),
            Node::Avatar { device, size, .. } => seen.avatars.push((device.clone(), *size)),
            Node::Button { id, label } => {
                seen.buttons.insert(label.clone(), id.clone());
            }
            _ => {}
        }
    }
    fn look(page: &mut Page, context: Context<'_>) -> Seen {
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let mut seen = Seen::default();
        walk(&tree, &mut seen);
        seen
    }

    // A group of three: the writer is known to the group as "First", and to this device as "Laptop".
    let (a, b, c) = (token(1), token(2), token(3));
    let (sa, sb, sc, scope) = (token(11), token(12), token(13), token(50));
    let group = Group {
        id: scope.clone(),
        title: "Workshop".into(),
        creator: sa.clone(),
        roster: vec![
            Member {
                id: sa.clone(),
                label: "First".into(),
            },
            Member {
                id: sb.clone(),
                label: "Second".into(),
            },
            Member {
                id: sc.clone(),
                label: "Third".into(),
            },
        ],
    };
    let mut left = Engine::default();
    let mut engine = Engine::default();
    left.set_group_routes(
        scope.clone(),
        sa.clone(),
        [(b.clone(), sb.clone()), (c.clone(), sc.clone())].into(),
    );
    engine.set_group_routes(scope.clone(), sb, [(a.clone(), sa.clone()), (c, sc)].into());
    let room = left.create_group(group, sa).unwrap();
    left.queue(room.clone(), token(101), body("hello group"), None)
        .unwrap();
    left.set_route(b.clone(), Route::Ready);
    send(&mut left, &mut engine, &b, &a, 1, Policy::default());
    send(&mut left, &mut engine, &b, &a, 2, Policy::default());
    let directory = BTreeMap::from([(
        a.clone(),
        Peer {
            alias: "laptop-alias".into(),
            name: "Laptop".into(),
            route: Route::Ready,
            sleeping: false,
            policy: Policy::default(),
            can_reply: true,
        },
    )]);
    let thumbs = Thumbs::default();
    let context = Context {
        engine: &engine,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    let mut page = Page::default();
    page.open(room.clone());
    let message = look(&mut page, context).message.unwrap();

    // A long press: the menu starts with who wrote it, with their picture.
    assert!(page.event(context, Event::Hold { id: message }).is_none());
    let menu = look(&mut page, context);
    assert!(
        menu.avatars.contains(&("laptop-alias".into(), 80)),
        "the writer's picture is missing from the menu: {:?}",
        menu.avatars
    );
    for line in [
        "First",
        "Laptop",
        "Online",
        "In Workshop",
        "1 group in common",
    ] {
        assert!(
            menu.texts.iter().any(|text| text == line),
            "{line:?} missing from {:?}",
            menu.texts
        );
    }

    // Details shows the same writer, picture and name, above what the bubble does not show.
    assert!(
        page.event(
            context,
            Event::Press {
                id: menu.buttons["Details"].clone()
            }
        )
        .is_none()
    );
    let details = look(&mut page, context);
    assert!(details.avatars.contains(&("laptop-alias".into(), 80)));
    assert!(details.texts.iter().any(|text| text == "First"));
    assert!(details.texts.iter().any(|text| text == "Message details"));

    // And from there, writing to them alone opens the conversation with their device.
    assert!(matches!(
        page.event(context, Event::Press { id: details.buttons["Write privately"].clone() }),
        Some(Effect::Open(RoomKey::Direct(peer))) if peer == a
    ));
}

#[test]
fn back_closes_what_is_open_then_leaves_the_conversation_and_my_own_message_shows_my_own_picture() {
    use crate::ui::{Context, Features, Page};
    use peersus_api::elements::{Event, Node, Observe, THIS_DEVICE};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Seen {
        pressed: Option<String>,
        back: Vec<String>,
        texts: Vec<String>,
        avatars: Vec<String>,
        buttons: BTreeMap<String, String>,
    }
    fn walk(node: &Node, seen: &mut Seen) {
        match node {
            Node::Box {
                id,
                observe,
                children,
                ..
            } => {
                // The first pressed box that is not a control drawn with a picture: a message.
                if observe.contains(&Observe::Press)
                    && seen.pressed.is_none()
                    && !children
                        .iter()
                        .any(|child| matches!(child, Node::Icon { .. }))
                {
                    seen.pressed.clone_from(id);
                }
                if observe.contains(&Observe::Back) {
                    seen.back.extend(id.clone());
                }
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Column { children } | Node::Row { children } => {
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Interval { child, .. } => walk(child, seen),
            Node::Text { text, .. } => seen.texts.push(text.clone()),
            Node::Avatar { device, .. } => seen.avatars.push(device.clone()),
            Node::Button { id, label } => {
                seen.buttons.insert(label.clone(), id.clone());
            }
            _ => {}
        }
    }
    fn look(page: &mut Page, context: Context<'_>) -> Seen {
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let mut seen = Seen::default();
        walk(&tree, &mut seen);
        seen
    }

    let peer = token(2);
    let room = RoomKey::Direct(peer.clone());
    let mut engine = Engine::default();
    engine.set_route(peer, Route::Ready);
    engine
        .queue(room.clone(), token(3), body("mine"), None)
        .unwrap();
    let (directory, thumbs) = (BTreeMap::new(), crate::ui::Thumbs::default());
    let context = |back| Context {
        engine: &engine,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            back,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    let mut page = Page::default();
    // On the list, Back is the application's: the page takes none.
    assert!(look(&mut page, context(true)).back.is_empty());
    page.open(room.clone());
    // An application without Back for pages is not asked to give one.
    assert!(look(&mut page, context(false)).back.is_empty());
    let open = look(&mut page, context(true));
    let [back] = open.back.as_slice() else {
        panic!("one box takes Back in a conversation: {:?}", open.back);
    };
    let back = back.clone();

    // My own message's menu shows my own picture, which the application draws, and says it is mine.
    let message = open.pressed.unwrap();
    assert!(
        page.event(context(true), Event::Hold { id: message })
            .is_none()
    );
    let menu = look(&mut page, context(true));
    assert!(
        menu.avatars.iter().any(|device| device == THIS_DEVICE),
        "{:?}",
        menu.avatars
    );
    assert!(menu.texts.iter().any(|text| text == "You"));
    assert!(menu.buttons.contains_key("Details"));

    // Back closes the menu and stays in the conversation, then leaves the conversation for the list.
    assert!(
        page.event(context(true), Event::Back { id: back })
            .is_none()
    );
    let closed = look(&mut page, context(true));
    assert!(!closed.buttons.contains_key("Details"));
    assert_eq!(page.room(), Some(&room));
    let back = closed.back[0].clone();
    assert!(
        page.event(context(true), Event::Back { id: back })
            .is_none()
    );
    assert_eq!(page.room(), None);
    assert!(look(&mut page, context(true)).back.is_empty());
}

#[test]
fn a_long_conversation_with_times_replies_and_voice_messages_is_cut_down_to_fit_never_refused() {
    use crate::ui::{Context, Features, Page, Thumbs, VOICE_NAME};
    use peersus_api::{elements::Node, extension_data::WallTime};
    use std::collections::BTreeMap;

    let (a, b) = (token(1), token(2));
    let (mut left, mut right) = (Engine::default(), Engine::default());
    left.set_route(b.clone(), Route::Ready);
    right.set_route(a.clone(), Route::Ready);
    let wall = Some(WallTime {
        unix_minute: 29_000_000,
        utc_offset_minutes: 120,
    });
    (left.wall, right.wall) = (wall, wall);
    for n in 0..120u64 {
        // A minute apart, well within the receiver's rate.
        let now = (n + 1) * 60_000;
        let mut message = body(&format!("message number {n} with some words to show"));
        // Every third message answers the one before it.
        if n % 3 == 2 {
            message.reply = right
                .data
                .rows
                .values()
                .max_by_key(|row| row.order)
                .map(|row| row.target());
        }
        right
            .queue(RoomKey::Direct(a.clone()), token(4), message, None)
            .unwrap();
        send(&mut right, &mut left, &a, &b, now, Policy::default());
    }
    // And voice messages among them, as this device holds them.
    for (i, row) in left
        .data
        .rows
        .values_mut()
        .enumerate()
        .filter(|(i, _)| i % 5 == 0)
    {
        row.body = Some(Body {
            kind: "audio".into(),
            text: None,
            attachments: vec![Attachment {
                name: VOICE_NAME.into(),
                media_type: "audio/wav".into(),
                bytes: 44 + 32_000 * (i as u32 % 9 + 1),
                sha256: token(9),
                wave: None,
            }],
            reply: None,
            sent: None,
        });
        row.asset = Some(format!("asset/{i}"));
    }
    let (directory, thumbs) = (BTreeMap::new(), Thumbs::default());
    let context = Context {
        engine: &left,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            listen: true,
            record: true,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    // On a phone and in a desktop window, where the list sits beside the conversation.
    for width in [400_000, 1_400_000] {
        let mut page = Page::default();
        let _ = page.event(
            context,
            peersus_api::elements::Event::Size {
                id: "width".into(),
                x: 0,
                y: 0,
                width,
                height: 900_000,
            },
        );
        page.open(RoomKey::Direct(b.clone()));
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let mut texts = Vec::new();
        tree.visit(&mut |node| {
            if let Node::Text { text, .. } = node {
                texts.push(text.clone());
            }
        });
        assert!(
            !texts.iter().any(|t| t.contains("too large")),
            "{width}: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t == "message number 119 with some words to show"),
            "{width}: {texts:?}"
        );
    }
}

#[test]
fn an_audio_message_this_device_holds_plays_in_its_bubble_and_shows_how_far_it_is() {
    use crate::ui::{Context, Effect, Features, Listening, Page, Thumbs};
    use peersus_api::elements::{Event, Node};
    use std::collections::BTreeMap;

    fn look(page: &mut Page, context: Context<'_>) -> (Vec<String>, BTreeMap<String, String>) {
        fn walk(node: &Node, texts: &mut Vec<String>, buttons: &mut BTreeMap<String, String>) {
            match node {
                Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                    children
                        .iter()
                        .for_each(|child| walk(child, texts, buttons));
                }
                Node::Interval { child, .. } => walk(child, texts, buttons),
                Node::Text { text, .. } => texts.push(text.clone()),
                Node::Button { id, label } => {
                    buttons.insert(label.clone(), id.clone());
                }
                _ => {}
            }
        }
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let (mut texts, mut buttons) = (Vec::new(), BTreeMap::new());
        walk(&tree, &mut texts, &mut buttons);
        (texts, buttons)
    }

    let (sender, recipient) = (token(1), token(2));
    let mut remote = Engine::default();
    remote.set_route(recipient.clone(), Route::Ready);
    remote
        .queue(
            RoomKey::Direct(recipient.clone()),
            token(3),
            body("x"),
            None,
        )
        .unwrap();
    let mut engine = Engine::default();
    send(
        &mut remote,
        &mut engine,
        &recipient,
        &sender,
        1,
        Policy::default(),
    );
    let room = RoomKey::Direct(sender);
    let row = engine.data.rows.values_mut().next().unwrap();
    let key = row.storage_key();
    row.body = Some(Body {
        kind: "audio".into(),
        text: None,
        attachments: vec![Attachment {
            name: "voice.ogg".into(),
            media_type: "audio/ogg".into(),
            bytes: 2048,
            sha256: token(9),
            wave: None,
        }],
        reply: None,
        sent: None,
    });
    row.asset = Some("asset/voice".into());
    let (directory, thumbs) = (BTreeMap::new(), Thumbs::default());
    let context = |listen, listening| Context {
        engine: &engine,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            listen,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening,
    };
    let mut page = Page::default();
    page.open(room.clone());

    // A process with no decoder shows the file as a file.
    assert!(!look(&mut page, context(false, None)).1.contains_key("Play"));

    // A chosen sound keeps its name above and shows its size until it plays.
    let (texts, buttons) = look(&mut page, context(true, None));
    assert!(
        texts.iter().any(|t| t == "voice.ogg") && texts.iter().any(|t| t == "2 KiB"),
        "{texts:?}"
    );
    let play = buttons["Play"].clone();
    match page.event(context(true, None), Event::Press { id: play }) {
        Some(Effect::Listen(which, row)) => assert_eq!((which, row), (room.clone(), key.clone())),
        other => panic!("pressing play did {other:?}"),
    }

    let heard = Listening {
        row: key,
        playing: true,
        position_ms: 65_400,
        length_ms: Some(125_000),
        decoder: 1,
        decoded_all: false,
        asked: 0,
    };
    let (texts, buttons) = look(&mut page, context(true, Some(&heard)));
    assert!(buttons.contains_key("Pause") && !buttons.contains_key("Play"));
    assert!(texts.iter().any(|t| t == "1:05"), "{texts:?}");
}

#[test]
fn a_recorded_voice_message_carries_the_waveform_it_was_heard_with_and_fills_it_as_it_plays() {
    use peersus_api::elements::Node;
    let file = |bytes| Attachment {
        name: crate::ui::VOICE_NAME.into(),
        media_type: "audio/wav".into(),
        bytes,
        sha256: token(9),
        wave: None,
    };
    // 44 bytes of header and 7 seconds of one 16-bit channel at 16 kHz.
    assert_eq!(
        crate::ui::recorded_length(&file(44 + 7 * 32_000)),
        Some(7_000)
    );
    let chosen = Attachment {
        name: "song.wav".into(),
        ..file(44 + 7 * 32_000)
    };
    assert_eq!(crate::ui::recorded_length(&chosen), None);

    // The writer's levels, loud in the middle, become forty digits: the loudest of each part.
    let levels: Vec<u8> = (0..80)
        .map(|i| if (30..50).contains(&i) { 255 } else { 0 })
        .collect();
    let wave = crate::wire::wave(&levels).unwrap();
    assert_eq!(wave.len(), crate::wire::WAVE_BARS);
    assert_eq!(&wave[..15], "000000000000000");
    assert_eq!(&wave[15..25], "ffffffffff");
    assert_eq!(crate::wire::wave(&[]), None);
    // Only forty hexadecimal digits are a waveform a message may carry.
    let with = |wave: &str| Body {
        kind: "audio".into(),
        text: None,
        attachments: vec![Attachment {
            wave: Some(wave.into()),
            ..file(64)
        }],
        reply: None,
        sent: None,
    };
    assert!(with(&wave).valid());
    assert!(!with("f").valid());
    assert!(!with(&"g".repeat(crate::wire::WAVE_BARS)).valid());

    // Silence is the shortest bar, a loud step a tall one, and the places not yet played are faded.
    let places = |node: &Node| {
        let mut found = Vec::new();
        node.visit(&mut |n| {
            if let Node::Box {
                style, children, ..
            } = n
                && children.is_empty()
            {
                found.push((style.height, style.opacity.is_some()));
            }
        });
        found
    };
    use peersus_api::elements::Length::Px;
    let drawn = places(&crate::ui::spectrum(&[0, 15, 8], Some(1)));
    assert_eq!(drawn, [(Px(6), false), (Px(32), true), (Px(19), true)]);
    assert!(
        places(&crate::ui::spectrum(&[15; 3], None))
            .iter()
            .all(|(_, faded)| !faded)
    );

    // A message with no waveform gets one measured from the WAV this device holds: silence, then a loud half.
    let mut heard = Vec::new();
    heard.extend(std::iter::repeat_n(0i16, 800));
    heard.extend(std::iter::repeat_n(i16::MAX, 800));
    let measured = crate::ui::measure_wav(&crate::sdk::audio::wav(&heard)).unwrap();
    assert_eq!(measured.len(), crate::wire::WAVE_BARS);
    assert_eq!(&measured[..20], "0".repeat(20));
    assert_eq!(&measured[20..], "f".repeat(20));
    assert_eq!(crate::ui::measure_wav(b"not a sound"), None);
    assert_eq!(crate::ui::measure_wav(&crate::sdk::audio::wav(&[])), None);
    let mut waves = crate::ui::Waves::default();
    waves.made("asset/one".into(), Some(measured.clone()));
    waves.made("asset/two".into(), None);
    assert_eq!(waves.get("asset/one"), Some(measured.as_str()));
    assert_eq!(waves.get("asset/two"), None);
}

/// Run by hand (`CHAT_WAV=file cargo test measures_a_file -- --ignored --nocapture`): the waveform measured from a real file.
#[test]
#[ignore]
fn measures_a_file() {
    let Some(path) = std::env::var_os("CHAT_WAV") else {
        return;
    };
    println!(
        "wave {:?}",
        crate::ui::measure_wav(&std::fs::read(path).unwrap())
    );
}

#[test]
fn a_conversation_s_notification_word_names_it_back_and_is_an_identifier() {
    for room in [RoomKey::Direct(token(5)), RoomKey::Group(token(6))] {
        let word = room.namespace();
        assert!(peersus_api::name::valid(&word), "{word}");
        assert_eq!(RoomKey::try_from(word).ok(), Some(room));
    }
    assert!(RoomKey::try_from("room.7".to_owned()).is_err());
}

#[test]
fn a_reply_names_the_message_it_answers_and_both_devices_find_it_from_either_side() {
    fn row<'e>(engine: &'e Engine, text: &str) -> &'e Row {
        engine
            .data
            .rows
            .values()
            .find(|row| row.body.as_ref().and_then(|b| b.text.as_deref()) == Some(text))
            .unwrap()
    }
    fn answered<'e>(engine: &'e Engine, text: &str) -> Option<&'e str> {
        engine
            .replied(row(engine, text))
            .and_then(|row| row.body.as_ref()?.text.as_deref())
    }
    let (a, b) = (token(1), token(2));
    let (mut left, mut right) = (Engine::default(), Engine::default());
    left.set_route(b.clone(), Route::Ready);
    right.set_route(a.clone(), Route::Ready);
    let (to_right, to_left) = (RoomKey::Direct(b.clone()), RoomKey::Direct(a.clone()));
    left.queue(to_right.clone(), token(3), body("question"), None)
        .unwrap();
    send(&mut left, &mut right, &b, &a, 1, Policy::default());

    // The other device answers my message.
    let reply = Body {
        reply: Some(row(&right, "question").target()),
        ..body("answer")
    };
    right.queue(to_left, token(4), reply, None).unwrap();
    send(&mut right, &mut left, &a, &b, 2, Policy::default());
    assert_eq!(answered(&right, "answer"), Some("question"));
    assert_eq!(answered(&left, "answer"), Some("question"));

    // I answer my own message.
    let reply = Body {
        reply: Some(row(&left, "question").target()),
        ..body("and more")
    };
    left.queue(to_right, token(3), reply, None).unwrap();
    send(&mut left, &mut right, &b, &a, 3, Policy::default());
    assert_eq!(answered(&left, "and more"), Some("question"));
    assert_eq!(answered(&right, "and more"), Some("question"));

    // A reply that names no message is refused.
    let mut broken = body("broken");
    broken.reply = Some(Target {
        actor: Actor::Sender,
        id: Id {
            epoch: token(3),
            no: 0,
        },
    });
    assert!(!broken.valid());
}

#[test]
fn replies_sit_in_the_thread_of_the_message_they_answer_and_the_menu_starts_one() {
    use crate::ui::{Context, Features, Page, Thumbs};
    use peersus_api::elements::{Event, Node, Observe};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Seen {
        message: Option<String>,
        texts: Vec<String>,
        buttons: BTreeMap<String, String>,
    }
    fn walk(node: &Node, seen: &mut Seen) {
        match node {
            Node::Box {
                id,
                observe,
                children,
                ..
            } => {
                if observe.contains(&Observe::Visibility) && seen.message.is_none() {
                    seen.message.clone_from(id);
                }
                // A pressed box drawn with a picture is a control: its words, then the picture's name.
                if observe.contains(&Observe::Press)
                    && let Some(id) = id
                    && let Some(Node::Icon { name, .. }) = children
                        .iter()
                        .find(|child| matches!(child, Node::Icon { .. }))
                    && let Some(Node::Text { text, .. }) = children.first()
                {
                    seen.buttons.insert(format!("{text} {name}"), id.clone());
                }
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Column { children } | Node::Row { children } => {
                children.iter().for_each(|child| walk(child, seen));
            }
            Node::Interval { child, .. } => walk(child, seen),
            Node::Text { text, .. } => seen.texts.push(text.clone()),
            Node::Button { id, label } => {
                seen.buttons.insert(label.clone(), id.clone());
            }
            _ => {}
        }
    }
    fn look(page: &mut Page, context: Context<'_>) -> Seen {
        let tree = Node::parse(&page.render(context, true)).unwrap();
        tree.validate().unwrap();
        let mut seen = Seen::default();
        walk(&tree, &mut seen);
        seen
    }

    // My question, then a later message of mine, then four answers from the other device: the first to the question, the
    // others to that answer (a reply to a reply is in the same thread).
    let (a, b) = (token(1), token(2));
    let (mut left, mut right) = (Engine::default(), Engine::default());
    left.set_route(b.clone(), Route::Ready);
    right.set_route(a.clone(), Route::Ready);
    for (n, text) in ["question", "later"].into_iter().enumerate() {
        left.queue(RoomKey::Direct(b.clone()), token(3), body(text), None)
            .unwrap();
        send(
            &mut left,
            &mut right,
            &b,
            &a,
            n as u64 + 1,
            Policy::default(),
        );
    }
    let mut answered = "question";
    for (n, text) in ["answer", "second", "third", "fourth"]
        .into_iter()
        .enumerate()
    {
        let target = right
            .data
            .rows
            .values()
            .find(|row| row.body.as_ref().and_then(|b| b.text.as_deref()) == Some(answered))
            .unwrap()
            .target();
        let reply = Body {
            reply: Some(target),
            ..body(text)
        };
        right
            .queue(RoomKey::Direct(a.clone()), token(4), reply, None)
            .unwrap();
        send(
            &mut right,
            &mut left,
            &a,
            &b,
            n as u64 + 10,
            Policy::default(),
        );
        answered = "answer";
    }

    let (directory, thumbs) = (BTreeMap::new(), Thumbs::default());
    let context = Context {
        engine: &left,
        directory: &directory,
        locale: Default::default(),
        features: Features {
            visibility: true,
            press: true,
            ..Features::default()
        },
        thumbs: &thumbs,
        waves: &crate::ui::NO_WAVES,
        listening: None,
    };
    let mut page = Page::default();
    page.open(RoomKey::Direct(b.clone()));

    // The question comes after my later message, as recent as its newest reply, with its thread under it: "4 replies",
    // the replies in order, and "Add a reply…". The three that answer the first reply quote it; the first answers the
    // question right above and quotes nothing.
    let seen = look(&mut page, context);
    let at = |text: &str| seen.texts.iter().position(|t| t == text);
    let answer = seen.texts.iter().filter(|t| *t == "answer").count();
    assert!(
        at("later").is_some()
            && at("later") < at("question")
            && at("question") < at("answer")
            && at("answer") < at("second")
            && at("second") < at("third")
            && at("third") < at("fourth")
            && at("fourth") < at("Add a reply…")
            && answer == 4
            && at("You").is_none(),
        "{:?}",
        seen.texts
    );
    // Folding leaves only the count under the question; opening shows the thread again.
    let fold = seen.buttons["4 replies chevron-up"].clone();
    assert!(page.event(context, Event::Press { id: fold }).is_none());
    let folded = look(&mut page, context);
    assert!(
        !folded
            .texts
            .iter()
            .any(|t| t == "second" || t == "Add a reply…"),
        "{:?}",
        folded.texts
    );
    let open = folded.buttons["4 replies chevron-down"].clone();
    assert!(page.event(context, Event::Press { id: open }).is_none());
    assert!(look(&mut page, context).texts.iter().any(|t| t == "fourth"));

    // A long press offers to reply; the composer then shows what is answered.
    assert!(
        page.event(
            context,
            Event::Hold {
                id: seen.message.unwrap()
            }
        )
        .is_none()
    );
    let reply = look(&mut page, context).buttons["Reply"].clone();
    assert!(page.event(context, Event::Press { id: reply }).is_none());
    let composing = look(&mut page, context);
    assert!(composing.texts.iter().any(|t| t == "Replying to"));

    // Cancel sends the next message without the quote.
    let cancel = composing.buttons["Cancel"].clone();
    assert!(page.event(context, Event::Press { id: cancel }).is_none());
    assert!(page.replying.is_none());
    assert!(
        !look(&mut page, context)
            .texts
            .iter()
            .any(|t| t == "Replying to")
    );

    // A message sent while replying ends the reply.
    page.replying = Some(
        left.data
            .rows
            .values()
            .find(|r| !r.mine)
            .unwrap()
            .storage_key(),
    );
    assert!(
        look(&mut page, context)
            .texts
            .iter()
            .any(|t| t == "Replying to")
    );
    page.submitted(Ok(()));
    assert!(page.replying.is_none());
}
