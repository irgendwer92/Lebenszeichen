//! Pairing with commitment and `reveal` (step 6b): codes only after a valid
//! `reveal`, lost and forged `reveal`, timeouts and leftovers of version 1.

use std::io;

use lz_core::crypto;
use lz_core::pairing::{HELLO_REPEAT_SECS, PairingState};
use lz_core::store::Store;
use lz_core::wire::{Body, Invitation, MAX_MESSAGE_AGE_SECS};

use super::{HOUR, MINUTE, Person, RELAY_A, RELAY_B, TOKEN_A, TOKEN_B, World, paired};
use crate::ctx::Status;
use crate::relay::Relay;
use crate::show::{COMPARE_HINT, Notice, OLD_INVITATION};

fn pairing_state(person: &Person, name: &str) -> PairingState {
    person.contact(name).state.contact.state
}

/// The code of the single `Accepted` among the notices.
fn accepted_code(notices: &[Notice]) -> String {
    let codes: Vec<_> = notices
        .iter()
        .filter_map(|n| match n {
            Notice::Accepted { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(codes.len(), 1, "{notices:?}");
    codes[0].clone()
}

fn revealed_code(notices: &[Notice]) -> String {
    match notices {
        [Notice::Revealed { code, .. }, ..] => code.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn pairing_works_in_both_directions() {
    let world = World::new();
    // Anna invites Bert.
    let (mut anna, mut bert) = paired(&world).await;
    assert!(anna.contact("Bert").state.is_paired());
    assert!(bert.contact("Anna").state.is_paired());
    // Nobody keeps the secret value once paired.
    assert_eq!(anna.contact("Bert").state.contact.secret, None);
    assert_eq!(bert.contact("Anna").state.contact.secret, None);

    // Bert invites Anna on a second contact.
    let code = bert.ctx.invite("Anna 2", false).await.unwrap();
    anna.ctx.accept("Bert 2", &code).await.unwrap();
    assert_eq!(pairing_state(&anna, "Bert 2"), PairingState::AwaitingReveal);
    assert_eq!(
        anna.ctx.overview().unwrap().contacts[1].status,
        Status::AwaitingReveal
    );
    let bert_code = accepted_code(&bert.sync().await);
    // Bert keeps n until he confirms.
    assert!(bert.contact("Anna 2").state.contact.secret.is_some());
    let anna_code = revealed_code(&anna.sync().await);
    assert_eq!(anna_code, bert_code);
    anna.out.take();
    anna.ctx.contacts().unwrap();
    let listing = anna.out.take();
    assert!(listing.contains(&anna_code), "{listing}");
    assert!(listing.contains(COMPARE_HINT), "{listing}");

    bert.ctx.confirm("Anna 2").await.unwrap();
    assert_eq!(bert.contact("Anna 2").state.contact.secret, None);
    assert!(matches!(
        anna.sync().await.as_slice(),
        [Notice::Paired { .. }, Notice::Rules { .. }]
    ));
    anna.ctx.request("Bert 2").await.unwrap();
    assert_eq!(
        bert.sync().await,
        vec![
            Notice::Rules {
                name: "Anna 2".into(),
                rules: lz_core::wire::Rules::DEFAULT,
            },
            Notice::Request {
                name: "Anna 2".into()
            }
        ]
    );
}

/// Mallory sits between Anna (inviting) and Bert (accepting): she accepts
/// Anna's invitation herself and gives Bert an invitation of her own.
#[tokio::test]
async fn person_in_the_middle_gets_different_codes() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let mut mallory = world.person(Some((RELAY_B, Some(TOKEN_B))));

    let to_bert = anna.ctx.invite("Bert", false).await.unwrap();
    // Intercepted: Bert gets Mallory's invitation instead.
    let swapped = mallory.ctx.invite("Bert", false).await.unwrap();
    // Mallory's commitment is fixed in the code before Bert's key exists.
    let commitment = Invitation::from_text(&swapped).unwrap().commitment;
    mallory.ctx.accept("Anna", &to_bert).await.unwrap();
    // Mallory's key towards Anna is fixed before Anna reveals anything.
    let mallory_key = mallory.contact("Anna").state.contact.key.public_key();
    bert.ctx.accept("Anna", &swapped).await.unwrap();

    let anna_code = accepted_code(&anna.sync().await);
    assert_eq!(
        anna.contact("Bert").state.contact.peer_key,
        mallory_key,
        "Anna paired the key Mallory fixed before the reveal"
    );
    let notices = mallory.sync().await;
    let mallory_code_to_bert = accepted_code(&notices);
    assert!(notices.contains(&Notice::Revealed {
        name: "Anna".into(),
        code: anna_code.clone(),
        paired: false,
    }));
    let bert_code = revealed_code(&bert.sync().await);
    assert_eq!(bert_code, mallory_code_to_bert);
    // Bert's code comes from the value behind the commitment Mallory gave
    // him, and from his own fresh key; Anna's from her own value and the
    // key Mallory fixed. Neither could be chosen to match the other.
    let n = mallory.contact("Bert").state.contact.secret.unwrap();
    assert_eq!(crypto::invite_commitment(&n), commitment);
    assert_eq!(
        bert_code,
        crypto::verification_code(
            &mallory.contact("Bert").state.contact.key.public_key(),
            &bert.contact("Anna").state.contact.key.public_key(),
            &n,
        )
    );
    assert_ne!(anna_code, bert_code);

    // Mallory now knows both codes. Trying to change Bert's code with
    // another value, found by searching, only raises the alarm.
    let bert_key = bert.contact("Anna").state.contact.key.public_key();
    let mallory_bert_key = mallory.contact("Bert").state.contact.key.public_key();
    let searched = (0u32..5_000)
        .map(|i| crypto::sha256(&[&n, &i.to_be_bytes()]))
        .find(|candidate| {
            crypto::verification_code(&mallory_bert_key, &bert_key, candidate) == anna_code
        })
        .unwrap_or([0x55; 32]);
    let forged = mallory
        .contact("Bert")
        .state
        .contact
        .seal(Body::Reveal(searched), world.now(), &mut rand_core::OsRng)
        .unwrap();
    let peer = mallory.contact("Bert").state.contact.peer;
    world
        .relay
        .deliver(&peer.relay, &peer.mailbox, &peer.send_token, &forged)
        .await
        .unwrap();
    assert_eq!(
        bert.sync().await,
        vec![Notice::RevealMismatch {
            name: "Anna".into()
        }]
    );
    assert!(bert.out.take().contains("Achtung:"));
    assert!(bert.contact("Anna").state.ended);
    assert!(!bert.contact("Anna").state.is_paired());
}

#[tokio::test]
async fn reveal_that_does_not_match_raises_the_alarm_and_ends() {
    let world = World::new();
    let mut mallory = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = mallory.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    let bert_mailbox = bert.contact("Anna").state.contact.own_mailbox;

    // The honest `reveal` does not get through; a forged one does.
    world.relay.set_down(RELAY_B, true);
    mallory.sync().await;
    world.relay.set_down(RELAY_B, false);
    let contact = mallory.contact("Bert").state.contact;
    let forged = contact
        .seal(Body::Reveal([9; 32]), world.now(), &mut rand_core::OsRng)
        .unwrap();
    world
        .relay
        .deliver(
            &contact.peer.relay,
            &contact.peer.mailbox,
            &contact.peer.send_token,
            &forged,
        )
        .await
        .unwrap();

    assert_eq!(
        bert.sync().await,
        vec![Notice::RevealMismatch {
            name: "Anna".into()
        }]
    );
    let out = bert.out.take();
    assert!(out.contains("Achtung: "), "{out}");
    assert!(bert.contact("Anna").state.ended);
    assert!(!world.relay.has_mailbox(RELAY_B, &bert_mailbox));
    assert_eq!(
        bert.ctx.overview().unwrap().contacts[0].status,
        Status::Ended
    );

    // The inviting side sees the rejection.
    assert_eq!(
        mallory.sync().await,
        vec![Notice::Rejected {
            name: "Bert".into()
        }]
    );
}

#[tokio::test]
async fn hello_ok_before_reveal_is_held_back() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();

    // `reveal` gets lost; a modified client on Anna's side sends `hello_ok`
    // right away, without the person confirming.
    world.relay.set_down(RELAY_B, true);
    let anna_code = accepted_code(&anna.sync().await);
    assert!(anna.out.take().contains("nicht zugestellt"));
    world.relay.set_down(RELAY_B, false);
    let contact = anna.contact("Bert").state.contact;
    let hello_ok = contact
        .seal(Body::HelloOk, world.now(), &mut rand_core::OsRng)
        .unwrap();
    world
        .relay
        .deliver(
            &contact.peer.relay,
            &contact.peer.mailbox,
            &contact.peer.send_token,
            &hello_ok,
        )
        .await
        .unwrap();

    assert!(bert.sync().await.is_empty());
    assert_eq!(pairing_state(&bert, "Anna"), PairingState::AwaitingReveal);
    expect_not_paired(&mut bert).await;

    // Bert repeats `hello`; the repeated `reveal` completes the pairing,
    // and Bert sees the code all the same.
    world.advance(HELLO_REPEAT_SECS);
    bert.sync().await;
    assert!(anna.sync().await.is_empty());
    let notices = bert.sync().await;
    assert_eq!(
        notices,
        vec![
            Notice::Revealed {
                name: "Anna".into(),
                code: anna_code,
                paired: true,
            },
            Notice::Paired {
                name: "Anna".into()
            },
        ]
    );
    assert!(bert.contact("Anna").state.is_paired());
}

async fn expect_not_paired(person: &mut Person) {
    let error = person.ctx.request("Anna").await.unwrap_err();
    assert!(error.0.contains("noch nicht gekoppelt"), "{error:?}");
}

#[tokio::test]
async fn lost_reveal_is_recovered_by_repeating_hello() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    let anna_mailbox = anna.device().invitations[0].pending.own.mailbox;

    world.relay.set_down(RELAY_B, true);
    let anna_code = accepted_code(&anna.sync().await);
    world.relay.set_down(RELAY_B, false);

    // Not yet time to repeat.
    world.advance(HELLO_REPEAT_SECS - 1);
    assert!(bert.sync().await.is_empty());
    assert_eq!(world.relay.waiting(RELAY_A, &anna_mailbox), 0);
    world.advance(1);
    assert!(bert.sync().await.is_empty());
    assert_eq!(world.relay.waiting(RELAY_A, &anna_mailbox), 1);

    // The same `hello` again: a new `reveal`, no warning, no new contact.
    assert!(anna.sync().await.is_empty());
    let out = anna.out.take();
    assert!(!out.contains("weiteres Mal"), "{out}");
    assert_eq!(anna.device().contacts.len(), 1);
    assert_eq!(revealed_code(&bert.sync().await), anna_code);
    assert_eq!(pairing_state(&bert, "Anna"), PairingState::AwaitingHelloOk);

    // Once the code is there, nothing is repeated anymore.
    world.advance(HOUR);
    bert.sync().await;
    assert_eq!(world.relay.waiting(RELAY_A, &anna_mailbox), 0);
    anna.ctx.confirm("Bert").await.unwrap();
    assert!(matches!(
        bert.sync().await.as_slice(),
        [Notice::Paired { .. }, Notice::Rules { .. }]
    ));
}

#[tokio::test]
async fn second_hello_with_another_key_gets_no_reveal() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let mut mallory = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    // Both `hello`s are fetched in one go.
    mallory.ctx.accept("Anna", &code).await.unwrap();

    let notices = anna.sync().await;
    assert!(matches!(
        notices.as_slice(),
        [
            Notice::Accepted { .. },
            Notice::SecondAcceptance { paired: false, .. }
        ]
    ));
    assert!(anna.out.take().contains("Achtung:"));
    assert_eq!(revealed_code(&bert.sync().await), accepted_code(&notices));
    assert!(mallory.sync().await.is_empty());
    assert_eq!(
        pairing_state(&mallory, "Anna"),
        PairingState::AwaitingReveal
    );

    // Repeating it does not help Mallory either: dropped as seen before.
    world.advance(HELLO_REPEAT_SECS);
    mallory.sync().await;
    assert!(anna.sync().await.is_empty());
    assert!(mallory.sync().await.is_empty());
    assert_eq!(
        pairing_state(&mallory, "Anna"),
        PairingState::AwaitingReveal
    );
}

#[tokio::test]
async fn without_reveal_for_a_day_the_pairing_fails() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let code = anna.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Anna", &code).await.unwrap();
    let mailbox = bert.contact("Anna").state.contact.own_mailbox;
    let anna_mailbox = anna.device().invitations[0].pending.own.mailbox;

    // Anna's device stays away; Bert waits a little over a day.
    world.advance(MAX_MESSAGE_AGE_SECS);
    assert!(bert.sync().await.is_empty());
    assert!(!bert.contact("Anna").state.ended);
    world.advance(1);
    assert_eq!(
        bert.sync().await,
        vec![Notice::PairingFailed {
            name: "Anna".into()
        }]
    );
    assert!(bert.out.take().contains("Kopplung mit Anna fehlgeschlagen"));
    assert!(bert.contact("Anna").state.ended);
    assert!(!world.relay.has_mailbox(RELAY_B, &mailbox));
    // `end` went to the mailbox of the invitation, after the repeated
    // `hello`s.
    assert!(world.relay.waiting(RELAY_A, &anna_mailbox) >= 2);
    // Nothing more happens.
    assert!(bert.sync().await.is_empty());
}

#[tokio::test]
async fn invitations_of_version_one_are_refused() {
    let world = World::new();
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    // As the previous release wrote it.
    let old = "LZ1-AEAAAAAAABVPO3CAAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQEAQCAIB";
    let error = bert.ctx.accept("Anna", old).await.unwrap_err();
    assert_eq!(error.0, OLD_INVITATION);
    let error = bert.ctx.accept("Anna", "lz1-aeaa").await.unwrap_err();
    assert_eq!(error.0, OLD_INVITATION);
    // `LZ2-` with the version byte 1.
    let error = bert.ctx.accept("Anna", "LZ2-AEAA").await.unwrap_err();
    assert_eq!(error.0, OLD_INVITATION);
    let error = bert.ctx.accept("Anna", "LZ2-AM").await.unwrap_err();
    assert!(error.0.contains("neueren Version"), "{error:?}");
    assert!(bert.device().contacts.is_empty());
}

#[tokio::test]
async fn leftovers_of_version_one_are_deleted_with_their_mailboxes() {
    let world = World::new();
    let mut anna = world.person(Some((RELAY_A, Some(TOKEN_A))));
    let mut bert = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let mut carl = world.person(Some((RELAY_B, Some(TOKEN_B))));
    let (mut paired_anna, mut paired_dora) = paired(&world).await;

    // An open invitation and a pairing in progress, as an older version
    // stored them: without secret value and commitment.
    anna.ctx.invite("Bert", false).await.unwrap();
    let invitation = anna.device().invitations[0].pending.own.mailbox;
    let code = carl.ctx.invite("Bert", false).await.unwrap();
    bert.ctx.accept("Carl", &code).await.unwrap();
    let accepted = bert.contact("Carl").state.contact.own_mailbox;
    anna.ctx
        .store
        .update(|device| {
            device.invitations[0].pending.secret = None;
            Ok::<_, io::Error>(())
        })
        .unwrap();
    bert.ctx
        .store
        .update(|device| {
            let contact = &mut device.contacts[0].state.contact;
            contact.state = PairingState::AwaitingHelloOk;
            contact.awaiting_reveal = None;
            contact.secret = None;
            Ok::<_, io::Error>(())
        })
        .unwrap();
    let text = std::fs::read_to_string(anna.ctx.store.path()).unwrap();
    assert!(!text.contains("secret"), "{text}");

    anna.sync().await;
    assert!(anna.device().invitations.is_empty());
    assert!(!world.relay.has_mailbox(RELAY_A, &invitation));
    assert!(anna.out.take().contains("älteren Version"));

    assert_eq!(
        bert.sync().await,
        vec![Notice::PairingFailed {
            name: "Carl".into()
        }]
    );
    assert!(bert.contact("Carl").state.ended);
    assert!(!world.relay.has_mailbox(RELAY_B, &accepted));

    // Contacts paired before stay as they are.
    world.advance(MINUTE);
    paired_dora.ctx.request("Anna").await.unwrap();
    assert!(matches!(
        paired_anna.sync().await.as_slice(),
        [Notice::Request { .. }]
    ));
}
