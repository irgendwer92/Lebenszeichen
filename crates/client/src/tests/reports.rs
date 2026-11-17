//! Reported answers end to end: Anna allows them for Bert, reports a state,
//! and her device answers once the waiting time has passed.

use lz_core::store::Store;
use lz_core::wire::{Reason, ResponseKind};

use super::{MINUTE, Person, World, paired};
use crate::show::Notice;

fn set_state(person: &Person, reason: &str, minutes: u32, now: u64) {
    person
        .ctx
        .store
        .update(|device| {
            device
                .set_state(Reason::new(reason).unwrap(), minutes, now)
                .unwrap();
            Ok::<_, std::io::Error>(())
        })
        .unwrap();
}

fn asleep() -> ResponseKind {
    ResponseKind::Reported(Reason::new("schläft").unwrap())
}

#[tokio::test]
async fn reported_answer_after_the_waiting_time() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    let setting = anna.ctx.set_report_setting("Bert", true, None).unwrap();
    assert_eq!(setting.delay_minutes, 15);
    set_state(&anna, "schläft", 480, world.now());

    bert.ctx.request("Anna").await.unwrap();
    // Arrives a while after it was sent; the waiting time starts here.
    world.advance(30 * MINUTE);
    let arrival = world.now();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );

    world.advance(15 * MINUTE - 1);
    assert!(anna.sync().await.is_empty());
    assert!(bert.sync().await.is_empty());

    world.advance(1);
    assert_eq!(
        anna.sync().await,
        vec![Notice::Reported {
            name: "Bert".into(),
            reason: "schläft".into()
        }]
    );
    assert!(anna.out.take().contains("automatisch geantwortet"));
    assert_eq!(
        bert.sync().await,
        vec![Notice::Response {
            name: "Anna".into(),
            kind: asleep(),
            at: arrival + 15 * MINUTE,
        }]
    );

    // Only once; the request stays open for a manual answer.
    world.advance(MINUTE);
    assert!(anna.sync().await.is_empty());
    anna.ctx.respond("Bert").await.unwrap();
    assert!(matches!(
        bert.sync().await[..],
        [Notice::Response {
            kind: ResponseKind::Manual,
            ..
        }]
    ));
}

#[tokio::test]
async fn no_reported_answer_unless_allowed_or_after_a_manual_one() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    set_state(&anna, "schläft", 480, world.now());

    // Not allowed: nothing goes out.
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    world.advance(60 * MINUTE);
    assert!(anna.sync().await.is_empty());
    assert!(bert.sync().await.is_empty());

    // Allowed, but answered by hand first.
    anna.ctx.set_report_setting("Bert", true, Some(5)).unwrap();
    anna.ctx.respond("Bert").await.unwrap();
    world.advance(60 * MINUTE);
    assert!(anna.sync().await.is_empty());
    assert!(matches!(
        bert.sync().await[..],
        [Notice::Response {
            kind: ResponseKind::Manual,
            ..
        }]
    ));
}

#[tokio::test]
async fn overview_and_listing_show_state_and_setting() {
    let world = World::new();
    let (mut anna, _bert) = paired(&world).await;
    assert_eq!(anna.ctx.overview().unwrap().state, None);
    set_state(&anna, "schläft", 60, world.now());
    anna.ctx.set_report_setting("Bert", true, Some(30)).unwrap();
    let overview = anna.ctx.overview().unwrap();
    let state = overview.state.unwrap();
    assert_eq!(state.reason, "schläft");
    assert_eq!(state.until, world.now() + 60 * MINUTE);
    assert!(overview.contacts[0].reports_allowed);
    assert_eq!(overview.contacts[0].report_delay_minutes, 30);

    anna.ctx.contacts().unwrap();
    let listing = anna.out.take();
    assert!(
        listing.contains("Gemeldeter Zustand: „schläft“ bis"),
        "{listing}"
    );
    assert!(
        listing.contains("nach 30 min mit dem gemeldeten"),
        "{listing}"
    );

    let error = anna
        .ctx
        .set_report_setting("Bert", true, Some(0))
        .unwrap_err();
    assert!(error.0.contains("Wartezeit"), "{error:?}");
}

#[test]
fn token_for_reports_from_other_apps() {
    let world = World::new();
    let mut anna = world.person(None);
    assert_eq!(anna.ctx.report_token().unwrap(), None);
    assert!(!anna.ctx.report_token_matches(&"0".repeat(64)).unwrap());

    let first = anna.ctx.enable_report_token().unwrap();
    assert_eq!(first.len(), 64);
    assert!(
        first
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    // Enabling again keeps it.
    assert_eq!(anna.ctx.enable_report_token().unwrap(), first);
    assert_eq!(anna.ctx.report_token().unwrap(), Some(first.clone()));
    assert!(anna.ctx.report_token_matches(&first).unwrap());
    assert!(
        anna.ctx
            .report_token_matches(&first.to_uppercase())
            .unwrap()
    );
    assert!(!anna.ctx.report_token_matches(&first[..63]).unwrap());
    assert!(!anna.ctx.report_token_matches("").unwrap());
    assert!(!anna.ctx.report_token_matches(&"é".repeat(32)).unwrap());

    let second = anna.ctx.renew_report_token().unwrap();
    assert_ne!(second, first);
    assert!(!anna.ctx.report_token_matches(&first).unwrap());
    assert!(anna.ctx.report_token_matches(&second).unwrap());

    anna.ctx.disable_report_token().unwrap();
    assert_eq!(anna.ctx.report_token().unwrap(), None);
    assert!(!anna.ctx.report_token_matches(&second).unwrap());
}

#[tokio::test]
async fn report_wait_follows_state_and_arrival() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    anna.ctx.set_report_setting("Bert", true, None).unwrap();
    assert_eq!(anna.ctx.report_wait().unwrap(), None);
    set_state(&anna, "schläft", 480, world.now());
    // A state alone: no request, nothing to wait for.
    assert_eq!(anna.ctx.report_wait().unwrap(), None);

    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    assert_eq!(anna.ctx.report_wait().unwrap(), Some(15 * MINUTE));
    world.advance(5 * MINUTE);
    assert_eq!(anna.ctx.report_wait().unwrap(), Some(10 * MINUTE));
    world.advance(10 * MINUTE);
    assert_eq!(anna.ctx.report_wait().unwrap(), Some(0));
}
