//! Own reader tokens and removal that locks a device out (M10, part 1).

use std::sync::atomic::Ordering;

use lz_core::store::{ReadAccess, Store};
use lz_core::wire::{MailboxId, Token};
use tokio::sync::mpsc;

use super::devices::{family, join};
use super::{Person, RELAY_A, RELAY_B, TOKEN_A, TOKEN_B, World, expect_err, paired, url};
use crate::relay::{ReaderChange, ReaderId, Relay, RelayError, ServerFrame};
use crate::show::Notice;

/// What the relay answers to one `subscribe`.
async fn subscribe(world: &World, mailbox: &MailboxId, token: &Token, reader: &ReaderId) -> String {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut connection = world.relay.connect(&url(RELAY_A), tx).await.unwrap();
    connection.subscribe(mailbox, token, reader).unwrap();
    match rx.recv().await.unwrap().frame {
        ServerFrame::Subscribed { .. } => "subscribed".into(),
        ServerFrame::Error { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

fn unauthorized(result: Result<(), RelayError>) {
    assert!(
        matches!(result, Err(RelayError::Refused { status: 401, .. })),
        "{result:?}"
    );
}

fn bert_mailbox(person: &Person) -> MailboxId {
    person.contact("Bert").state.contact.own_mailbox
}

#[tokio::test]
async fn devices_take_own_tokens_and_hand_out_enrolment_tokens() {
    let world = World::new();
    let (anna, bert) = paired(&world).await;
    // Both created their mailboxes in the old form and took their own
    // token at once.
    let contact = anna.contact("Bert");
    let mailbox = contact.state.contact.own_mailbox;
    let reader = anna.device().reader_id;
    let own = contact.own_token.unwrap();
    assert_ne!(own, contact.read_token);
    assert!(world.relay.has_shared_token(RELAY_A, &mailbox));
    assert!(world.relay.has_own_token(RELAY_A, &mailbox, &reader));
    assert!(bert.contact("Anna").own_token.is_some());
    // The shared token no longer reads as Anna.
    assert_eq!(
        subscribe(&world, &mailbox, &contact.read_token, &reader).await,
        "unauthorized"
    );
    assert_eq!(
        subscribe(&world, &mailbox, &own, &reader).await,
        "subscribed"
    );

    // A device added later gets enrolment tokens and replaces them at
    // once; the sync mailbox has no shared token at all.
    let (phone, laptop, _bert) = family(&world).await;
    let group = phone.device().group.unwrap();
    assert!(!world.relay.has_shared_token(RELAY_A, &group.mailbox));
    assert_eq!(group.own_token, Some(group.read_token));
    let mailbox = bert_mailbox(&phone);
    let enrolled = laptop.contact("Bert");
    let laptop_reader = laptop.device().reader_id;
    let laptop_own = enrolled.own_token.unwrap();
    assert!(enrolled.new_token.is_none());
    assert_ne!(enrolled.read_token, phone.contact("Bert").token());
    assert_ne!(enrolled.read_token, phone.contact("Bert").read_token);
    // The enrolment token is void; only the laptop knows its token.
    let put = world
        .relay
        .put_reader(
            &url(RELAY_A),
            &mailbox,
            &enrolled.read_token,
            &laptop_reader,
            ReaderChange::push(None),
        )
        .await;
    unauthorized(put);
    assert_eq!(
        subscribe(&world, &mailbox, &laptop_own, &laptop_reader).await,
        "subscribed"
    );
    let laptop_group = laptop.device().group.unwrap();
    assert!(laptop_group.own_token.is_some());
    assert_ne!(laptop_group.token(), group.token());
}

/// The phone removes the laptop; `thief` keeps a copy of the laptop's
/// state, as someone who took it could.
async fn removed_with_copy(world: &World) -> (Person, Person, Person, lz_core::store::Device) {
    let (mut phone, thief, mut bert) = family(world).await;
    // A request open at the moment of removal.
    bert.ctx.request("Anna").await.unwrap();
    phone.sync().await;
    let copy = thief.device();
    let removal = phone.ctx.remove_device("Laptop").await.unwrap();
    assert!(removal.weak.is_empty(), "{removal:?}");
    assert_eq!(removal.hidden, 0);
    assert_eq!(removal.open_requests, ["Bert"]);
    (phone, thief, bert, copy)
}

#[tokio::test]
async fn a_copy_of_a_removed_device_is_locked_out() {
    let world = World::new();
    let (mut phone, mut thief, mut bert, copy) = removed_with_copy(&world).await;
    let mailbox = bert_mailbox(&phone);
    let contact = copy.contact_by_mailbox(&mailbox).unwrap();
    assert!(!world.relay.has_reader(RELAY_A, &mailbox, &copy.reader_id));
    assert!(!world.relay.has_shared_token(RELAY_A, &mailbox));
    let out = phone.out.take();
    assert!(out.contains("Anfragen von Bert waren offen"), "{out}");

    // With none of the tokens it holds can the copy read the mailbox,
    // register itself again, or delete it.
    let group = copy.group.clone().unwrap();
    let tokens = [
        contact.read_token,
        contact.own_token.unwrap(),
        group.read_token,
        group.own_token.unwrap(),
    ];
    for token in &tokens {
        assert_eq!(
            subscribe(&world, &mailbox, token, &copy.reader_id).await,
            "unknown_reader"
        );
        for reader in [copy.reader_id, [0x77; 16]] {
            let change = ReaderChange {
                token: Some(&[0x78; 32]),
                ..ReaderChange::default()
            };
            let put = world
                .relay
                .put_reader(&url(RELAY_A), &mailbox, token, &reader, change)
                .await;
            unauthorized(put);
        }
        let deleted = world
            .relay
            .delete_mailbox(&url(RELAY_A), &mailbox, token)
            .await;
        unauthorized(deleted);
    }
    assert!(world.relay.has_mailbox(RELAY_A, &mailbox));

    // A new request reaches the phone only; the copy cannot answer it.
    phone.ctx.respond("Bert").await.unwrap();
    bert.sync().await;
    world.clock.fetch_add(7 * 3600, Ordering::SeqCst);
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(
        phone.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
    let copy_path = thief.ctx.store.path().to_owned();
    thief
        .ctx
        .store
        .update(|device| {
            *device = copy.clone();
            Ok::<_, std::io::Error>(())
        })
        .unwrap();
    expect_err(thief.ctx.respond("Bert").await, "Keine offene Anfrage");

    // Unpatched, it learns that it was removed and deletes its state.
    let notices = thief.ctx.sync().await.unwrap();
    assert!(notices.contains(&Notice::Removed), "{notices:?}");
    assert!(!copy_path.exists());
}

#[tokio::test]
async fn readers_planted_before_removal_are_revoked() {
    let world = World::new();
    let (mut phone, laptop, _bert) = family(&world).await;
    let mailbox = bert_mailbox(&phone);
    let stolen = laptop.contact("Bert");
    // Before it is removed, the laptop plants two readers: one with a token
    // of its choice, one with the shared token, which it knows from the
    // pairing (`contact_part`) or, here, not at all: so with its own.
    let planted: [ReaderId; 2] = [[0x51; 16], [0x52; 16]];
    let tokens: [Token; 2] = [[0x61; 32], [0x62; 32]];
    for (reader, token) in planted.iter().zip(&tokens) {
        let change = ReaderChange {
            token: Some(token),
            ..ReaderChange::default()
        };
        world
            .relay
            .put_reader(&url(RELAY_A), &mailbox, &stolen.token(), reader, change)
            .await
            .unwrap();
    }
    let removal = phone.ctx.remove_device("Laptop").await.unwrap();
    assert_eq!(removal.hidden, 2);
    let out = phone.out.take();
    assert!(out.contains("2 weitere Leser"), "{out}");
    let readers = world.relay.readers(RELAY_A, &mailbox);
    assert_eq!(readers, [phone.device().reader_id]);
    for (reader, token) in planted.iter().zip(&tokens) {
        assert_eq!(
            subscribe(&world, &mailbox, token, reader).await,
            "unknown_reader"
        );
        let again = world
            .relay
            .put_reader(
                &url(RELAY_A),
                &mailbox,
                token,
                reader,
                ReaderChange::push(None),
            )
            .await;
        unauthorized(again);
    }
}

#[tokio::test]
async fn devices_that_stay_keep_reading_after_a_removal() {
    let world = World::new();
    let (mut phone, _laptop, mut bert) = family(&world).await;
    let mut tablet = super::devices::add_tablet(&world, &mut phone).await;
    let removal = phone.ctx.remove_device("Laptop").await.unwrap();
    assert_eq!(removal.hidden, 0);
    // The tablet leaves the ended group, keeps its own token and reads on.
    let notices = tablet.sync().await;
    assert!(
        notices.iter().any(|n| matches!(n, Notice::Problem(_))),
        "{notices:?}"
    );
    assert!(tablet.device().group.is_none());
    bert.ctx.request("Anna").await.unwrap();
    for person in [&mut phone, &mut tablet] {
        assert_eq!(
            person.sync().await,
            vec![Notice::Request {
                name: "Bert".into()
            }]
        );
    }
}

#[tokio::test]
async fn replaced_token_ends_the_subscription_and_it_subscribes_again() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    anna.ctx.idle = std::time::Duration::from_millis(300);
    let mailbox = bert_mailbox(&anna);
    let reader = anna.device().reader_id;
    let store = anna.ctx.store.clone();
    let relay = world.relay.clone();
    let replacement: Token = [0x44; 32];

    let fetch = anna.ctx.sync();
    let meanwhile = async {
        while !relay.subscribed(RELAY_A, &mailbox, &reader) {
            tokio::task::yield_now().await;
        }
        // Another process of the same device replaces its token, as a
        // device does with an enrolment token; the subscription with the
        // old one ends.
        let old = store
            .load()
            .unwrap()
            .contact_by_mailbox(&mailbox)
            .unwrap()
            .token();
        let change = ReaderChange {
            token: Some(&replacement),
            ..ReaderChange::default()
        };
        relay
            .put_reader(&url(RELAY_A), &mailbox, &old, &reader, change)
            .await
            .unwrap();
        store
            .update(|device| {
                device.contact_by_mailbox_mut(&mailbox).unwrap().own_token = Some(replacement);
                Ok::<_, std::io::Error>(())
            })
            .unwrap();
        bert.ctx.request("Anna").await.unwrap();
    };
    let (notices, ()) = tokio::join!(fetch, meanwhile);
    let notices: Vec<_> = notices
        .unwrap()
        .into_iter()
        .filter(|n| !n.is_dropped())
        .collect();
    // Not a problem, not a removal: subscribed again with the new token.
    assert_eq!(
        notices,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
}

#[tokio::test]
async fn devices_before_m10_take_their_own_tokens_on_the_next_fetch() {
    let world = World::new();
    // Paired by clients before M10: shared tokens only.
    let (mut anna, mut bert) = {
        let (mut anna, mut bert) = (
            world.person(Some((RELAY_A, Some(TOKEN_A)))),
            world.person(Some((RELAY_B, Some(TOKEN_B)))),
        );
        anna.ctx.relay = world.relay.old_client();
        bert.ctx.relay = world.relay.old_client();
        let code = anna.ctx.invite("Bert", false).await.unwrap();
        bert.ctx.accept("Anna", &code).await.unwrap();
        anna.sync().await;
        bert.sync().await;
        anna.ctx.confirm("Bert").await.unwrap();
        bert.sync().await;
        anna.sync().await;
        (anna, bert)
    };
    let mailbox = bert_mailbox(&anna);
    let reader = anna.device().reader_id;
    assert!(anna.contact("Bert").own_token.is_none());
    assert!(!world.relay.has_own_token(RELAY_A, &mailbox, &reader));
    // Its state file as written before M10.
    let path = anna.ctx.store.path().to_owned();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"format\": 2", "\"format\": 1")).unwrap();

    // Updated, it takes its own token on the next fetch and reads on.
    anna.ctx.relay = world.relay.clone();
    bert.ctx.relay = world.relay.clone();
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
    let contact = anna.contact("Bert");
    let own = contact.own_token.unwrap();
    assert!(world.relay.has_own_token(RELAY_A, &mailbox, &reader));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"format\": 2")
    );
    assert_eq!(
        subscribe(&world, &mailbox, &contact.read_token, &reader).await,
        "unauthorized"
    );
    // Once is enough.
    let puts = world.relay.reader_puts(RELAY_A);
    anna.sync().await;
    assert_eq!(world.relay.reader_puts(RELAY_A), puts);

    // A token sent while the answer got lost is confirmed by the next try.
    let pending: Token = [0x33; 32];
    let change = ReaderChange {
        token: Some(&pending),
        ..ReaderChange::default()
    };
    world
        .relay
        .put_reader(&url(RELAY_A), &mailbox, &own, &reader, change)
        .await
        .unwrap();
    anna.ctx
        .store
        .update(|device| {
            let contact = device.contact_by_mailbox_mut(&mailbox).unwrap();
            contact.new_token = Some(pending);
            Ok::<_, std::io::Error>(())
        })
        .unwrap();
    anna.sync().await;
    let contact = anna.contact("Bert");
    assert_eq!(
        (contact.own_token, contact.new_token),
        (Some(pending), None)
    );
}

#[tokio::test]
async fn devices_before_m10_keep_working_with_updated_ones() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    // The laptop runs a client before M10: it uses the enrolment tokens of
    // the handover as its read tokens, and never replaces them.
    let mut laptop = world.blank();
    laptop.ctx.relay = world.relay.old_client();
    join(&world, &mut phone, &mut laptop, RELAY_A).await;
    assert!(laptop.contact("Bert").own_token.is_none());
    laptop
        .ctx
        .set_push_url(Some("https://push.example/up"))
        .await
        .unwrap();
    bert.ctx.request("Anna").await.unwrap();
    for person in [&mut phone, &mut laptop] {
        assert_eq!(
            person.sync().await,
            vec![Notice::Request {
                name: "Bert".into()
            }]
        );
    }

    // A contact paired on the phone reaches it; it registers itself with
    // the shared token, as before M10, and reads it.
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = phone.ctx.invite("Carl", false).await.unwrap();
    carl.ctx.accept("Anna", &code).await.unwrap();
    phone.sync().await;
    carl.sync().await;
    phone.ctx.confirm("Carl").await.unwrap();
    carl.sync().await;
    phone.sync().await;
    let notices = laptop.sync().await;
    assert!(
        notices
            .iter()
            .any(|n| matches!(n, Notice::OwnContactAdded { name, .. } if name == "Carl")),
        "{notices:?}"
    );
    let carl_mailbox = laptop.contact("Carl").state.contact.own_mailbox;
    let laptop_reader = laptop.device().reader_id;
    assert!(
        world
            .relay
            .has_reader(RELAY_A, &carl_mailbox, &laptop_reader)
    );
    assert!(
        !world
            .relay
            .has_own_token(RELAY_A, &carl_mailbox, &laptop_reader)
    );
    carl.ctx.request("Anna").await.unwrap();
    assert_eq!(
        laptop.sync().await,
        vec![Notice::Request {
            name: "Carl".into()
        }]
    );

    // Removing it locks it out all the same.
    phone.ctx.remove_device("Laptop").await.unwrap();
    for mailbox in [bert_mailbox(&phone), carl_mailbox] {
        assert!(!world.relay.has_reader(RELAY_A, &mailbox, &laptop_reader));
    }
    let notices = laptop.ctx.sync().await.unwrap();
    assert!(notices.contains(&Notice::Removed), "{notices:?}");
}

#[tokio::test]
async fn an_updated_device_added_by_one_before_m10_takes_its_own_tokens() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    phone.ctx.relay = world.relay.old_client();
    // The phone, before M10, registers the laptop with the shared tokens.
    let mut laptop = world.blank();
    join(&world, &mut phone, &mut laptop, RELAY_A).await;
    let mailbox = bert_mailbox(&laptop);
    let reader = laptop.device().reader_id;
    let contact = laptop.contact("Bert");
    assert!(contact.own_token.is_some());
    assert!(world.relay.has_own_token(RELAY_A, &mailbox, &reader));
    assert_eq!(
        subscribe(&world, &mailbox, &contact.read_token, &reader).await,
        "unauthorized"
    );
}

#[tokio::test]
async fn removal_is_weak_on_a_relay_before_m10_and_says_so() {
    let world = World::new();
    world.relay.set_old(RELAY_B, true);
    // Anna has no access token: her own mailboxes are on the relays of
    // the contacts that invited her, Bert's (updated) and Carl's (not).
    let mut phone = world.person(None);
    let mut bert = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    for (inviter, name) in [(&mut bert, "Bert"), (&mut carl, "Carl")] {
        let code = inviter.ctx.invite("Anna", true).await.unwrap();
        phone.ctx.accept(name, &code).await.unwrap();
        inviter.sync().await;
        inviter.ctx.confirm("Anna").await.unwrap();
        phone.sync().await;
    }
    let carl_mailbox = phone.contact("Carl").state.contact.own_mailbox;
    assert_eq!(phone.contact("Carl").own_relay, url(RELAY_B));
    assert!(phone.contact("Carl").own_token.is_none());
    let mut thief = world.blank();
    join(&world, &mut phone, &mut thief, RELAY_A).await;
    let copy = thief.device();

    let removal = phone.ctx.remove_device("Laptop").await.unwrap();
    assert_eq!(removal.weak, ["Carl"]);
    let out = phone.out.take();
    assert!(
        out.contains("Das Entfernen wirkt für Carl nur zum Teil"),
        "{out}"
    );
    // Bert's mailbox locks it out; on Carl's relay it can register itself
    // again with the shared token it holds, as before M10.
    let bert_mailbox = bert_mailbox(&phone);
    let bert_copy = copy.contact_by_mailbox(&bert_mailbox).unwrap();
    let again = world
        .relay
        .put_reader(
            &url(RELAY_A),
            &bert_mailbox,
            &bert_copy.token(),
            &copy.reader_id,
            ReaderChange::push(None),
        )
        .await;
    unauthorized(again);
    let carl_copy = copy.contact_by_mailbox(&carl_mailbox).unwrap();
    assert!(
        !world
            .relay
            .has_reader(RELAY_B, &carl_mailbox, &copy.reader_id)
    );
    world
        .relay
        .put_reader(
            &url(RELAY_B),
            &carl_mailbox,
            &carl_copy.token(),
            &copy.reader_id,
            ReaderChange::push(None),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_reader_without_own_token_loses_access_when_the_shared_token_goes() {
    let world = World::new();
    let (mut phone, _laptop, _bert) = family(&world).await;
    // The tablet runs a client before M10, so it never takes its own token
    // on Carl's mailbox, which it registered with the shared token.
    let mut tablet = world.blank();
    tablet.ctx.relay = world.relay.old_client();
    join(&world, &mut phone, &mut tablet, RELAY_A).await;
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = phone.ctx.invite("Carl", false).await.unwrap();
    carl.ctx.accept("Anna", &code).await.unwrap();
    phone.sync().await;
    carl.sync().await;
    phone.ctx.confirm("Carl").await.unwrap();
    carl.sync().await;
    phone.sync().await;
    tablet.sync().await;
    assert!(tablet.contact("Carl").own_token.is_none());

    phone.ctx.remove_device("Laptop").await.unwrap();
    let notices = tablet.sync().await;
    assert!(
        notices.contains(&Notice::LostAccess {
            name: Some("Carl".into())
        }),
        "{notices:?}"
    );
    assert!(
        tablet
            .out
            .take()
            .contains("Richte es erneut als weiteres Gerät ein")
    );
    // Its enrolment token from the handover still reads Bert's mailbox.
    assert!(!notices.contains(&Notice::LostAccess {
        name: Some("Bert".into())
    }));
    assert!(tablet.ctx.store.path().exists());
}
