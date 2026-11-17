//! Reported answers: sending them when they are due, and the setting per
//! contact of whether this device may send them.
//!
//! The state itself is set through the local interface (`local.rs`); the
//! rules of when a reported answer is due live in `lz_core::report`.

use std::collections::HashSet;

use lz_core::report::Invalid;
use lz_core::store::ReportSetting;
use lz_core::wire::{MessageId, Reason, ResponseKind};
use rand_core::OsRng;

use crate::ctx::{Ctx, Fail, Result, contact_mut, core};
use crate::i18n::Texts;
use crate::relay::Relay;
use crate::show::Notice;

/// Requests for which sending a reported answer failed in this run, so the
/// problem is shown once and not on every attempt.
pub(crate) type Failed = HashSet<MessageId>;

fn invalid(t: Texts, error: Invalid) -> Fail {
    match error {
        Invalid::EmptyReason => fail!(t, "error-reason-empty"),
        Invalid::Duration => fail!(
            t,
            "error-state-duration",
            max = &lz_core::report::MAX_STATE_MINUTES.to_string()
        ),
        Invalid::Delay => fail!(
            t,
            "error-report-delay",
            max = &lz_core::report::MAX_DELAY_MINUTES.to_string()
        ),
        Invalid::UnknownContact => Fail(error.to_string()),
    }
}

impl<R: Relay> Ctx<R> {
    /// Sets the reported state of this device for `minutes` from now.
    pub fn set_state(&mut self, reason: &str, minutes: u32) -> Result<()> {
        let t = self.texts();
        let reason = Reason::new(reason.trim()).map_err(|_| fail!(t, "error-reason-long"))?;
        let now = self.now();
        self.update(|device| {
            device
                .set_state(reason, minutes, now)
                .map_err(|error| invalid(t, error))
        })
    }

    /// Ends the reported state of this device now.
    pub fn clear_state(&mut self) -> Result<()> {
        self.update(|device| {
            device.clear_state();
            Ok(())
        })
    }

    /// The setting for a contact: whether this device may answer it with a
    /// reported state, and after which waiting time.
    pub fn report_setting(&self, name: &str) -> Result<ReportSetting> {
        let mailbox = self.own_mailbox_of(name)?;
        Ok(self.load()?.report_setting(&mailbox))
    }

    /// Changes the setting for a contact. `delay_minutes` keeps the current
    /// waiting time if `None`.
    pub fn set_report_setting(
        &mut self,
        name: &str,
        allowed: bool,
        delay_minutes: Option<u32>,
    ) -> Result<ReportSetting> {
        let t = self.texts();
        self.update(|device| {
            let mailbox = contact_mut(t, device, name)?.state.contact.own_mailbox;
            let delay = delay_minutes.unwrap_or(device.report_setting(&mailbox).delay_minutes);
            device
                .set_report_setting(&mailbox, allowed, delay)
                .map_err(|error| invalid(t, error))?;
            Ok(device.report_setting(&mailbox))
        })
    }

    /// Sends the reported answers that are due and tells the other own
    /// devices. A failed delivery is rolled back and tried again on the
    /// next call; it is shown once per `failed`.
    pub(crate) async fn send_due_reports(
        &mut self,
        notices: &mut Vec<Notice>,
        failed: &mut Failed,
    ) -> Result<()> {
        let now = self.now();
        let due = self.update(|device| {
            device.refresh_arrivals(now);
            Ok(device.due_reports(now))
        })?;
        for (mailbox, request, reason) in due {
            let device = self.load()?;
            let Some(name) = device.contact_by_mailbox(&mailbox).map(|c| c.name.clone()) else {
                continue;
            };
            let sent = self
                .change_and_send(&name, |state, now| {
                    let envelope = state
                        .report(&request, reason, now, &mut OsRng)
                        .map_err(core)?;
                    Ok((now, vec![envelope]))
                })
                .await;
            let at = match sent {
                Ok(at) => at,
                Err(error) => {
                    if failed.insert(request) {
                        let notice = Notice::Problem(tr!(
                            self.texts(),
                            "problem-report-not-sent",
                            name = &name,
                            error = &error.0
                        ));
                        self.show(&notice);
                        notices.push(notice);
                    }
                    continue;
                }
            };
            failed.remove(&request);
            let notice = Notice::Reported {
                name,
                reason: reason.to_string(),
            };
            self.show(&notice);
            notices.push(notice);
            self.tell_devices(|device, now| {
                let kind = ResponseKind::Reported(reason);
                device.sync_responded(&mailbox, request, kind, at, now, &mut OsRng)
            })
            .await?;
        }
        Ok(())
    }
}

/// What `lz state` asks of the local interface of a running `lz listen`.
#[cfg(unix)]
pub enum StateCall<'a> {
    Show,
    Set { reason: &'a str, minutes: u32 },
    Clear,
}

#[cfg(unix)]
impl<R: Relay> Ctx<R> {
    /// Shows, sets or clears the state through the socket at `socket`, so
    /// that it takes the same way as every other program.
    pub async fn state_call(
        &mut self,
        socket: &std::path::Path,
        call: StateCall<'_>,
    ) -> Result<()> {
        let t = self.texts();
        let (method, body) = match &call {
            StateCall::Show => ("GET", None),
            StateCall::Set { reason, minutes } => (
                "POST",
                Some(serde_json::json!({ "reason": reason, "minutes": minutes }).to_string()),
            ),
            StateCall::Clear => ("DELETE", None),
        };
        let (status, answer) = crate::local::request(socket, method, body.as_deref())
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    fail!(t, "error-no-listener", path = &socket.display().to_string())
                }
                _ => Fail(error.to_string()),
            })?;
        let json: serde_json::Value = serde_json::from_str(&answer).unwrap_or_default();
        let text = match (&call, status) {
            (StateCall::Show, 404) => tr!(t, "out-state-none"),
            (StateCall::Show, 200) => {
                let reason = json["reason"].as_str().unwrap_or_default();
                let until = json["until"].as_u64().unwrap_or_default();
                tr!(
                    t,
                    "status-reported-state",
                    reason = &crate::show::reported_state(t, reason),
                    until = &crate::show::when(t, until, self.now())
                )
            }
            (StateCall::Set { reason, minutes }, 204) => tr!(
                t,
                "out-state-set",
                reason = &crate::show::reported_state(t, reason.trim()),
                duration = &crate::show::minutes(t, *minutes)
            ),
            (StateCall::Clear, 204) => tr!(t, "out-state-cleared"),
            _ => {
                let error = json["error"].as_str().unwrap_or("unexpected answer");
                return Err(Fail(format!("{status}: {error}")));
            }
        };
        self.say(&text);
        Ok(())
    }
}

impl<R: Relay> Ctx<R> {
    /// `lz state allow` and `lz state deny`: changes the setting and says
    /// what holds now.
    pub fn allow_reports(&mut self, name: &str, allowed: bool, delay: Option<u32>) -> Result<()> {
        let t = self.texts();
        let setting = self.set_report_setting(name, allowed, delay)?;
        let text = if setting.allowed {
            tr!(
                t,
                "out-reports-allowed",
                name = name,
                delay = &crate::show::minutes(t, setting.delay_minutes)
            )
        } else {
            tr!(t, "out-reports-denied", name = name)
        };
        self.say(&text);
        Ok(())
    }
}
