//! Notifications when a contact answers an own request (M7).
//!
//! The wording says that an answer came, not that the person is fine (see
//! `docs/concept.md`). The core drops an answer that arrives again, so each
//! answer is notified once on each own device.

use lz_client::ctx::{Ctx, system_now};
use lz_client::i18n::{Arg, Texts};
use lz_client::relay::Http;
use lz_client::show::{self, Notice};
use serde::Serialize;
use tauri::AppHandle;

/// An answer as the notification shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Answer {
    /// Name of the contact; on Android one notification per contact.
    pub name: String,
    /// When the answer was sent, Unix seconds.
    pub at: u64,
    /// "Anna has answered".
    pub title: String,
    /// The time of a manual answer, or the reason of a reported one.
    pub text: String,
}

impl Answer {
    /// The answer in a notice, if it is one.
    pub fn from_notice(t: Texts, notice: &Notice, now: u64) -> Option<Self> {
        let Notice::Response { name, kind, at } = notice else {
            return None;
        };
        Some(Self {
            name: name.clone(),
            at: *at,
            title: t.text("app-answer-title", &[("name", Arg::Text(name))]),
            text: show::response(t, kind, *at, now),
        })
    }
}

/// Lets `ctx` notify of every answer it receives.
pub fn notify_from(ctx: &mut Ctx<Http>, app: &AppHandle) {
    let app = app.clone();
    ctx.on_notice = Some(Box::new(move |notice| {
        if let Some(answer) = Answer::from_notice(crate::lang::texts(), notice, system_now()) {
            show(&app, &answer);
        }
    }));
}

fn show(app: &AppHandle, answer: &Answer) {
    #[cfg(desktop)]
    crate::desktop::answer(app, answer);
    #[cfg(target_os = "android")]
    if let Err(error) = tauri_plugin_lebenszeichen::show_answer(app, answer) {
        eprintln!("Answer notification failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use lz_client::i18n::Language;
    use lz_core::wire::{Reason, ResponseKind};

    use super::*;

    #[test]
    fn says_who_answered_and_how() {
        let t = Language::En.texts();
        let reported = Notice::Response {
            name: "Anna".into(),
            kind: ResponseKind::Reported(Reason::new("asleep").unwrap()),
            at: 1000,
        };
        let answer = Answer::from_notice(t, &reported, 1000).unwrap();
        assert_eq!(answer.title, "Anna has answered");
        assert_eq!(answer.text, "“asleep” (reported)");
        let manual = Notice::Response {
            name: "Anna".into(),
            kind: ResponseKind::Manual,
            at: 1000,
        };
        assert!(
            Answer::from_notice(t, &manual, 1000)
                .unwrap()
                .text
                .starts_with('✓')
        );
        let request = Notice::Request {
            name: "Anna".into(),
        };
        assert_eq!(Answer::from_notice(t, &request, 1000), None);
    }
}
