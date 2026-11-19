//! Several devices of one person: Anna's phone ("Handy") adds her laptop;
//! Bert is her contact. Both flows of adding run side by side on one
//! thread, with the manual clock advanced by every pause.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lz_core::crypto;
use lz_core::handover::{Adding, NewDevice, challenge_slot, handover_slot};
use lz_core::pairing::Address;
use lz_core::store::{Device, Group};
use lz_core::wire::{DeviceCode, ENVELOPE_LEN, Hash, ResponseKind, Token, device_code};
use rand_core::OsRng;

use super::{
    HOUR, MINUTE, Person, RELAY_A, RELAY_B, TOKEN_A, TOKEN_B, World, expect_err, paired, url,
};
use crate::ctx::Fail;
use crate::devices::{Check, Dialog};
use crate::relay::fake::{FakeRelay, Origin};
use crate::relay::{Relay, RelayError};
use crate::show::Notice;

/// Seconds the clock moves on every pause.
const STEP: u64 = 10;

/// What both sides showed, shared between the dialogs of one adding.
#[derive(Default)]
struct Log {
    /// The device code of the new device.
    code: Option<String>,
    /// The device check code the new device showed.
    new_device: Option<String>,
    /// The handover check code the existing device showed.
    old_handover: Option<String>,
    /// Every question with its code, in order.
    asked: Vec<(Check, String)>,
    /// An attacker has stored its slot.
    planted: bool,
}

type Shared = Rc<RefCell<Log>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    New,
    Existing,
}

/// Answers like a careful person: yes exactly if the other device showed
/// the same code.
struct TestDialog {
    role: Role,
    log: Shared,
    clock: Arc<AtomicU64>,
    relay: FakeRelay,
    /// Answer this instead of comparing.
    answer: Option<bool>,
    /// Takes this relay down while answering.
    down_on_confirm: Option<&'static str>,
    /// Brings this relay up on every pause.
    up_on_pause: Option<&'static str>,
    /// Answers only once an attacker has planted its slot.
    wait_for_plant: bool,
}

impl World {
    fn dialog(&self, role: Role, log: &Shared) -> TestDialog {
        TestDialog {
            role,
            log: log.clone(),
            clock: self.clock.clone(),
            relay: self.relay.clone(),
            answer: None,
            down_on_confirm: None,
            up_on_pause: None,
            wait_for_plant: false,
        }
    }
}

async fn tick(clock: &AtomicU64) {
    clock.fetch_add(STEP, Ordering::SeqCst);
    tokio::task::yield_now().await;
}

impl Dialog for TestDialog {
    fn device_code(&mut self, code: &str) {
        self.log.borrow_mut().code = Some(code.into());
    }

    fn check_code(&mut self, check: Check, code: &str) {
        let mut log = self.log.borrow_mut();
        match (self.role, check) {
            (Role::New, Check::Device) => log.new_device = Some(code.into()),
            (Role::Existing, Check::Handover) => log.old_handover = Some(code.into()),
            _ => panic!("shown on the wrong side"),
        }
    }

    async fn confirm(&mut self, check: Check, code: &str) -> bool {
        while self.wait_for_plant && !self.log.borrow().planted {
            tick(&self.clock).await;
        }
        let expected = match (self.role, check) {
            (Role::Existing, Check::Device) => Check::Device,
            (Role::New, Check::Handover) => Check::Handover,
            _ => panic!("asked on the wrong side"),
        };
        assert_eq!(check, expected);
        self.log.borrow_mut().asked.push((check, code.into()));
        if let Some(relay) = self.down_on_confirm {
            self.relay.set_down(relay, true);
        }
        if let Some(answer) = self.answer {
            return answer;
        }
        let log = self.log.borrow();
        let other = match self.role {
            Role::Existing => &log.new_device,
            Role::New => &log.old_handover,
        };
        other.as_deref() == Some(code)
    }

    async fn pause(&mut self) {
        if let Some(relay) = self.up_on_pause {
            self.relay.set_down(relay, false);
        }
        tick(&self.clock).await;
    }
}

/// Waits until the new device has shown its code.
async fn code_of(log: &Shared) -> String {
    loop {
        if let Some(code) = log.borrow().code.clone() {
            return code;
        }
        tokio::task::yield_now().await;
    }
}

fn text(code: &DeviceCode) -> String {
    code.to_text(&mut [0; device_code::MAX_TEXT_LEN]).to_owned()
}

/// `new` joins on `relay` while `old` adds it, with the given dialogs.
/// Returns the results of adding and of joining.
async fn join_and_add(
    old: &mut Person,
    new: &mut Person,
    relay: &str,
    mut old_dialog: TestDialog,
    mut new_dialog: TestDialog,
) -> (Result<(), Fail>, Result<(), Fail>) {
    let log = old_dialog.log.clone();
    let join = new.ctx.join_device(relay, "Laptop", None, &mut new_dialog);
    let add = async {
        let code = code_of(&log).await;
        old.ctx.add_device(&code, "Handy", &mut old_dialog).await
    };
    let (joined, added) = tokio::join!(join, add);
    (added, joined)
}

/// Anna's phone paired with Bert, then her laptop added.
pub(super) async fn family(world: &World) -> (Person, Person, Person) {
    let (mut phone, bert) = paired(world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    let (added, joined) = join_and_add(
        &mut phone,
        &mut laptop,
        RELAY_A,
        world.dialog(Role::Existing, &log),
        world.dialog(Role::New, &log),
    )
    .await;
    added.unwrap();
    joined.unwrap();
    phone.out.take();
    laptop.out.take();
    (phone, laptop, bert)
}

fn bert_mailbox(phone: &Person) -> lz_core::wire::MailboxId {
    phone.contact("Bert").state.contact.own_mailbox
}

/// Fetches a slot as soon as it is there.
async fn take(world: &World, relay: &str, slot: &Hash, token: &Token) -> Vec<u8> {
    loop {
        match world.relay.get_slot(&url(relay), slot, token).await {
            Ok(data) => return data,
            Err(RelayError::UnknownHandover) => tick(&world.clock).await,
            Err(error) => panic!("{error}"),
        }
    }
}

/// A handover sealed to `code` by someone with a group of its own.
fn forged_handover(code: &DeviceCode, challenge: [u8; 32]) -> Vec<u8> {
    let mut mallory = Device::new(&mut OsRng);
    mallory.group = Some(Group::new(
        Address {
            relay: url(RELAY_A),
            mailbox: [9; 32],
            send_token: [8; 32],
        },
        [7; 32],
        &mut OsRng,
    ));
    let handover = mallory.handover(challenge).unwrap();
    crypto::seal_bytes(&handover.to_json(), &code.handover_key, &mut OsRng).unwrap()
}

#[tokio::test]
async fn full_adding_flow_hands_over_everything() {
    let world = World::new();
    let (mut phone, mut bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    let (added, joined) = join_and_add(
        &mut phone,
        &mut laptop,
        RELAY_A,
        world.dialog(Role::Existing, &log),
        world.dialog(Role::New, &log),
    )
    .await;
    added.unwrap();
    joined.unwrap();

    // Each side confirmed once, with the code the other side showed.
    let asked = log.borrow().asked.clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(Some(&asked[0].1), log.borrow().new_device.as_ref());
    assert_eq!(Some(&asked[1].1), log.borrow().old_handover.as_ref());

    let on_phone = phone.device();
    let on_laptop = laptop.device();
    assert_eq!(on_phone.name.as_deref(), Some("Handy"));
    assert_eq!(on_laptop.name.as_deref(), Some("Laptop"));
    assert_eq!(on_phone.devices.len(), 1);
    assert_eq!(on_phone.devices[0].name, "Laptop");
    assert_eq!(on_phone.devices[0].reader, on_laptop.reader_id);
    assert_eq!(on_laptop.devices.len(), 1);
    assert_eq!(on_laptop.devices[0].name, "Handy");
    assert_eq!(on_laptop.devices[0].reader, on_phone.reader_id);
    let group = on_phone.group.clone().unwrap();
    assert_eq!(on_laptop.group.as_ref().unwrap().mailbox, group.mailbox);
    assert_eq!(
        world.relay.origin(RELAY_A, &group.mailbox),
        Origin::AccessToken
    );
    assert_eq!(
        on_laptop.relay.as_ref().unwrap().access_token,
        Some(TOKEN_A)
    );
    assert_eq!(on_laptop.contacts.len(), 1);
    assert_eq!(on_laptop.contacts[0].name, "Bert");
    for reader in [&on_phone.reader_id, &on_laptop.reader_id] {
        assert!(world.relay.has_reader(RELAY_A, &group.mailbox, reader));
        assert!(
            world
                .relay
                .has_reader(RELAY_A, &bert_mailbox(&phone), reader)
        );
    }
    // Nothing is left in the slots.
    let code = DeviceCode::from_text(log.borrow().code.as_ref().unwrap()).unwrap();
    let challenge = challenge_slot(&code.handover_key);
    for slot in [
        challenge,
        lz_core::handover::reveal_slot(&challenge),
        handover_slot(&code.handover_key),
    ] {
        assert!(!world.relay.has_slot(RELAY_A, &slot));
    }
    assert!(
        phone
            .out
            .take()
            .contains("Eigenes Gerät „Laptop“ ist dabei")
    );
    let out = laptop.out.take();
    assert!(out.contains("LZD1-"), "{out}");
    assert!(
        out.contains("eingerichtet als „Laptop“, mit 1 Kontakt."),
        "{out}"
    );

    // Both devices get Bert's requests.
    bert.ctx.request("Anna").await.unwrap();
    let request = Notice::Request {
        name: "Bert".into(),
    };
    assert!(phone.sync().await.contains(&request));
    assert!(laptop.sync().await.contains(&request));

    phone.ctx.list_devices().unwrap();
    let out = phone.out.take();
    assert!(
        out.contains("„Handy“ (dieses Gerät)") && out.contains("„Laptop“"),
        "{out}"
    );

    // A device with state cannot join.
    let mut dialog = world.dialog(Role::New, &log);
    expect_err(
        phone
            .ctx
            .join_device(RELAY_A, "Noch eins", None, &mut dialog)
            .await,
        "gibt es schon ein Gerät",
    );
}

#[tokio::test]
async fn answer_on_one_device_withdraws_the_request_on_the_other() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    let request = Notice::Request {
        name: "Bert".into(),
    };
    assert_eq!(phone.sync().await, vec![request.clone()]);
    assert_eq!(laptop.sync().await, vec![request]);

    phone.ctx.respond("Bert").await.unwrap();
    assert_eq!(
        laptop.sync().await,
        vec![Notice::OwnResponded {
            name: "Bert".into(),
            device: Some("Handy".into()),
            kind: ResponseKind::Manual,
        }]
    );
    assert!(
        laptop
            .out
            .take()
            .contains("Anfrage von Bert auf „Handy“ beantwortet.")
    );
    expect_err(laptop.ctx.respond("Bert").await, "Keine offene Anfrage");
    let overview = laptop.ctx.overview().unwrap();
    assert!(overview.contacts[0].incoming.is_none());
    // Bert gets exactly one response.
    assert_eq!(bert.sync().await.len(), 1);
    // The minimum interval started on the laptop as well.
    assert!(laptop.contact("Bert").state.incoming.last_answer.is_some());
}

#[tokio::test]
async fn weekly_limit_counts_across_both_devices() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    bert.ctx
        .rules("Anna", Some(0), None, Some(2))
        .await
        .unwrap();
    phone.sync().await;
    laptop.sync().await;

    phone.ctx.request("Bert").await.unwrap();
    bert.sync().await;
    bert.ctx.respond("Anna").await.unwrap();
    let notices = laptop.sync().await;
    assert!(notices.contains(&Notice::OwnRequested {
        name: "Bert".into(),
        device: Some("Handy".into()),
    }));
    world.advance(MINUTE);
    laptop.ctx.request("Bert").await.unwrap();
    bert.sync().await;
    bert.ctx.respond("Anna").await.unwrap();
    phone.sync().await;
    laptop.sync().await;

    world.advance(MINUTE);
    expect_err(phone.ctx.request("Bert").await, "wieder möglich ab");
    expect_err(laptop.ctx.request("Bert").await, "wieder möglich ab");
}

#[tokio::test]
async fn response_to_a_request_from_the_other_device_is_accepted() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    phone.ctx.request("Bert").await.unwrap();
    assert_eq!(
        laptop.sync().await,
        vec![Notice::OwnRequested {
            name: "Bert".into(),
            device: Some("Handy".into()),
        }]
    );
    bert.sync().await;
    bert.ctx.respond("Anna").await.unwrap();
    let response = Notice::Response {
        name: "Bert".into(),
        kind: ResponseKind::Manual,
        at: world.now(),
    };
    assert_eq!(laptop.sync().await, vec![response.clone()]);
    assert_eq!(phone.sync().await, vec![response]);
    // The phone's own copy of `requested` was dropped as a duplicate.
    assert!(phone.contact("Bert").state.outgoing.entries.len() == 1);
}

#[tokio::test]
async fn later_rules_win_on_own_devices_and_at_the_contact() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    phone.ctx.rules("Bert", Some(30), None, None).await.unwrap();
    world.advance(MINUTE);
    // The laptop has not seen the phone's change yet.
    laptop
        .ctx
        .rules("Bert", Some(90), None, None)
        .await
        .unwrap();

    let notices = phone.sync().await;
    assert!(
        notices.iter().any(
            |n| matches!(n, Notice::OwnRules { rules, .. } if rules.min_interval_minutes == 90)
        ),
        "{notices:?}"
    );
    // The older one from the phone is dropped on the laptop.
    assert!(
        !laptop
            .sync()
            .await
            .iter()
            .any(|n| matches!(n, Notice::OwnRules { .. }))
    );
    bert.sync().await;
    assert_eq!(
        phone.contact("Bert").state.own_rules.min_interval_minutes,
        90
    );
    assert_eq!(
        laptop.contact("Bert").state.own_rules.min_interval_minutes,
        90
    );
    assert_eq!(
        bert.contact("Anna").state.peer_rules.min_interval_minutes,
        90
    );
    assert_eq!(
        phone.contact("Bert").state.own_rules_stamp,
        bert.contact("Anna").state.peer_rules_stamp
    );
}

#[tokio::test]
async fn contact_paired_on_one_device_arrives_on_the_other() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;

    // The laptop invites Carl, with the relay account it was handed.
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = laptop.ctx.invite("Carl", false).await.unwrap();
    carl.ctx.accept("Anna", &code).await.unwrap();
    assert!(matches!(
        laptop.sync().await.as_slice(),
        [Notice::Accepted { .. }]
    ));
    laptop.ctx.confirm("Carl").await.unwrap();
    assert_eq!(
        phone.sync().await,
        vec![Notice::OwnContactAdded {
            name: "Carl".into(),
            device: Some("Laptop".into()),
        }]
    );
    let on_phone = phone.contact("Carl");
    assert!(on_phone.state.is_paired());
    let mailbox = on_phone.state.contact.own_mailbox;
    assert!(
        world
            .relay
            .has_reader(RELAY_A, &mailbox, &phone.device().reader_id)
    );
    assert!(matches!(
        carl.sync().await.as_slice(),
        [
            Notice::Revealed { .. },
            Notice::Paired { .. },
            Notice::Rules { .. }
        ]
    ));
    carl.ctx.request("Anna").await.unwrap();
    let request = Notice::Request {
        name: "Carl".into(),
    };
    // The phone became a reader before Carl sent his rules, so it gets
    // them as well.
    let rules = Notice::Rules {
        name: "Carl".into(),
        rules: lz_core::wire::Rules::DEFAULT,
    };
    assert_eq!(phone.sync().await, vec![rules.clone(), request.clone()]);
    assert_eq!(laptop.sync().await, vec![rules, request]);

    // The phone accepts Dora's invitation; the pairing completes when
    // `hello_ok` arrives on the phone.
    let mut dora = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = dora.ctx.invite("Anna", false).await.unwrap();
    phone.ctx.accept("Dora", &code).await.unwrap();
    dora.sync().await;
    dora.ctx.confirm("Anna").await.unwrap();
    assert!(matches!(
        phone.sync().await.as_slice(),
        [
            Notice::Revealed { .. },
            Notice::Paired { .. },
            Notice::Rules { .. }
        ]
    ));
    let notices = laptop.sync().await;
    assert!(
        notices.contains(&Notice::OwnContactAdded {
            name: "Dora".into(),
            device: Some("Handy".into()),
        }),
        "{notices:?}"
    );
    assert_eq!(
        laptop.contact("Dora").state.own_rules_stamp,
        phone.contact("Dora").state.own_rules_stamp
    );
    dora.ctx.request("Anna").await.unwrap();
    assert!(laptop.sync().await.contains(&Notice::Request {
        name: "Dora".into()
    }));
}

#[tokio::test]
async fn only_the_inviting_device_holds_the_secret_value() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = laptop.ctx.invite("Carl", false).await.unwrap();
    carl.ctx.accept("Anna", &code).await.unwrap();
    assert!(matches!(
        laptop.sync().await.as_slice(),
        [Notice::Accepted { .. }]
    ));
    assert!(laptop.contact("Carl").state.contact.secret.is_some());
    assert!(matches!(
        carl.sync().await.as_slice(),
        [Notice::Revealed { .. }]
    ));
    // Until the pairing is complete, the phone knows nothing of Carl.
    assert!(phone.sync().await.is_empty());
    assert!(phone.device().contacts.iter().all(|c| c.name != "Carl"));
    let text = std::fs::read_to_string(phone.ctx.store.path()).unwrap();
    assert!(!text.contains("secret"), "{text}");

    laptop.ctx.confirm("Carl").await.unwrap();
    assert_eq!(laptop.contact("Carl").state.contact.secret, None);
    assert_eq!(
        phone.sync().await,
        vec![Notice::OwnContactAdded {
            name: "Carl".into(),
            device: Some("Laptop".into()),
        }]
    );
    let on_phone = phone.contact("Carl").state;
    assert!(on_phone.is_paired());
    assert_eq!(on_phone.contact.secret, None);
    assert!(on_phone.contact.awaiting_reveal.is_none());

    // On the accepting side, too: the phone accepts Dora's invitation and
    // the laptop learns of her only after `hello_ok`.
    let mut dora = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = dora.ctx.invite("Anna", false).await.unwrap();
    phone.ctx.accept("Dora", &code).await.unwrap();
    dora.sync().await;
    assert!(matches!(
        phone.sync().await.as_slice(),
        [Notice::Revealed { .. }]
    ));
    assert!(phone.contact("Dora").state.contact.secret.is_some());
    let notices = laptop.sync().await;
    assert!(
        !notices
            .iter()
            .any(|n| matches!(n, Notice::OwnContactAdded { name, .. } if name == "Dora")),
        "{notices:?}"
    );
    dora.ctx.confirm("Anna").await.unwrap();
    phone.sync().await;
    assert!(laptop.sync().await.contains(&Notice::OwnContactAdded {
        name: "Dora".into(),
        device: Some("Handy".into()),
    }));
    assert_eq!(laptop.contact("Dora").state.contact.secret, None);
}

#[tokio::test]
async fn removed_contact_is_removed_on_the_other_device() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    phone.ctx.remove("Bert").await.unwrap();
    let notices = laptop.sync().await;
    assert_eq!(
        notices,
        vec![Notice::OwnContactRemoved {
            name: "Bert".into(),
            device: Some("Handy".into()),
        }]
    );
    assert!(laptop.device().contacts.is_empty());
    assert_eq!(
        bert.sync().await,
        vec![Notice::Ended {
            name: "Anna".into()
        }]
    );
}

#[tokio::test]
async fn renamed_device_is_known_by_its_new_name_elsewhere() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    // The name of another own device is taken.
    assert!(phone.ctx.rename_device("Laptop").await.is_err());
    phone.ctx.rename_device("Phone").await.unwrap();
    assert_eq!(phone.device().name.as_deref(), Some("Phone"));
    laptop.sync().await;
    let names: Vec<_> = laptop
        .device()
        .devices
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(names, vec!["Phone".to_owned()]);
}

#[tokio::test]
async fn removed_device_deletes_its_state() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let reader = laptop.device().reader_id;
    let group = phone.device().group.unwrap();
    expect_err(phone.ctx.remove_device("Handy").await, "ist dieses Gerät");
    expect_err(
        phone.ctx.remove_device("Tablet").await,
        "Kein eigenes Gerät",
    );

    // The app removes by reader identifier, because names need not be
    // unique; this device itself is refused that way as well.
    let own = phone.ctx.devices().unwrap();
    let this = own.iter().find(|d| d.this).unwrap().reader.clone();
    expect_err(
        phone.ctx.remove_device_by_reader(&this).await,
        "dieses Gerät",
    );
    let other = own.iter().find(|d| !d.this).unwrap().reader.clone();
    phone.ctx.remove_device_by_reader(&other).await.unwrap();
    assert!(phone.out.take().contains("„Laptop“ entfernt."));
    assert!(phone.device().devices.is_empty());
    assert!(!world.relay.has_reader(RELAY_A, &group.mailbox, &reader));
    assert!(
        !world
            .relay
            .has_reader(RELAY_A, &bert_mailbox(&phone), &reader)
    );

    let path = laptop.ctx.store.path().to_owned();
    let notices = laptop.ctx.sync().await.unwrap();
    assert!(notices.contains(&Notice::Removed));
    assert!(laptop.out.take().contains("Dieses Gerät wurde entfernt."));
    assert!(!path.exists());
    assert!(!path.with_extension("json.lock").exists());
    expect_err(laptop.ctx.sync().await.map(drop), "Kein Gerät");

    // The same through `device_removed`, while still registered.
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let reader = laptop.device().reader_id;
    let now = world.now();
    phone
        .ctx
        .update(|device| {
            device
                .sync_device_removed(&reader, now, &mut OsRng)
                .map_err(|e| Fail(e.to_string()))
        })
        .unwrap();
    phone.sync().await;
    let notices = laptop.ctx.sync().await.unwrap();
    assert!(notices.contains(&Notice::Removed));
    assert!(!laptop.ctx.store.path().exists());
}

#[tokio::test]
async fn swapped_device_code_is_noticed_and_nothing_handed_over() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();

    // Mallory shows the phone her own device code instead of the laptop's,
    // and plays the existing device towards the laptop.
    let mut mallory = NewDevice::create(url(RELAY_A), world.now(), &mut OsRng);
    let mallory_code = text(&mallory.code());
    let mallory_handover = mallory.handover_slot();
    let attacker = async {
        let laptop_code = DeviceCode::from_text(&code_of(&log).await).unwrap();
        let mut towards_laptop =
            Adding::start(laptop_code.clone(), world.now(), &mut OsRng).unwrap();
        world
            .relay
            .put_slot(
                &url(RELAY_A),
                &towards_laptop.challenge_slot(),
                &TOKEN_A,
                &laptop_code.fetch_hash,
                Some(&towards_laptop.reply_hash()),
                &towards_laptop.challenge.to_bytes(),
            )
            .await
            .unwrap();
        let n = take(
            &world,
            RELAY_A,
            &towards_laptop.reveal_slot(),
            &towards_laptop.challenge.reply_token,
        )
        .await;
        let on_laptop = towards_laptop.check_reveal(&n).unwrap();
        let challenge = take(
            &world,
            RELAY_A,
            &mallory.challenge_slot(),
            &mallory.fetch_token,
        )
        .await;
        let reply = mallory.take_challenge(&challenge, world.now()).unwrap();
        world
            .relay
            .put_reveal(
                &url(RELAY_A),
                &mallory.reveal_slot(),
                &reply,
                &mallory.secret,
            )
            .await
            .unwrap();
        (on_laptop, mallory.check_code().unwrap(), laptop_code.reader)
    };
    let mut new_dialog = world.dialog(Role::New, &log);
    let mut old_dialog = world.dialog(Role::Existing, &log);
    let join = laptop
        .ctx
        .join_device(RELAY_A, "Laptop", None, &mut new_dialog);
    let add = phone
        .ctx
        .add_device(&mallory_code, "Handy", &mut old_dialog);
    let (joined, added, (on_laptop, on_mallory, laptop_reader)) = tokio::join!(join, add, attacker);

    let log = log.borrow();
    // Mallory and the phone agree, but the laptop shows other digits, so
    // the person says no.
    assert_eq!(log.asked, vec![(Check::Device, on_mallory.clone())]);
    assert_eq!(log.new_device.as_ref(), Some(&on_laptop));
    assert_ne!(on_laptop, on_mallory);
    expect_err(added, "Abgebrochen; nichts wurde übergeben.");
    expect_err(joined, "nichts übergeben");
    assert!(!laptop.ctx.store.path().exists());
    assert!(phone.device().group.is_none());
    assert!(!world.relay.has_slot(RELAY_A, &mallory_handover));
    assert!(
        !world
            .relay
            .has_reader(RELAY_A, &bert_mailbox(&phone), &laptop_reader)
    );
}

#[tokio::test]
async fn occupied_challenge_or_reveal_slot_is_an_alarm() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let log = Shared::default();
    let mut dialog = world.dialog(Role::Existing, &log);

    // Someone who saw the code stored something in the challenge slot.
    let new = NewDevice::create(url(RELAY_A), world.now(), &mut OsRng);
    world
        .relay
        .put_slot(
            &url(RELAY_A),
            &new.challenge_slot(),
            &TOKEN_A,
            &[1; 32],
            Some(&[2; 32]),
            b"x",
        )
        .await
        .unwrap();
    let error = phone
        .ctx
        .add_device(&text(&new.code()), "Handy", &mut dialog)
        .await
        .unwrap_err();
    assert!(error.0.starts_with("Achtung:"), "{error:?}");
    assert!(error.0.contains("schon etwas"), "{error:?}");

    // Or in the reveal slot that belongs to it.
    let new = NewDevice::create(url(RELAY_A), world.now(), &mut OsRng);
    world
        .relay
        .put_slot(
            &url(RELAY_A),
            &new.reveal_slot(),
            &TOKEN_B_ON_A,
            &[1; 32],
            None,
            b"x",
        )
        .await
        .unwrap_err();
    // Only tokens of the relay may store; with the access token it works.
    world
        .relay
        .put_slot(
            &url(RELAY_A),
            &new.reveal_slot(),
            &TOKEN_A,
            &[1; 32],
            None,
            b"x",
        )
        .await
        .unwrap();
    let error = phone
        .ctx
        .add_device(&text(&new.code()), "Handy", &mut dialog)
        .await
        .unwrap_err();
    assert!(error.0.contains("schon etwas"), "{error:?}");
    assert!(log.borrow().asked.is_empty());
    assert!(phone.device().group.is_none());
}

/// A token relay A does not know.
const TOKEN_B_ON_A: Token = TOKEN_B;

#[tokio::test]
async fn planted_handover_with_wrong_challenge_is_rejected() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    let attacker = async {
        let code = DeviceCode::from_text(&code_of(&log).await).unwrap();
        world
            .relay
            .put_slot(
                &url(RELAY_A),
                &handover_slot(&code.handover_key),
                &TOKEN_A,
                &code.fetch_hash,
                None,
                &forged_handover(&code, [0x55; 32]),
            )
            .await
            .unwrap();
        log.borrow_mut().planted = true;
        code.reader
    };
    let mut old_dialog = world.dialog(Role::Existing, &log);
    old_dialog.wait_for_plant = true;
    let mut new_dialog = world.dialog(Role::New, &log);
    let join = laptop
        .ctx
        .join_device(RELAY_A, "Laptop", None, &mut new_dialog);
    let add = async {
        let code = code_of(&log).await;
        phone.ctx.add_device(&code, "Handy", &mut old_dialog).await
    };
    let (joined, added, reader) = tokio::join!(join, add, attacker);

    // Depending on who is first at the slot, the phone finds it taken
    // (alarm) or stores its own after the laptop fetched the planted one;
    // then the laptop never announces itself. Either way adding fails.
    let error = added.unwrap_err();
    assert!(
        error.0.contains("hingelegt") || error.0.contains("nicht rechtzeitig gemeldet"),
        "{error:?}"
    );
    let error = joined.unwrap_err();
    assert!(
        error.0.starts_with("Achtung:") && error.0.contains("stammt nicht"),
        "{error:?}"
    );
    assert!(!laptop.ctx.store.path().exists());
    // The phone took everything back.
    let device = phone.device();
    assert!(device.group.is_none() && device.devices.is_empty());
    assert!(
        !world
            .relay
            .has_reader(RELAY_A, &bert_mailbox(&phone), &reader)
    );
}

#[tokio::test]
async fn planted_handover_from_the_relay_operator_fails_the_handover_check() {
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    // The operator sees the challenge and so can plant a handover that
    // carries it, but with a group key of its own.
    let attacker = async {
        let code = DeviceCode::from_text(&code_of(&log).await).unwrap();
        let slot = challenge_slot(&code.handover_key);
        let challenge = loop {
            if let Some(data) = world.relay.seen_slot(RELAY_A, &slot) {
                break <[u8; 32]>::try_from(&data[..32]).unwrap();
            }
            tick(&world.clock).await;
        };
        world
            .relay
            .put_slot(
                &url(RELAY_A),
                &handover_slot(&code.handover_key),
                &TOKEN_A,
                &code.fetch_hash,
                None,
                &forged_handover(&code, challenge),
            )
            .await
            .unwrap();
        log.borrow_mut().planted = true;
    };
    let mut old_dialog = world.dialog(Role::Existing, &log);
    old_dialog.wait_for_plant = true;
    let mut new_dialog = world.dialog(Role::New, &log);
    let join = laptop
        .ctx
        .join_device(RELAY_A, "Laptop", None, &mut new_dialog);
    let add = async {
        let code = code_of(&log).await;
        phone.ctx.add_device(&code, "Handy", &mut old_dialog).await
    };
    let (joined, added, ()) = tokio::join!(join, add, attacker);

    assert!(added.is_err());
    // The laptop opened it and asked, but its handover check code is not
    // the one the phone shows (if it got to show one), so the person said
    // no.
    let log = log.borrow();
    assert_eq!(log.asked.len(), 2);
    let (_, shown) = log
        .asked
        .iter()
        .find(|(check, _)| *check == Check::Handover)
        .unwrap();
    assert_ne!(log.old_handover.as_ref(), Some(shown));
    let device = phone.device();
    assert!(device.group.is_none() && device.devices.is_empty());
    expect_err(
        joined,
        "Abgebrochen; dieses Gerät wurde nicht eingerichtet.",
    );
    assert!(!laptop.ctx.store.path().exists());
}

#[tokio::test]
async fn full_sync_mailbox_is_retried_and_dropped_after_a_day() {
    let world = World::new();
    let (mut phone, mut laptop, _bert) = family(&world).await;
    let group = phone.device().group.unwrap();
    let fill = async || {
        // Until the relay refuses: 32 messages at most.
        while world
            .relay
            .deliver(
                &group.relay,
                &group.mailbox,
                &group.send_token,
                &[0; ENVELOPE_LEN],
            )
            .await
            .is_ok()
        {}
    };
    fill().await;
    phone.ctx.request("Bert").await.unwrap();
    assert!(phone.out.take().contains("noch nicht zugestellt"));
    assert_eq!(phone.device().outbox.len(), 1);
    // Still full: the laptop has not fetched the waiting messages.
    phone.sync().await;
    assert_eq!(phone.device().outbox.len(), 1);
    assert!(laptop.sync().await.is_empty());
    assert_eq!(world.relay.waiting(RELAY_A, &group.mailbox), 0);
    // The next fetch sends it.
    phone.sync().await;
    assert!(phone.device().outbox.is_empty());
    assert_eq!(
        laptop.sync().await,
        vec![Notice::OwnRequested {
            name: "Bert".into(),
            device: Some("Handy".into()),
        }]
    );

    // After 24 hours it is dropped.
    fill().await;
    phone.ctx.rules("Bert", Some(10), None, None).await.unwrap();
    assert_eq!(phone.device().outbox.len(), 1);
    world.advance(25 * HOUR);
    phone.sync().await;
    assert!(phone.device().outbox.is_empty());
    laptop.sync().await;
    assert_ne!(
        laptop.contact("Bert").state.own_rules.min_interval_minutes,
        10
    );
}

#[tokio::test]
async fn without_access_token_the_sync_mailbox_is_derived() {
    let world = World::new();
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let mut phone = world.person(None);
    let code = bert.ctx.invite("Anna", true).await.unwrap();
    phone.ctx.accept("Bert", &code).await.unwrap();
    bert.sync().await;
    bert.ctx.confirm("Anna").await.unwrap();
    phone.sync().await;
    let voucher_mailbox = phone.contact("Bert");
    assert_eq!(voucher_mailbox.own_relay, url(RELAY_B));

    // While its voucher mailbox has no room left, adding fails clearly.
    let mut derived = Vec::new();
    for _ in 0..10 {
        let id = world
            .relay
            .create_mailbox(
                &url(RELAY_B),
                &voucher_mailbox.read_token,
                &[1; 32],
                &[2; 32],
            )
            .await
            .unwrap();
        derived.push(id);
    }
    let mut laptop = world.blank();
    let log = Shared::default();
    let (added, joined) = join_and_add(
        &mut phone,
        &mut laptop,
        RELAY_B,
        world.dialog(Role::Existing, &log),
        world.dialog(Role::New, &log),
    )
    .await;
    expect_err(added, "Mehrere Geräte brauchen einen Zugangstoken");
    assert!(joined.is_err());
    assert!(!laptop.ctx.store.path().exists());
    for id in derived {
        world
            .relay
            .delete_mailbox(&url(RELAY_B), &id, &[1; 32])
            .await
            .unwrap();
    }

    let mut laptop = world.blank();
    let log = Shared::default();
    let (added, joined) = join_and_add(
        &mut phone,
        &mut laptop,
        RELAY_B,
        world.dialog(Role::Existing, &log),
        world.dialog(Role::New, &log),
    )
    .await;
    added.unwrap();
    joined.unwrap();
    let group = phone.device().group.unwrap();
    assert_eq!(group.relay, url(RELAY_B));
    assert_eq!(
        world.relay.origin(RELAY_B, &group.mailbox),
        Origin::Derived(voucher_mailbox.state.contact.own_mailbox)
    );
    // Without a relay account, none is handed over.
    assert!(laptop.device().relay.is_none());
    bert.ctx.request("Anna").await.unwrap();
    let request = Notice::Request {
        name: "Bert".into(),
    };
    assert_eq!(phone.sync().await, vec![request.clone()]);
    assert_eq!(laptop.sync().await, vec![request]);
}

#[tokio::test]
async fn adding_that_fails_halfway_leaves_nothing_behind() {
    // The relay goes down after the phone confirmed the device check code.
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    let mut old_dialog = world.dialog(Role::Existing, &log);
    old_dialog.down_on_confirm = Some(RELAY_A);
    let (added, joined) = join_and_add(
        &mut phone,
        &mut laptop,
        RELAY_A,
        old_dialog,
        world.dialog(Role::New, &log),
    )
    .await;
    world.relay.set_down(RELAY_A, false);
    expect_err(added, "nicht angelegt");
    expect_err(joined, "nicht eingerichtet");
    let reader = DeviceCode::from_text(log.borrow().code.as_ref().unwrap())
        .unwrap()
        .reader;
    assert!(!laptop.ctx.store.path().exists());
    let device = phone.device();
    assert!(device.group.is_none() && device.devices.is_empty());
    assert!(
        !world
            .relay
            .has_reader(RELAY_A, &bert_mailbox(&phone), &reader)
    );

    // The relay goes down after the laptop confirmed the handover, so its
    // announcement does not arrive; the phone takes everything back.
    let world = World::new();
    let (mut phone, _bert) = paired(&world).await;
    let mut laptop = world.blank();
    let log = Shared::default();
    let mut old_dialog = world.dialog(Role::Existing, &log);
    old_dialog.up_on_pause = Some(RELAY_A);
    let mut new_dialog = world.dialog(Role::New, &log);
    new_dialog.down_on_confirm = Some(RELAY_A);
    let (added, joined) =
        join_and_add(&mut phone, &mut laptop, RELAY_A, old_dialog, new_dialog).await;
    expect_err(joined, "nicht eingerichtet");
    expect_err(added, "nicht rechtzeitig gemeldet");
    let reader = DeviceCode::from_text(log.borrow().code.as_ref().unwrap())
        .unwrap()
        .reader;
    assert!(!laptop.ctx.store.path().exists());
    let device = phone.device();
    assert!(device.group.is_none() && device.devices.is_empty());
    assert!(
        !world
            .relay
            .has_reader(RELAY_A, &bert_mailbox(&phone), &reader)
    );
}

#[tokio::test]
async fn device_offline_for_hours_catches_up() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    bert.ctx.request("Anna").await.unwrap();
    phone.sync().await;
    phone.ctx.respond("Bert").await.unwrap();
    phone.ctx.rules("Bert", Some(30), None, None).await.unwrap();
    phone.ctx.request("Bert").await.unwrap();

    world.advance(20 * HOUR);
    let notices = laptop.sync().await;
    for expected in [
        Notice::OwnResponded {
            name: "Bert".into(),
            device: Some("Handy".into()),
            kind: ResponseKind::Manual,
        },
        Notice::OwnRequested {
            name: "Bert".into(),
            device: Some("Handy".into()),
        },
    ] {
        assert!(notices.contains(&expected), "{notices:?}");
    }
    // Bert's request arrived after its answer and is not shown as open.
    assert!(!notices.contains(&Notice::Request {
        name: "Bert".into()
    }));
    let state = laptop.contact("Bert").state;
    assert_eq!(state.open_request(world.now()), None);
    assert_eq!(state.own_rules.min_interval_minutes, 30);

    bert.sync().await;
    bert.ctx.respond("Anna").await.unwrap();
    assert!(
        laptop
            .sync()
            .await
            .iter()
            .any(|n| matches!(n, Notice::Response { .. }))
    );
}

#[tokio::test]
async fn shared_state_answers_from_the_other_device() {
    let world = World::new();
    let (mut phone, mut laptop, mut bert) = family(&world).await;
    laptop.ctx.set_report_setting("Bert", true, None).unwrap();
    phone.ctx.set_state("Akku leer", 1440, true).unwrap();
    // The fetch sends it; then the phone is off.
    assert!(phone.sync().await.is_empty());
    assert_eq!(
        laptop.sync().await,
        vec![Notice::OwnState {
            reason: Some("Akku leer".into()),
            device: Some("Handy".into()),
        }]
    );
    let state = laptop.ctx.overview().unwrap().state.unwrap();
    assert!(state.handed && !state.shared);

    bert.ctx.request("Anna").await.unwrap();
    assert_eq!(laptop.sync().await.len(), 1);
    world.advance(15 * MINUTE);
    assert_eq!(
        laptop.sync().await,
        vec![Notice::Reported {
            name: "Bert".into(),
            reason: "Akku leer".into()
        }]
    );
    assert!(matches!(
        &bert.sync().await[..],
        [Notice::Response {
            kind: ResponseKind::Reported(reason),
            ..
        }] if reason.as_str() == "Akku leer"
    ));

    // Charged again: cleared on the phone, and so on the laptop.
    phone.sync().await;
    phone.ctx.clear_state(true).unwrap();
    phone.sync().await;
    assert_eq!(
        laptop.sync().await,
        vec![Notice::OwnState {
            reason: None,
            device: Some("Handy".into()),
        }]
    );
    assert!(laptop.ctx.overview().unwrap().state.is_none());
}
