//! The M2 scenario end to end: two devices with their own state files talk
//! through fake relays.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lz_core::store::{Device, JsonFile, Store, StoredContact};
use lz_core::wire::{Reason, RelayUrl, ResponseKind, Rules, Token};

use crate::app::{self, Clock, Ctx, Fail};
use crate::relay::fake::FakeRelay;
use crate::relay::{Relay, b64};
use crate::show::Notice;

const T: u64 = 1_794_600_000;
const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const RELAY_A: &str = "https://relay-a.example";
const RELAY_B: &str = "https://relay-b.example";
const TOKEN_A: Token = [0xA1; 32];
const TOKEN_B: Token = [0xB2; 32];

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Output {
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}

/// Two or more devices sharing fake relays and one clock.
struct World {
    relay: FakeRelay,
    clock: Arc<AtomicU64>,
}

struct Person {
    ctx: Ctx<FakeRelay>,
    out: Output,
    _dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let relay = FakeRelay::default();
        relay.add(RELAY_A, TOKEN_A);
        relay.add(RELAY_B, TOKEN_B);
        Self {
            relay,
            clock: Arc::new(AtomicU64::new(T)),
        }
    }

    /// A device with its own state file, created by `lz init`.
    fn person(&self, relay: Option<(&str, Option<Token>)>) -> Person {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonFile::new(dir.path().join("sub").join("device.json"));
        let out = Output::default();
        let token = relay.and_then(|(_, t)| t).map(|t| b64(&t));
        app::init(
            &store,
            relay.map(|(url, _)| url),
            token.as_deref(),
            &mut out.clone(),
        )
        .unwrap();
        out.take();
        Person {
            ctx: Ctx {
                store,
                relay: self.relay.clone(),
                clock: Clock::Manual(self.clock.clone()),
                out: Box::new(out.clone()),
                verbose: true,
                idle: Duration::from_millis(20),
            },
            out,
            _dir: dir,
        }
    }

    fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
}

impl Person {
    fn device(&self) -> Device {
        self.ctx.store.load().unwrap()
    }

    fn contact(&self, name: &str) -> StoredContact {
        self.device()
            .contacts
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
    }

    /// `lz sync`, returning only notices that are not dropped messages.
    async fn sync(&mut self) -> Vec<Notice> {
        let notices = self.ctx.sync().await.unwrap();
        notices.into_iter().filter(|n| !n.is_dropped()).collect()
    }
}

fn url(text: &str) -> RelayUrl {
    RelayUrl::new(text).unwrap()
}

fn expect_err(result: Result<(), Fail>, part: &str) {
    let error = result.unwrap_err();
    assert!(error.0.contains(part), "{error:?} lacks {part:?}");
}

/// Anna (relay A) invites Bert (relay B); both confirm and exchange rules.
async fn paired(world: &World) -> (Person, Person) {
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));

    let code = anna.ctx.invite("Bert", false).await.unwrap();
    assert!(code.starts_with("LZ1-"));
    assert!(anna.out.take().contains(&code));
    let verification = bert.ctx.accept("Anna", &code).await.unwrap();
    assert_eq!(bert.contact("Anna").own_relay, url(RELAY_B));

    assert_eq!(
        anna.sync().await,
        vec![Notice::Accepted {
            name: "Bert".into(),
            code: verification.clone(),
        }]
    );
    assert!(anna.out.take().contains("lz confirm Bert"));
    assert!(anna.device().invitations.is_empty());
    anna.ctx.confirm("Bert").await.unwrap();

    assert_eq!(
        bert.sync().await,
        vec![
            Notice::Paired {
                name: "Anna".into()
            },
            Notice::Rules {
                name: "Anna".into(),
                rules: Rules::DEFAULT
            },
        ]
    );
    assert_eq!(
        anna.sync().await,
        vec![Notice::Rules {
            name: "Bert".into(),
            rules: Rules::DEFAULT
        }]
    );
    anna.out.take();
    bert.out.take();
    (anna, bert)
}

#[tokio::test]
async fn m2_scenario() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    let anna_mailbox = anna.contact("Bert").state.contact.own_mailbox;
    let bert_mailbox = bert.contact("Anna").state.contact.own_mailbox;

    // Request and manual response.
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
    // Acknowledged after storing, so the relay holds nothing anymore.
    assert_eq!(world.relay.waiting(RELAY_A, &anna_mailbox), 0);
    anna.ctx.contacts().unwrap();
    assert!(anna.out.take().contains("offene Anfrage seit"));
    anna.ctx.respond("Bert").await.unwrap();
    expect_err(anna.ctx.respond("Bert").await, "Keine offene Anfrage");
    assert_eq!(
        bert.sync().await,
        vec![Notice::Response {
            name: "Anna".into(),
            kind: ResponseKind::Manual,
            at: T,
        }]
    );

    // Minimum interval: refused on the asking side.
    world.advance(HOUR);
    expect_err(bert.ctx.request("Anna").await, "wieder möglich ab");
    bert.out.take();
    bert.ctx.contacts().unwrap();
    let listing = bert.out.take();
    assert!(listing.contains("Anfrage wieder möglich ab"), "{listing}");
    assert!(listing.contains("letzte Antwort: ✓"), "{listing}");

    // Weekly limit after changed rules.
    world.advance(HOUR);
    anna.ctx
        .rules("Bert", Some(0), Some(60), Some(2))
        .await
        .unwrap();
    let rules = Rules {
        min_interval_minutes: 0,
        expiry_minutes: 60,
        weekly_limit: 2,
    };
    assert_eq!(
        bert.sync().await,
        vec![Notice::Rules {
            name: "Anna".into(),
            rules
        }]
    );
    bert.ctx.request("Anna").await.unwrap();
    anna.sync().await;
    anna.ctx.respond("Bert").await.unwrap();
    bert.sync().await;
    world.advance(HOUR);
    expect_err(bert.ctx.request("Anna").await, "wieder möglich ab");
    assert_eq!(
        bert.contact("Anna").state.next_request_at(world.now()),
        T + 7 * 24 * HOUR
    );

    // Reported state, later a manual response.
    anna.ctx.request("Bert").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::Request {
            name: "Anna".into()
        }]
    );
    world.advance(15 * MINUTE);
    bert.ctx.report("Anna", "schläft").await.unwrap();
    expect_err(bert.ctx.report("Anna", "schläft").await, "schon");
    let reported_at = world.now();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Response {
            name: "Bert".into(),
            kind: ResponseKind::Reported(Reason::new("schläft").unwrap()),
            at: reported_at,
        }]
    );
    anna.out.take();
    anna.ctx.contacts().unwrap();
    assert!(
        anna.out
            .take()
            .contains("letzte Antwort: „schläft“ (gemeldet)")
    );
    world.advance(10 * MINUTE);
    bert.ctx.respond("Anna").await.unwrap();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Response {
            name: "Bert".into(),
            kind: ResponseKind::Manual,
            at: world.now(),
        }]
    );

    // Removal is visible to the other side.
    bert.ctx.remove("Anna").await.unwrap();
    assert!(bert.device().contacts.is_empty());
    assert!(!world.relay.has_mailbox(RELAY_B, &bert_mailbox));
    assert_eq!(
        anna.sync().await,
        vec![Notice::Ended {
            name: "Bert".into()
        }]
    );
    assert!(anna.out.take().contains("Bert hat die Verbindung beendet."));
    assert!(anna.contact("Bert").state.ended);
    assert!(!world.relay.has_mailbox(RELAY_A, &anna_mailbox));
    expect_err(anna.ctx.request("Bert").await, "beendet");
    anna.ctx.remove("Bert").await.unwrap();
    assert!(anna.device().contacts.is_empty());
}

#[tokio::test]
async fn voucher_lets_a_device_without_relay_pair() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut carl = world.person(None);

    let code = anna.ctx.invite("Carl", true).await.unwrap();
    carl.ctx.accept("Anna", &code).await.unwrap();
    // Carl's mailbox lives on Anna's relay.
    assert_eq!(carl.contact("Anna").own_relay, url(RELAY_A));
    assert!(matches!(
        anna.sync().await.as_slice(),
        [Notice::Accepted { .. }]
    ));
    anna.ctx.confirm("Carl").await.unwrap();
    assert!(matches!(
        carl.sync().await.as_slice(),
        [Notice::Paired { .. }, Notice::Rules { .. }]
    ));
    carl.ctx.request("Anna").await.unwrap();
    let notices = anna.sync().await;
    assert!(notices.contains(&Notice::Request {
        name: "Carl".into()
    }));

    // Without voucher, such a device cannot accept.
    let code = anna.ctx.invite("Dora", false).await.unwrap();
    let mut dora = world.person(None);
    let error = dora.ctx.accept("Anna", &code).await.unwrap_err();
    assert!(error.0.contains("Gutschein"), "{error:?}");
    assert!(dora.device().contacts.is_empty());
}

#[tokio::test]
async fn lost_end_is_detected_by_unknown_mailbox() {
    let world = World::new();
    let (mut anna, bert) = paired(&world).await;

    // Bert removes Anna, but `end` gets lost: only the mailbox disappears.
    let stored = bert.contact("Anna");
    world
        .relay
        .delete_mailbox(
            &stored.own_relay,
            &stored.state.contact.own_mailbox,
            &stored.read_token,
        )
        .await
        .unwrap();
    bert.ctx
        .store
        .update(|device| {
            device.contacts.clear();
            Ok::<_, io::Error>(())
        })
        .unwrap();

    expect_err(anna.ctx.request("Bert").await, "beendet");
    let contact = anna.contact("Bert");
    assert!(contact.state.ended);
    assert!(
        !world
            .relay
            .has_mailbox(RELAY_A, &contact.state.contact.own_mailbox)
    );
}

#[tokio::test]
async fn failed_delivery_changes_nothing() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;

    world.relay.set_down(RELAY_A, true);
    expect_err(bert.ctx.request("Anna").await, "Nicht zugestellt");
    assert!(bert.contact("Anna").state.outgoing.entries.is_empty());

    world.relay.set_down(RELAY_A, false);
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(
        anna.sync().await,
        vec![Notice::Request {
            name: "Bert".into()
        }]
    );
}

#[tokio::test]
async fn expired_invitation_is_refused_and_cleaned_up() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));

    let code = anna.ctx.invite("Bert", false).await.unwrap();
    let mailbox = anna.device().invitations[0].pending.own.mailbox;
    world.advance(11 * MINUTE);
    let error = bert.ctx.accept("Anna", &code).await.unwrap_err();
    assert!(error.0.contains("abgelaufen"), "{error:?}");

    // A `hello` sent in time may still arrive, so the invitation is kept
    // until the replay window has passed.
    anna.sync().await;
    assert_eq!(anna.device().invitations.len(), 1);
    world.advance(25 * HOUR);
    anna.sync().await;
    assert!(anna.device().invitations.is_empty());
    assert!(!world.relay.has_mailbox(RELAY_A, &mailbox));
    assert!(anna.out.take().contains("abgelaufen"));
}

#[tokio::test]
async fn acceptance_in_time_can_be_fetched_later() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));

    let code = anna.ctx.invite("Bert", false).await.unwrap();
    world.advance(9 * MINUTE);
    bert.ctx.accept("Anna", &code).await.unwrap();
    world.advance(5 * HOUR);
    anna.sync().await;
    assert!(anna.device().invitations.is_empty());
    assert_eq!(anna.device().contacts.len(), 1);
    assert!(anna.out.take().contains("Prüfcode"));
}

#[tokio::test]
async fn acknowledged_messages_do_not_come_again() {
    let world = World::new();
    let (mut anna, mut bert) = paired(&world).await;
    let stored = anna.contact("Bert");
    let mailbox = stored.state.contact.own_mailbox;
    // A second reader of the same mailbox, as with a second device.
    world
        .relay
        .put_reader(&url(RELAY_A), &mailbox, &stored.read_token, &[9; 16])
        .await
        .unwrap();
    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(anna.sync().await.len(), 1);
    // Kept for the other reader, but not delivered to Anna again.
    assert_eq!(world.relay.waiting(RELAY_A, &mailbox), 1);
    assert!(anna.sync().await.is_empty());
    assert!(anna.out.take().contains("Keine neuen Nachrichten."));
}

#[test]
fn init_checks_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFile::new(dir.path().join("device.json"));
    let mut out = Vec::new();
    assert!(app::init(&store, Some("ftp://x"), None, &mut out).is_err());
    assert!(app::init(&store, Some(RELAY_A), Some("kurz"), &mut out).is_err());
    assert!(app::init(&store, None, Some(&b64(&TOKEN_A)), &mut out).is_err());
    app::init(&store, Some("http://localhost:8080/"), None, &mut out).unwrap();
    assert_eq!(
        store.load().unwrap().relay.unwrap().url,
        url("http://localhost:8080")
    );
    let error = app::init(&store, None, None, &mut out).unwrap_err();
    assert!(error.0.contains("gibt es schon"));
}

#[tokio::test]
async fn intercepted_invitation_is_noticed_and_rejected_visibly() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let mut mallory = world.person(Some((RELAY_B, Some(TOKEN_B))));

    // Mallory intercepted the code and accepts before Bert does.
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    let mallory_code = mallory.ctx.accept("Anna", &code).await.unwrap();
    let bert_code = bert.ctx.accept("Anna", &code).await.unwrap();
    assert_ne!(mallory_code, bert_code);

    anna.sync().await;
    let out = anna.out.take();
    assert!(out.contains(&mallory_code), "{out}");
    assert!(out.contains("ein weiteres Mal angenommen"), "{out}");
    assert!(out.contains("lz reject Bert"), "{out}");

    // Anna compares with Bert, the codes differ, she rejects.
    anna.ctx.reject("Bert").await.unwrap();
    assert!(anna.device().contacts.is_empty());
    assert!(anna.out.take().contains("abgelehnt"));

    mallory.sync().await;
    assert!(mallory.out.take().contains("hat die Kopplung abgelehnt"));
    assert!(mallory.contact("Anna").state.ended);

    // Bert never gets `hello_ok`; he rejects on his side as well.
    bert.sync().await;
    bert.ctx.reject("Anna").await.unwrap();
    assert!(bert.device().contacts.is_empty());
    // Anna's mailbox is gone, so Bert is not told that she sees it.
    let out = bert.out.take();
    assert!(out.contains("abgelehnt") && !out.contains("sieht das"), "{out}");
}

#[tokio::test]
async fn reject_only_before_pairing_is_complete() {
    let world = World::new();
    let (mut anna, _bert) = paired(&world).await;
    let error = anna.ctx.reject("Bert").await.unwrap_err();
    assert!(error.0.contains("lz remove Bert"), "{error:?}");
}
