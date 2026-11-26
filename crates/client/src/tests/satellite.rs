//! The satellite end to end (M10, part 2): full devices of the person, a
//! contact, and the reference satellite on fake relays.

use lz_core::crypto;
use lz_core::wire::{
    Body, Message, Reason, Responded, ResponseKind, Rules, RulesSet, SatelliteSettings,
};
use rand_core::{OsRng, RngCore};

use super::devices::family;
use super::reference::{Did, RefSatellite, SatState, vectors};
use super::{HOUR, MINUTE, Person, RELAY_A, RELAY_B, TOKEN_B, World, expect_err, paired, url};
use crate::relay::{MailboxRead, ReaderChange, Relay, RelayError, Role};
use crate::satellite::PresetChange;
use crate::show::Notice;

/// Sets up satellite "Flur" on `phone`.
async fn satellite_of(world: &World, phone: &mut Person) -> RefSatellite {
    let setup = phone
        .ctx
        .add_satellite("Flur", None, "home", "secret")
        .await
        .unwrap();
    phone.out.take();
    RefSatellite::install(&world.relay, &world.clock, &setup.bytes).await
}

/// `anna` pairs with a new person `name` on relay B; returns that person.
async fn pair_another(world: &World, anna: &mut Person, name: &str) -> Person {
    let mut other = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite(name, false).await.unwrap();
    other.ctx.accept("Anna", &code).await.unwrap();
    anna.sync().await;
    other.sync().await;
    anna.ctx.confirm(name).await.unwrap();
    other.sync().await;
    anna.sync().await;
    anna.out.take();
    other.out.take();
    other
}

fn unauthorized<T: std::fmt::Debug>(result: Result<T, RelayError>) {
    assert!(
        matches!(result, Err(RelayError::Refused { status: 401, .. })),
        "{result:?}"
    );
}

/// An envelope on the channel as the satellite would seal it at `now`.
fn sealed_by(satellite: &RefSatellite, body: Body, now: u64) -> Vec<u8> {
    let mut id = [0; 16];
    OsRng.fill_bytes(&mut id);
    let message = Message {
        id,
        sent_at: now,
        body,
    };
    crypto::seal_group(&message, &satellite.state().channel.key, &mut OsRng)
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn adding_a_satellite_registers_it_with_its_own_tokens() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let satellite = satellite_of(&world, &mut phone).await;
    let state = satellite.state();
    let device = phone.device();
    let stored = &device.satellites[0];
    assert_eq!(stored.name, "Flur");
    assert_eq!(state.contacts.len(), 1);
    let bert = &state.contacts[0];
    assert_eq!(bert.name, "Bert");
    let mailbox = bert.own_mailbox;
    // Its reader is on the contact mailbox and the channel, with tokens
    // only it knows; the enrolment tokens no longer work.
    for (mailbox, own, enrolment) in [
        (mailbox, bert.own_token.unwrap(), bert.enrolment),
        (
            state.channel.mailbox,
            state.channel.own_token.unwrap(),
            state.channel.enrolment,
        ),
    ] {
        assert!(world.relay.has_own_token(RELAY_A, &mailbox, &state.reader));
        assert_ne!(own, enrolment);
        let put = world
            .relay
            .put_reader(
                &url(RELAY_A),
                &mailbox,
                &enrolment,
                &state.reader,
                ReaderChange::push(None),
            )
            .await;
        unauthorized(put);
    }
    // The setup carries the channel, not the shared token of the channel
    // nor of any contact mailbox.
    assert_ne!(state.channel.enrolment, stored.read_token);
    assert_ne!(bert.enrolment, phone.contact("Bert").read_token);
    let list = phone.ctx.devices().unwrap();
    assert!(list.iter().any(|d| d.satellite && d.name == "Flur"));
}

#[tokio::test]
async fn a_press_answers_every_open_request_and_the_phone_withdraws_its_own() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut carl = pair_another(&world, &mut phone, "Carl").await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    bert.ctx.request("Anna").await.unwrap();
    carl.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    phone.sync().await;
    let now = world.now();
    assert_eq!(satellite.state().open_requests(now), ["Bert", "Carl"]);
    assert!(phone.contact("Bert").state.open_request(now).is_some());

    world.advance(MINUTE);
    assert_eq!(satellite.press(), 2);
    satellite.poll().await;
    assert!(satellite.state().open_requests(world.now()).is_empty());
    for (person, name) in [(&mut bert, "Anna"), (&mut carl, "Anna")] {
        let notices = person.sync().await;
        assert!(
            matches!(
                &notices[..],
                [Notice::Response { name: n, kind: ResponseKind::Manual, .. }] if n == name
            ),
            "{notices:?}"
        );
    }
    let notices = phone.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteAnswered {
            name: "Bert".into(),
            satellite: "Flur".into(),
            kind: ResponseKind::Manual,
        }),
        "{notices:?}"
    );
    let now = world.now();
    assert_eq!(phone.contact("Bert").state.open_request(now), None);
    assert_eq!(phone.contact("Carl").state.open_request(now), None);
    // The minimum interval started on the phone as well.
    assert_eq!(phone.contact("Bert").state.incoming.last_answer, Some(now));
}

#[tokio::test]
async fn an_answer_elsewhere_turns_the_light_off() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    assert_eq!(satellite.state().open_requests(world.now()), ["Bert"]);
    phone.sync().await;
    phone.ctx.respond("Bert").await.unwrap();
    satellite.poll().await;
    assert!(satellite.state().open_requests(world.now()).is_empty());
    // Nothing for the satellite to answer any more.
    assert_eq!(satellite.press(), 0);
}

#[tokio::test]
async fn requests_made_elsewhere_reach_the_satellite() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    phone
        .ctx
        .set_satellite_contact("Flur", "Bert", None, Some(1))
        .await
        .unwrap();
    satellite.poll().await;
    phone.ctx.request("Bert").await.unwrap();
    satellite.poll().await;
    // The ask button is disabled, and the contact's answer shows.
    assert!(satellite.ask(1).is_err());
    bert.sync().await;
    bert.ctx.respond("Anna").await.unwrap();
    satellite.poll().await;
    assert_eq!(
        satellite.state().shown_answers(world.now()),
        [("Bert".to_owned(), ResponseKind::Manual)]
    );
}

#[tokio::test]
async fn asking_obeys_the_rules_of_the_contact_and_the_phone_sees_it() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    // Unbound button: nothing.
    assert_eq!(satellite.ask(1), Err(u64::MAX));
    phone
        .ctx
        .set_satellite_contact("Flur", "Bert", None, Some(1))
        .await
        .unwrap();
    satellite.poll().await;
    satellite.ask(1).unwrap();
    satellite.poll().await;
    assert!(matches!(&bert.sync().await[..], [Notice::Request { .. }]));
    let notices = phone.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteAsked {
            name: "Bert".into(),
            satellite: "Flur".into()
        }),
        "{notices:?}"
    );
    // The phone's button is disabled, and it accepts Bert's answer.
    expect_err(phone.ctx.request("Bert").await, "Bert");
    world.advance(MINUTE);
    bert.ctx.respond("Anna").await.unwrap();
    let notices = phone.sync().await;
    assert!(
        matches!(&notices[..], [Notice::Response { .. }]),
        "{notices:?}"
    );
    satellite.poll().await;
    assert_eq!(
        satellite.state().shown_answers(world.now()),
        [("Bert".to_owned(), ResponseKind::Manual)]
    );
    // Within the minimum interval of Bert's rules the satellite refuses,
    // and says until when.
    let answered = world.now();
    assert_eq!(
        satellite.ask(1),
        Err(answered + u64::from(Rules::DEFAULT.min_interval_minutes) * 60)
    );
    // A press dismisses the answer and sends nothing.
    assert_eq!(satellite.press(), 0);
    assert!(satellite.state().shown_answers(world.now()).is_empty());
}

#[tokio::test]
async fn reported_answers_after_the_waiting_time_only_where_allowed_once() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut carl = pair_another(&world, &mut phone, "Carl").await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    let asleep = PresetChange::Set {
        minutes: 480,
        share: false,
        reason: "asleep".into(),
    };
    phone
        .ctx
        .set_satellite_settings("Flur", Some(15), Some(asleep))
        .await
        .unwrap();
    phone
        .ctx
        .set_satellite_contact("Flur", "Bert", Some(true), None)
        .await
        .unwrap();
    satellite.poll().await;
    satellite.long_press();
    assert!(satellite.state().current_state(world.now()).is_some());
    bert.ctx.request("Anna").await.unwrap();
    carl.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    world.advance(14 * MINUTE);
    satellite.poll().await;
    assert!(bert.sync().await.is_empty(), "not before the waiting time");
    world.advance(2 * MINUTE);
    satellite.poll().await;
    let reason = Reason::new("asleep").unwrap();
    assert!(
        matches!(
            &bert.sync().await[..],
            [Notice::Response { kind: ResponseKind::Reported(r), .. }] if *r == reason
        ),
        "reported to Bert"
    );
    assert!(carl.sync().await.is_empty(), "Carl does not allow it");
    // Once per request, also after more ticks.
    world.advance(10 * MINUTE);
    satellite.poll().await;
    assert!(bert.sync().await.is_empty());
    // The request stays open; the phone knows a reported answer went out.
    let notices = phone.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteAnswered {
            name: "Bert".into(),
            satellite: "Flur".into(),
            kind: ResponseKind::Reported(reason),
        }),
        "{notices:?}"
    );
    let phone_entry = phone.contact("Bert").state.incoming.entries[0].clone();
    assert_eq!(phone_entry.reported, Some(reason));
    assert!(phone_entry.answered_at.is_none());
    // A later press still sends ✓.
    assert_eq!(satellite.press(), 2);
    satellite.poll().await;
    assert!(matches!(
        &bert.sync().await[..],
        [Notice::Response {
            kind: ResponseKind::Manual,
            ..
        }]
    ));
}

#[tokio::test]
async fn a_restart_during_the_waiting_time_still_reports_once() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    let asleep = PresetChange::Set {
        minutes: 480,
        share: false,
        reason: "asleep".into(),
    };
    phone
        .ctx
        .set_satellite_settings("Flur", Some(15), Some(asleep))
        .await
        .unwrap();
    phone
        .ctx
        .set_satellite_contact("Flur", "Bert", Some(true), None)
        .await
        .unwrap();
    satellite.poll().await;
    satellite.long_press();
    bert.ctx.request("Anna").await.unwrap();
    world.advance(5 * MINUTE);
    satellite.poll().await;
    satellite.restart();
    world.advance(9 * MINUTE);
    satellite.poll().await;
    assert!(bert.sync().await.is_empty());
    satellite.restart();
    world.advance(MINUTE + 1);
    satellite.poll().await;
    assert_eq!(bert.sync().await.len(), 1, "at the right time");
    satellite.restart();
    world.advance(MINUTE);
    satellite.poll().await;
    assert!(bert.sync().await.is_empty(), "and only once");
}

#[tokio::test]
async fn states_are_shared_both_ways() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    let shared = PresetChange::Set {
        minutes: 60,
        share: true,
        reason: "asleep".into(),
    };
    phone
        .ctx
        .set_satellite_settings("Flur", None, Some(shared))
        .await
        .unwrap();
    satellite.poll().await;

    // Up: the satellite's state is handed to the phone.
    satellite.long_press();
    satellite.poll().await;
    let notices = phone.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteState {
            reason: Some("asleep".into()),
            satellite: "Flur".into()
        }),
        "{notices:?}"
    );
    let state = phone.device().current_state(world.now()).unwrap();
    assert!(state.handed && state.reason.as_str() == "asleep");

    // Down: "Phone empty" shared by the phone replaces it on the satellite.
    world.advance(10);
    phone.ctx.set_state("Phone empty", 120, true).unwrap();
    phone.sync().await;
    satellite.poll().await;
    let now = world.now();
    let current = satellite.state().current_state(now).unwrap();
    assert_eq!(current.reason.as_str(), "Phone empty");
    assert!(satellite.state().own_state.is_none(), "the shared one went");

    // A clear on the phone ends it on the satellite.
    world.advance(10);
    phone.ctx.clear_state(true).unwrap();
    phone.sync().await;
    satellite.poll().await;
    assert!(satellite.state().current_state(world.now()).is_none());

    // An own state that was never shared stays through a clear.
    let unshared = PresetChange::Set {
        minutes: 60,
        share: false,
        reason: "out".into(),
    };
    phone
        .ctx
        .set_satellite_settings("Flur", None, Some(unshared))
        .await
        .unwrap();
    satellite.poll().await;
    satellite.long_press();
    world.advance(10);
    phone.ctx.set_state("Phone empty", 120, true).unwrap();
    phone.sync().await;
    world.advance(10);
    phone.ctx.clear_state(true).unwrap();
    phone.sync().await;
    satellite.poll().await;
    let current = satellite.state().current_state(world.now()).unwrap();
    assert_eq!(current.reason.as_str(), "out");

    // A clear from the satellite ends the phone's shared state.
    let shared = PresetChange::Set {
        minutes: 60,
        share: true,
        reason: "asleep".into(),
    };
    phone
        .ctx
        .set_satellite_settings("Flur", None, Some(shared))
        .await
        .unwrap();
    satellite.poll().await;
    satellite.long_press(); // ends "out"
    world.advance(10);
    phone.ctx.set_state("Phone empty", 120, true).unwrap();
    phone.sync().await;
    satellite.poll().await;
    world.advance(10);
    satellite.long_press(); // sets and shares "asleep"
    world.advance(10);
    satellite.long_press(); // ends it everywhere
    satellite.poll().await;
    phone.sync().await;
    assert!(phone.device().current_state(world.now()).is_none());
}

#[tokio::test]
async fn settings_propagate_between_full_devices_and_down() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    // The laptop learns of the satellite and reads its channel.
    let notices = laptop.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteAdded {
            name: "Flur".into()
        }),
        "{notices:?}"
    );
    let channel = satellite.state().channel.mailbox;
    let laptop_reader = laptop.device().reader_id;
    assert!(world.relay.has_own_token(RELAY_A, &channel, &laptop_reader));

    let preset = PresetChange::Set {
        minutes: 90,
        share: true,
        reason: "out".into(),
    };
    laptop
        .ctx
        .set_satellite_settings("Flur", Some(30), Some(preset))
        .await
        .unwrap();
    laptop
        .ctx
        .set_satellite_contact("Flur", "Bert", Some(true), Some(2))
        .await
        .unwrap();
    let notices = phone.sync().await;
    assert!(notices.contains(&Notice::SatelliteSettings {
        satellite: "Flur".into()
    }));
    let on_phone = phone.device().satellites[0].clone();
    assert_eq!(on_phone.settings.waiting_minutes, 30);
    let bert_mailbox = phone.contact("Bert").state.contact.own_mailbox;
    assert!(on_phone.contact_flags(&bert_mailbox).reported);
    satellite.poll().await;
    let state = satellite.state();
    assert_eq!(state.settings.waiting_minutes, 30);
    assert_eq!(state.settings.preset.unwrap().minutes, 90);
    assert_eq!(state.contact("Bert").unwrap().flags.ask_button, 2);
    // An older change arriving late is not applied, on the satellite either.
    let older = Message {
        id: [0; 16],
        sent_at: state.settings_stamp.0 - 1,
        body: Body::HomeSettings(SatelliteSettings::DEFAULT),
    };
    let envelope = crypto::seal_group(&older, &state.channel.key, &mut OsRng).unwrap();
    let now = world.now();
    assert_eq!(
        satellite.state_mut().receive_channel(&envelope, now),
        [Did::Dropped("outdated")]
    );
    // The phone's own settings changes go down too.
    phone
        .ctx
        .set_satellite_settings("Flur", Some(45), None)
        .await
        .unwrap();
    satellite.poll().await;
    laptop.sync().await;
    assert_eq!(satellite.state().settings.waiting_minutes, 45);
    assert_eq!(laptop.device().satellites[0].settings.waiting_minutes, 45);
}

#[tokio::test]
async fn a_contact_paired_later_reaches_the_satellite() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    laptop.sync().await;
    // Paired on the phone, and on the laptop.
    let mut carl = pair_another(&world, &mut phone, "Carl").await;
    let mut dora = pair_another(&world, &mut laptop, "Dora").await;
    let did = satellite.poll().await;
    assert!(did.contains(&Did::ContactAdded("Carl".into())), "{did:?}");
    assert!(did.contains(&Did::ContactAdded("Dora".into())), "{did:?}");
    let reader = satellite.state().reader;
    let carl_mailbox = satellite.state().contact("Carl").unwrap().own_mailbox;
    assert!(world.relay.has_own_token(RELAY_A, &carl_mailbox, &reader));
    // It reads them.
    carl.ctx.request("Anna").await.unwrap();
    dora.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    assert_eq!(
        satellite.state().open_requests(world.now()),
        ["Carl", "Dora"]
    );
    assert_eq!(satellite.press(), 2);
    satellite.poll().await;
    assert_eq!(carl.sync().await.len(), 1);
    assert_eq!(dora.sync().await.len(), 1);
    // A contact ended disappears.
    phone.ctx.remove("Carl").await.unwrap();
    satellite.poll().await;
    assert!(satellite.state().contact("Carl").is_none());
}

#[tokio::test]
async fn the_satellite_cannot_act_as_a_full_device() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    let state = satellite.state().clone();
    let relay = url(RELAY_A);
    let channel = state.channel.mailbox;
    let bert = state.contacts[0].clone();
    for (mailbox, token) in [
        (channel, state.channel.own_token.unwrap()),
        (bert.own_mailbox, bert.own_token.unwrap()),
    ] {
        // Register or change another reader, or itself as full.
        unauthorized(
            world
                .relay
                .put_reader(
                    &relay,
                    &mailbox,
                    &token,
                    &[0x77; 16],
                    ReaderChange::push(None),
                )
                .await,
        );
        let as_full = ReaderChange {
            role: Some(Role::Full),
            ..ReaderChange::default()
        };
        assert!(
            world
                .relay
                .put_reader(&relay, &mailbox, &token, &state.reader, as_full)
                .await
                .is_err()
        );
        unauthorized(world.relay.delete_mailbox(&relay, &mailbox, &token).await);
        unauthorized(world.relay.drop_read_token(&relay, &mailbox, &token).await);
        unauthorized(
            world
                .relay
                .put_send_token(&relay, &mailbox, &token, &[1; 32])
                .await,
        );
        unauthorized(
            world
                .relay
                .revoke_readers(&relay, &mailbox, &token, &[], true)
                .await,
        );
        unauthorized(
            world
                .relay
                .delete_reader(&relay, &mailbox, &token, &phone.device().reader_id)
                .await,
        );
        unauthorized(
            world
                .relay
                .create_mailbox(&relay, &token, MailboxRead::Shared(&[2; 32]), &[3; 32])
                .await,
        );
        unauthorized(
            world
                .relay
                .put_slot(&relay, &[4; 32], &token, &[5; 32], None, b"x")
                .await,
        );
    }

    // Down types and sync types on the channel are dropped by the phone.
    let bert_mailbox = bert.own_mailbox;
    let before = phone.device();
    for body in [
        Body::HomeSettings(SatelliteSettings::DEFAULT),
        Body::HomeRules(RulesSet {
            contact: bert_mailbox,
            rules: Rules {
                weekly_limit: 0,
                ..Rules::DEFAULT
            },
        }),
        Body::HomeSendToken([9; 32]),
        Body::RulesSet(RulesSet {
            contact: bert_mailbox,
            rules: Rules {
                weekly_limit: 0,
                ..Rules::DEFAULT
            },
        }),
        Body::ContactRemoved(bert_mailbox),
        Body::Responded(Responded {
            contact: bert_mailbox,
            request: [1; 16],
            kind: ResponseKind::Manual,
            at: world.now(),
        }),
        Body::Request,
    ] {
        let envelope = sealed_by(&satellite, body, world.now());
        world
            .relay
            .deliver(&relay, &channel, &state.channel.send_token, &envelope)
            .await
            .unwrap();
    }
    let notices = phone.ctx.sync().await.unwrap();
    assert!(notices.iter().all(Notice::is_dropped), "{notices:?}");
    let after = phone.device();
    assert_eq!(after.contacts.len(), 1);
    assert_eq!(
        after.contacts[0].state.own_rules,
        before.contacts[0].state.own_rules
    );
    assert_eq!(
        after.satellites[0].send_token,
        before.satellites[0].send_token
    );
    // The satellite has neither the group key nor the sync mailbox: it
    // cannot send there at all.
    let _ = satellite.poll().await;
}

#[tokio::test]
async fn the_satellite_drops_up_types_and_replays_after_a_restart() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    // Its own up message, reflected, and an unknown type are dropped.
    let reflected = sealed_by(
        &satellite,
        Body::SatAnswered(Responded {
            contact: satellite.state().contacts[0].own_mailbox,
            request: [1; 16],
            kind: ResponseKind::Manual,
            at: world.now(),
        }),
        world.now(),
    );
    let now = world.now();
    assert_eq!(
        satellite.state_mut().receive_channel(&reflected, now),
        [Did::Dropped("not a down type")]
    );
    // A down message, applied once; again after a restart, dropped.
    let id = phone.contact("Bert").state.open_request(now);
    assert!(id.is_none(), "the phone has not fetched yet");
    phone.sync().await;
    phone.ctx.respond("Bert").await.unwrap();
    let device = phone.device();
    assert!(device.satellites[0].outbox.is_empty(), "delivered");
    let down = Message {
        id: [0xAB; 16],
        sent_at: world.now(),
        body: Body::HomeStateCleared,
    };
    let key = satellite.state().channel.key.clone();
    let envelope = crypto::seal_group(&down, &key, &mut OsRng).unwrap();
    assert_eq!(
        satellite.state_mut().receive_channel(&envelope, now),
        [Did::Applied(95)]
    );
    satellite.restart();
    assert_eq!(
        satellite.state_mut().receive_channel(&envelope, now),
        [Did::Dropped("replay")]
    );
    satellite.poll().await;
    assert!(satellite.state().open_requests(world.now()).is_empty());
}

#[tokio::test]
async fn removing_the_satellite_locks_it_out_and_it_wipes_itself() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    laptop.sync().await;
    let copy: SatState = satellite.state().clone();
    phone.ctx.remove_satellite("Flur").await.unwrap();
    assert!(phone.device().satellites.is_empty());
    assert!(!world.relay.has_mailbox(RELAY_A, &copy.channel.mailbox));
    let bert_mailbox = copy.contacts[0].own_mailbox;
    assert!(!world.relay.has_reader(RELAY_A, &bert_mailbox, &copy.reader));
    // The satellite wipes itself on its next connection.
    satellite.poll().await;
    assert!(satellite.wiped());
    // A copy of its flash can read nothing new, nor register itself again.
    bert.ctx.request("Anna").await.unwrap();
    let token = copy.contacts[0].own_token.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut connection = world.relay.connect(&url(RELAY_A), tx).await.unwrap();
    connection
        .subscribe(&bert_mailbox, &token, &copy.reader)
        .unwrap();
    assert!(matches!(
        rx.recv().await.unwrap().frame,
        crate::relay::ServerFrame::Error { code, .. } if code == "unknown_reader"
    ));
    unauthorized(
        world
            .relay
            .put_reader(
                &url(RELAY_A),
                &bert_mailbox,
                &token,
                &copy.reader,
                ReaderChange::push(None),
            )
            .await,
    );
    // The group stays; the laptop forgets the satellite.
    let notices = laptop.sync().await;
    assert!(
        notices.contains(&Notice::SatelliteGone {
            name: "Flur".into()
        }),
        "{notices:?}"
    );
    assert!(laptop.device().satellites.is_empty());
    assert!(phone.device().group.is_some() && laptop.device().group.is_some());
}

#[tokio::test]
async fn removing_a_full_device_keeps_the_satellite_and_rotates_the_send_token() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    laptop.sync().await;
    let copy = laptop.device();
    let old_send = copy.satellites[0].send_token;
    let channel = copy.satellites[0].mailbox;
    phone.ctx.remove_device("Laptop").await.unwrap();
    // The laptop is gone from the channel; the send token changed.
    assert!(!world.relay.has_reader(RELAY_A, &channel, &copy.reader_id));
    let new_send = phone.device().satellites[0].send_token;
    assert_ne!(new_send, old_send);
    let envelope = sealed_by(&satellite, Body::SatStateCleared, world.now());
    unauthorized(
        world
            .relay
            .deliver(&url(RELAY_A), &channel, &old_send, &envelope)
            .await,
    );
    // The satellite takes the new token and keeps working.
    satellite.poll().await;
    assert_eq!(satellite.state().channel.send_token, new_send);
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    assert_eq!(satellite.press(), 1);
    satellite.poll().await;
    assert!(satellite.state().outbox.is_empty(), "delivered");
    let notices = phone.sync().await;
    assert!(
        notices
            .iter()
            .any(|n| matches!(n, Notice::SatelliteAnswered { .. })),
        "{notices:?}"
    );
    assert_eq!(bert.sync().await.len(), 1);
    // The removed laptop gets nothing more there.
    laptop.ctx.sync().await.unwrap();
    assert!(!laptop.ctx.store.path().exists(), "it deleted its state");
}

#[tokio::test]
async fn setting_up_refuses_an_old_relay() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    world.relay.set_old(RELAY_A, true);
    expect_err(
        phone.ctx.add_satellite("Flur", None, "", "").await,
        "relay-a",
    );
    assert!(phone.device().satellites.is_empty());
}

#[tokio::test]
async fn setting_up_refuses_more_than_three_relays() {
    let world = World::new();
    // Without an access token, every own mailbox is created with the
    // voucher of the invitation, on the inviter's relay.
    let mut phone = world.person(Some((RELAY_A, None)));
    let relays = [
        RELAY_B,
        "https://relay-c.example",
        "https://relay-d.example",
        "https://relay-e.example",
    ];
    for (i, relay) in relays.into_iter().enumerate() {
        let token = [0xC0 + i as u8; 32];
        if relay != RELAY_B {
            world.relay.add(relay, token);
        }
        let token = if relay == RELAY_B { TOKEN_B } else { token };
        let mut other = world.person(Some((relay, Some(token))));
        let code = other.ctx.invite("Anna", true).await.unwrap();
        phone.ctx.accept(&format!("C{i}"), &code).await.unwrap();
        other.sync().await;
        phone.sync().await;
        other.ctx.confirm("Anna").await.unwrap();
        phone.sync().await;
    }
    expect_err(phone.ctx.add_satellite("Flur", None, "", "").await, "4");
    assert!(phone.device().satellites.is_empty());
}

#[test]
fn vectors_are_reproducible() {
    let first = vectors(b"m10");
    assert_eq!(first, vectors(b"m10"));
    assert_ne!(first, vectors(b"other"));
    let channel = first["channel"].as_array().unwrap();
    assert!(channel.len() >= 15);
    for vector in channel {
        assert_eq!(vector["envelope"].as_str().unwrap().len(), 2 * 297);
        assert_eq!(vector["plaintext"].as_str().unwrap().len(), 2 * 256);
    }
    if let Ok(path) = std::env::var("LZ_VECTORS_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&first).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn a_device_added_later_gets_the_satellite_in_the_handover() {
    let world = World::new();
    let (mut phone, _laptop, mut bert) = family(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    phone
        .ctx
        .set_satellite_settings("Flur", Some(20), None)
        .await
        .unwrap();
    let tablet = super::devices::add_tablet(&world, &mut phone).await;
    let device = tablet.device();
    let there = &device.satellites[0];
    assert_eq!(there.name, "Flur");
    assert_eq!(there.settings.waiting_minutes, 20);
    // It took its own token on the channel; the enrolment token is void.
    assert!(there.own_token.is_some());
    assert!(
        world
            .relay
            .has_own_token(RELAY_A, &there.mailbox, &device.reader_id)
    );
    assert_ne!(there.read_token, phone.device().satellites[0].read_token);
    // And it reads what the satellite does.
    let mut tablet = tablet;
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    satellite.press();
    satellite.poll().await;
    let notices = tablet.sync().await;
    assert!(
        notices
            .iter()
            .any(|n| matches!(n, Notice::SatelliteAnswered { .. })),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_press_offline_goes_out_later_with_the_time_of_the_press() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    world.relay.set_down(RELAY_A, true);
    world.relay.set_down(RELAY_B, true);
    world.advance(MINUTE);
    let pressed = world.now();
    assert_eq!(satellite.press(), 1);
    satellite.poll().await;
    assert_eq!(satellite.state().outbox.len(), 2, "kept");
    world.advance(HOUR);
    world.relay.set_down(RELAY_A, false);
    world.relay.set_down(RELAY_B, false);
    satellite.poll().await;
    assert!(satellite.state().outbox.is_empty());
    let notices = bert.sync().await;
    assert!(
        matches!(&notices[..], [Notice::Response { kind: ResponseKind::Manual, at, .. }] if *at == pressed),
        "{notices:?}"
    );
    phone.sync().await;
    assert_eq!(
        phone.contact("Bert").state.incoming.last_answer,
        Some(pressed)
    );
}

#[tokio::test]
async fn rules_changed_on_the_phone_reach_the_satellite() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut satellite = satellite_of(&world, &mut phone).await;
    phone
        .ctx
        .rules("Bert", Some(0), None, Some(1))
        .await
        .unwrap();
    satellite.poll().await;
    let on_phone = phone.contact("Bert").state;
    let there = &satellite.state().contact("Bert").unwrap().state;
    assert_eq!(there.own_rules, on_phone.own_rules);
    assert_eq!(there.own_rules_stamp, on_phone.own_rules_stamp);
    assert_eq!(there.own_rules.weekly_limit, 1);
    // One request is shown; after its answer, a second within the week is
    // discarded by both, as Bert's own client would not send it.
    bert.sync().await;
    bert.ctx.request("Anna").await.unwrap();
    satellite.poll().await;
    assert_eq!(satellite.press(), 1);
    satellite.poll().await;
    phone.sync().await;
    bert.sync().await;
    world.advance(MINUTE);
    let mut forged = bert.contact("Anna").state;
    forged.peer_rules = Rules {
        min_interval_minutes: 0,
        ..Rules::DEFAULT
    };
    let (_, envelope) = forged.request(world.now(), &mut OsRng).unwrap();
    let peer = forged.contact.peer.clone();
    world
        .relay
        .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &envelope)
        .await
        .unwrap();
    satellite.poll().await;
    phone.sync().await;
    let now = world.now();
    assert!(satellite.state().open_requests(now).is_empty());
    assert_eq!(phone.contact("Bert").state.open_request(now), None);
}
