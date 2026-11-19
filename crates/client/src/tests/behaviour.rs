//! M9 behaviour tests: relay outage, clock deviation and lost devices.
//! Relay restarts are tested in the relay crate.

use lz_core::wire::{Reason, ResponseKind};

use super::devices::family;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use lz_core::store::Store;

use crate::ctx::Clock;
use crate::relay::{Relay, RelayError};

use super::{
    HOUR, MINUTE, Person, RELAY_A, RELAY_B, TOKEN_A, TOKEN_B, World, expect_err, paired, url,
};
use crate::show::Notice;

/// `sync`, without filtering anything.
async fn raw_sync(person: &mut Person) -> Vec<Notice> {
    person.ctx.sync().await.unwrap()
}

fn problems(notices: &[Notice]) -> Vec<&Notice> {
    notices
        .iter()
        .filter(|n| matches!(n, Notice::Problem(_)))
        .collect()
}

fn count(notices: &[Notice], is: impl Fn(&Notice) -> bool) -> usize {
    notices.iter().filter(|n| is(n)).count()
}

/// `setter` lets `asker` ask once a week, without a minimum interval.
async fn one_per_week(setter: &mut Person, asker: &mut Person, name_of_setter: &str) {
    setter
        .ctx
        .rules(name_of_setter, Some(0), None, Some(1))
        .await
        .unwrap();
    asker.sync().await;
}

#[tokio::test]
async fn request_during_an_outage_fails_visibly_and_is_sent_once_later() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    // Anna may ask Bert once a week.
    one_per_week(&mut bert, &mut anna, "Anna").await;

    world.relay.set_down(RELAY_B, true);
    let error = anna.ctx.request("Bert").await.unwrap_err();
    assert!(
        error.0.contains("Nicht zugestellt") && error.0.contains("nicht erreichbar"),
        "{error:?}"
    );
    // Nothing is recorded: no open request, nothing counted.
    let state = anna.contact("Bert").state;
    assert!(state.outgoing.entries.is_empty());
    assert!(state.next_request_at(world.now()) <= world.now());
    assert!(
        anna.ctx.overview().unwrap().contacts[0]
            .outgoing_since
            .is_none()
    );
    // The user is not told it worked.
    assert!(!anna.out.take().contains("Anfrage an Bert gesendet"));

    // Retrying while still down fails the same way.
    expect_err(anna.ctx.request("Bert").await, "Nicht zugestellt");

    world.relay.set_down(RELAY_B, false);
    world.advance(MINUTE);
    anna.ctx.request("Bert").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::Request {
            name: "Anna".into()
        }]
    );
    // The weekly limit of one was not used up by the failed attempts, and
    // is by the successful one.
    expect_err(anna.ctx.request("Bert").await, "wieder möglich ab");
    assert!(bert.sync().await.is_empty());
}

#[tokio::test]
async fn manual_response_during_an_outage_is_sent_once_later() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    anna.out.take();

    world.relay.set_down(RELAY_B, true);
    expect_err(anna.ctx.respond("Bert").await, "Nicht zugestellt");
    assert!(!anna.out.take().contains("Lebenszeichen an"));
    // The request is still open and can be answered.
    assert!(anna.ctx.overview().unwrap().contacts[0].incoming.is_some());
    assert!(
        anna.contact("Bert")
            .state
            .open_request(world.now())
            .is_some()
    );

    world.relay.set_down(RELAY_B, false);
    anna.ctx.respond("Bert").await.unwrap();
    let notices = bert.sync().await;
    assert!(
        matches!(
            notices.as_slice(),
            [Notice::Response {
                kind: ResponseKind::Manual,
                ..
            }]
        ),
        "{notices:?}"
    );
    // Exactly once: a second answer is refused, and nothing else is waiting.
    expect_err(anna.ctx.respond("Bert").await, "Keine offene Anfrage");
    assert!(bert.sync().await.is_empty());
}

#[tokio::test]
async fn reported_response_during_an_outage_is_sent_once_later() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    anna.out.take();

    world.relay.set_down(RELAY_B, true);
    expect_err(anna.ctx.report("Bert", "schläft").await, "Nicht zugestellt");
    assert!(!anna.out.take().contains("An Bert gemeldet"));
    assert!(
        anna.contact("Bert")
            .state
            .open_request(world.now())
            .is_some()
    );

    world.relay.set_down(RELAY_B, false);
    anna.ctx.report("Bert", "schläft").await.unwrap();
    let reason = Reason::new("schläft").unwrap();
    let notices = bert.sync().await;
    assert!(
        matches!(
            notices.as_slice(),
            [Notice::Response { kind: ResponseKind::Reported(r), .. }] if *r == reason
        ),
        "{notices:?}"
    );
    expect_err(anna.ctx.report("Bert", "schläft").await, "schon");
    assert!(bert.sync().await.is_empty());
}

#[tokio::test]
async fn sync_message_during_an_outage_waits_in_the_outbox_and_arrives_once() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    phone.sync().await;
    laptop.sync().await;
    phone.out.take();

    // Anna's relay holds the sync mailbox; Bert's relay is up.
    world.relay.set_down(RELAY_A, true);
    phone.ctx.respond("Bert").await.unwrap();
    // Bert got the response, the laptop does not know yet, and the phone
    // says so.
    let out = phone.out.take();
    assert!(out.contains("noch nicht zugestellt"), "{out}");
    assert!(out.contains("nicht erreichbar"), "{out}");
    assert_eq!(phone.device().outbox.len(), 1);

    // Still down: a fetch keeps it.
    phone.ctx.sync().await.unwrap();
    assert_eq!(phone.device().outbox.len(), 1);

    world.relay.set_down(RELAY_A, false);
    phone.sync().await;
    assert!(phone.device().outbox.is_empty());
    let notices = laptop.sync().await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::OwnResponded { .. })),
        1,
        "{notices:?}"
    );
    // The laptop does not answer a second time, and Bert saw one response.
    expect_err(laptop.ctx.respond("Bert").await, "Keine offene Anfrage");
    assert_eq!(bert.sync().await.len(), 1);
    // Fetching again changes nothing.
    assert!(phone.sync().await.is_empty());
    assert!(laptop.sync().await.is_empty());
}

#[tokio::test]
async fn invite_during_an_outage_fails_and_leaves_nothing() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    world.relay.set_down(RELAY_A, true);
    let error = anna.ctx.invite("Bert", false).await.unwrap_err();
    assert!(error.0.contains("nicht erreichbar"), "{error:?}");
    assert!(anna.device().invitations.is_empty());
    assert!(!anna.out.take().contains("LZ2-"));

    world.relay.set_down(RELAY_A, false);
    anna.ctx.invite("Bert", false).await.unwrap();
    assert_eq!(anna.device().invitations.len(), 1);
}

#[tokio::test]
async fn accept_during_an_outage_fails_and_works_later_with_one_hello() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();

    // The inviting relay is down: `hello` cannot go out.
    world.relay.set_down(RELAY_A, true);
    let error = bert.ctx.accept("Anna", &code).await.unwrap_err();
    assert!(error.0.contains("nicht erreichbar"), "{error:?}");
    assert!(bert.device().contacts.is_empty());
    // Bert's own relay is down: no mailbox for him.
    world.relay.set_down(RELAY_A, false);
    world.relay.set_down(RELAY_B, true);
    let error = bert.ctx.accept("Anna", &code).await.unwrap_err();
    assert!(error.0.contains("nicht erreichbar"), "{error:?}");
    assert!(bert.device().contacts.is_empty());
    assert!(!bert.out.take().contains("Einladung angenommen"));

    world.relay.set_down(RELAY_B, false);
    bert.ctx.accept("Anna", &code).await.unwrap();
    let notices = anna.sync().await;
    assert!(
        matches!(notices.as_slice(), [Notice::Accepted { .. }]),
        "{notices:?}"
    );
}

#[tokio::test]
async fn reveal_during_an_outage_is_sent_again_when_hello_is_repeated() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();

    // Anna reads `hello`, but cannot send `reveal` to Bert's relay.
    world.relay.set_down(RELAY_B, true);
    let notices = raw_sync(&mut anna).await;
    assert!(matches!(notices[0], Notice::Accepted { .. }), "{notices:?}");
    assert_eq!(problems(&notices).len(), 1, "{notices:?}");
    let out = anna.out.take();
    assert!(out.contains("nicht erreichbar"), "{out}");

    // Bert has no code yet and waits; his `hello` goes out again later.
    world.relay.set_down(RELAY_B, false);
    assert!(bert.sync().await.is_empty());
    assert_eq!(
        bert.ctx.overview().unwrap().contacts[0].status,
        crate::ctx::Status::AwaitingReveal
    );
    world.advance(10 * MINUTE);
    assert!(bert.sync().await.is_empty());
    // Anna gets the repeated `hello` and sends `reveal` again, without
    // a second acceptance.
    let notices = anna.sync().await;
    assert!(
        !notices
            .iter()
            .any(|n| matches!(n, Notice::SecondAcceptance { .. })),
        "{notices:?}"
    );
    let notices = bert.sync().await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Revealed { .. })),
        1,
        "{notices:?}"
    );
}

#[tokio::test]
async fn confirm_during_an_outage_fails_and_works_later() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    anna.sync().await;
    bert.sync().await;

    world.relay.set_down(RELAY_B, true);
    expect_err(anna.ctx.confirm("Bert").await, "Nicht zugestellt");
    assert!(!anna.out.take().contains("gekoppelt"));
    assert!(matches!(
        anna.ctx.overview().unwrap().contacts[0].status,
        crate::ctx::Status::AwaitingConfirmation { .. }
    ));

    world.relay.set_down(RELAY_B, false);
    anna.ctx.confirm("Bert").await.unwrap();
    let notices = bert.sync().await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Paired { .. })),
        1,
        "{notices:?}"
    );
    expect_err(anna.ctx.confirm("Bert").await, "schon");
}

#[tokio::test]
async fn answer_lost_after_the_relay_stored_the_request() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    let mailbox = bert.contact("Anna").state.contact.own_mailbox;

    // The relay stores the request, but Anna never hears of it.
    world.relay.lose_answers(RELAY_B, 1);
    expect_err(anna.ctx.request("Bert").await, "Nicht zugestellt");
    assert_eq!(world.relay.waiting(RELAY_B, &mailbox), 1);
    assert!(anna.contact("Bert").state.outgoing.entries.is_empty());

    // Anna tries again, as the message says she may.
    world.advance(MINUTE);
    anna.ctx.request("Bert").await.unwrap();
    // Bert sees the request once; the second one is refused by his rules.
    let notices = raw_sync(&mut bert).await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Request { .. })),
        1,
        "{notices:?}"
    );
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Dropped { .. })),
        1,
        "{notices:?}"
    );

    // GAP (current behaviour, see the M9 report): the request Bert holds is
    // the one whose answer was lost, and Anna rolled it back. Bert's
    // response names that request, so Anna drops it as unknown, and her
    // second request stays open without ever being answered.
    bert.ctx.respond("Anna").await.unwrap();
    let notices = raw_sync(&mut anna).await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Response { .. })),
        0,
        "{notices:?}"
    );
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Dropped { .. })),
        1,
        "{notices:?}"
    );
    let overview = anna.ctx.overview().unwrap();
    assert!(overview.contacts[0].outgoing_since.is_some());
    assert!(overview.contacts[0].last_response.is_none());
}

#[tokio::test]
async fn answer_lost_after_the_relay_stored_the_response() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;

    world.relay.lose_answers(RELAY_B, 1);
    expect_err(anna.ctx.respond("Bert").await, "Nicht zugestellt");
    anna.ctx.respond("Bert").await.unwrap();
    let notices = bert.sync().await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Response { .. })),
        1,
        "{notices:?}"
    );
}

#[tokio::test]
async fn answer_lost_after_the_relay_stored_hello() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();

    world.relay.lose_answers(RELAY_A, 1);
    expect_err(bert.ctx.accept("Anna", &code).await, "nicht erreichbar");
    assert!(bert.device().contacts.is_empty());
    bert.ctx.accept("Anna", &code).await.unwrap();

    // GAP (current behaviour, see the M9 report): Anna accepts the lost
    // `hello`, whose sender forgot its mailbox, so her `reveal` finds no
    // mailbox and she is told that Bert ended the connection. Bert's retry
    // is dropped, because the invitation is used up. Bert waits for a
    // `reveal` that never comes, with no hint that he has to start over.
    let notices = raw_sync(&mut anna).await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Accepted { .. })),
        1,
        "{notices:?}"
    );
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::Ended { .. })),
        1,
        "{notices:?}"
    );
    assert!(bert.sync().await.is_empty());
    assert_eq!(
        bert.ctx.overview().unwrap().contacts[0].status,
        crate::ctx::Status::AwaitingReveal
    );
}

/// Gives `person` a clock of its own, `offset` seconds ahead of the world
/// (negative: behind).
fn skew(world: &World, person: &mut Person, offset: i64) {
    let now = world.now().checked_add_signed(offset).unwrap();
    person.ctx.clock = Clock::Manual(Arc::new(AtomicU64::new(now)));
}

#[tokio::test]
async fn clock_9_minutes_ahead_is_tolerated() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    skew(&world, &mut anna, 9 * 60);
    anna.ctx.request("Bert").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::Request {
            name: "Anna".into()
        }]
    );
}

#[tokio::test]
async fn clock_15_minutes_ahead_is_refused_without_the_sender_noticing() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    skew(&world, &mut anna, 15 * 60);

    // The sender sees success: nothing tells her that the other side
    // refuses the message.
    anna.ctx.request("Bert").await.unwrap();
    assert!(anna.out.take().contains("Anfrage an Bert gesendet"));
    assert!(
        anna.ctx.overview().unwrap().contacts[0]
            .outgoing_since
            .is_some()
    );
    // Bert's side drops it as expired; he is only told in verbose mode,
    // and `lz sync` reports no news.
    let notices = raw_sync(&mut bert).await;
    assert!(
        matches!(notices.as_slice(), [Notice::Dropped { why, .. }] if why.contains("time window")),
        "{notices:?}"
    );
    assert!(bert.contact("Anna").state.incoming.entries.is_empty());

    // The same for a response, which Bert sees as unanswered.
    world.advance(7 * HOUR);
    skew(&world, &mut anna, 15 * 60);
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    anna.ctx.respond("Bert").await.unwrap();
    assert!(anna.out.take().contains("Lebenszeichen an"));
    let notices = raw_sync(&mut bert).await;
    assert!(
        matches!(notices.as_slice(), [Notice::Dropped { .. }]),
        "{notices:?}"
    );
    assert!(
        bert.ctx.overview().unwrap().contacts[0]
            .last_response
            .is_none()
    );
}

#[tokio::test]
async fn clock_15_minutes_behind_sends_fine_but_refuses_everything_received() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    skew(&world, &mut anna, -15 * 60);

    // Her messages are old, but within the 24 hours: Bert accepts them.
    anna.ctx.request("Bert").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::Request {
            name: "Anna".into()
        }]
    );
    // What Bert sends is 15 minutes in the future for her: refused.
    bert.ctx.respond("Anna").await.unwrap();
    let notices = raw_sync(&mut anna).await;
    assert!(
        matches!(notices.as_slice(), [Notice::Dropped { why, .. }] if why.contains("time window")),
        "{notices:?}"
    );
    assert!(
        anna.ctx.overview().unwrap().contacts[0]
            .last_response
            .is_none()
    );
    // The sender saw success.
    assert!(bert.out.take().contains("Lebenszeichen an"));
}

/// The laptop of Anna's family is lost, and the phone removes it. Returns
/// the state of the laptop as a thief could still hold it, without the
/// sync mailbox (whose reader entry is gone, which would make a thief's
/// app delete itself on the next fetch; a thief patches that out).
async fn lost_laptop() -> (World, Person, Person, Person) {
    let world = World::new();
    let (mut phone, thief, bert) = family(&world).await;
    let mut stolen = thief.device();
    stolen.group = None;
    phone.ctx.remove_device("Laptop").await.unwrap();
    thief
        .ctx
        .store
        .update(|device| {
            *device = stolen;
            Ok::<_, std::io::Error>(())
        })
        .unwrap();
    (world, phone, thief, bert)
}

#[tokio::test]
async fn removed_device_keeps_the_old_mailboxes_until_the_contact_is_ended() {
    let (world, mut phone, mut thief, mut bert) = lost_laptop().await;
    let old = phone.contact("Bert");
    let mailbox = old.state.contact.own_mailbox;
    let reader = thief.device().reader_id;
    assert!(!world.relay.has_reader(RELAY_A, &mailbox, &reader));

    // GAP (see the M9 report): removing a device does not change the keys
    // of the contacts. The thief registers itself as a reader of the old
    // mailbox again with the read token it holds, and reads what Bert
    // sends from then on.
    world
        .relay
        .put_reader(&url(RELAY_A), &mailbox, &old.read_token, &reader, None)
        .await
        .unwrap();
    assert!(world.relay.has_reader(RELAY_A, &mailbox, &reader));
    bert.ctx.request("Anna").await.unwrap();
    thief.sync().await;
    let seen = thief.contact("Bert").state.open_request(world.now());
    assert!(seen.is_some(), "the removed device could not read");
    // The phone still reads it too.
    assert_eq!(
        phone.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );

    // It also still sends as Anna with the old send token and key. Bert
    // cannot tell: the thief answers Bert's request for her.
    thief.ctx.respond("Bert").await.unwrap();
    let notices = bert.sync().await;
    assert!(
        matches!(notices.as_slice(), [Notice::Response { .. }]),
        "{notices:?}"
    );
}

#[tokio::test]
async fn removed_device_cannot_use_the_mailboxes_of_a_new_pairing() {
    let (world, mut phone, mut thief, mut bert) = lost_laptop().await;
    let old = phone.contact("Bert");
    let old_mailbox = old.state.contact.own_mailbox;
    let old_peer = old.state.contact.peer.clone();
    let reader = thief.device().reader_id;

    // Anna ends the contact and pairs Bert again.
    phone.ctx.remove("Bert").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::Ended {
            name: "Anna".into()
        }]
    );
    bert.ctx.remove("Anna").await.unwrap();
    let code = phone.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    phone.sync().await;
    bert.sync().await;
    phone.ctx.confirm("Bert").await.unwrap();
    bert.sync().await;
    phone.sync().await;
    let new = phone.contact("Bert");
    let new_mailbox = new.state.contact.own_mailbox;
    let new_peer = new.state.contact.peer.clone();
    assert_ne!(new_mailbox, old_mailbox);
    assert_ne!(new_peer.mailbox, old_peer.mailbox);

    // The old mailboxes are gone: the thief cannot read or send there.
    assert!(!world.relay.has_mailbox(RELAY_A, &old_mailbox));
    assert!(!world.relay.has_mailbox(RELAY_B, &old_peer.mailbox));
    expect_err(thief.ctx.request("Bert").await, "beendet");
    assert!(thief.contact("Bert").state.ended);

    // The new mailboxes: the old tokens do not open them, and the thief
    // is no reader there.
    let old_read = old.read_token;
    let relay = &world.relay;
    let put = relay
        .put_reader(&url(RELAY_A), &new_mailbox, &old_read, &reader, None)
        .await;
    assert!(
        matches!(put, Err(RelayError::Refused { status: 401, .. })),
        "{put:?}"
    );
    assert!(!relay.has_reader(RELAY_A, &new_mailbox, &reader));
    let sent = relay
        .deliver(
            &new_peer.relay,
            &new_peer.mailbox,
            &old_peer.send_token,
            &[0; lz_core::wire::ENVELOPE_LEN],
        )
        .await;
    assert!(
        matches!(sent, Err(RelayError::Refused { status: 401, .. })),
        "{sent:?}"
    );
    // Bert's new request reaches only the phone.
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(
        phone.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
    assert!(thief.sync().await.is_empty());
}

#[tokio::test]
async fn answer_lost_after_the_relay_stored_a_sync_message() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    phone.sync().await;
    laptop.sync().await;

    world.relay.lose_answers(RELAY_A, 1);
    phone.ctx.respond("Bert").await.unwrap();
    assert_eq!(phone.device().outbox.len(), 1);
    phone.sync().await;
    assert!(phone.device().outbox.is_empty());
    let notices = laptop.sync().await;
    assert_eq!(
        count(&notices, |n| matches!(n, Notice::OwnResponded { .. })),
        1,
        "{notices:?}"
    );
}
