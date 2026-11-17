//! Reported answers: sending them when they are due, and the setting per
//! contact of whether this device may send them.
//!
//! The state itself is set through the local interface (`local.rs`); the
//! rules of when a reported answer is due live in `lz_core::report`.

use std::collections::HashSet;

use lz_core::report::Invalid;
use lz_core::store::ReportSetting;
use lz_core::wire::{MessageId, Reason, ResponseKind, Token};
use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;

use crate::ctx::{Ctx, Fail, Result, contact_mut, core};
use crate::i18n::Texts;
use crate::relay::{Relay, hex};
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
        Invalid::UnknownContact | Invalid::Encrypt => Fail(error.to_string()),
    }
}

fn new_token() -> Token {
    let mut token = [0; 32];
    OsRng.fill_bytes(&mut token);
    token
}

/// 64 hex digits (either case) to bytes.
fn unhex(text: &str) -> Option<Token> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0; 32];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

impl<R: Relay> Ctx<R> {
    /// Sets the reported state of this device for `minutes` from now; with
    /// `share` also on the other own devices (sent with the next fetch).
    pub fn set_state(&mut self, reason: &str, minutes: u32, share: bool) -> Result<()> {
        let t = self.texts();
        let reason = Reason::new(reason.trim()).map_err(|_| fail!(t, "error-reason-long"))?;
        let now = self.now();
        self.update(|device| {
            if share {
                device.share_state(reason, minutes, now, &mut OsRng)
            } else {
                device.set_state(reason, minutes, now)
            }
            .map_err(|error| invalid(t, error))
        })
    }

    /// Ends the own reported state of this device now; with `share` also
    /// the handed state and the shared state on the other own devices.
    pub fn clear_state(&mut self, share: bool) -> Result<()> {
        let t = self.texts();
        let now = self.now();
        self.update(|device| {
            if share {
                device
                    .clear_shared_state(now, &mut OsRng)
                    .map_err(|error| invalid(t, error))
            } else {
                device.clear_state();
                Ok(())
            }
        })
    }

    /// Ends the state that applies now, on all own devices if it is the
    /// shared one. For the ✕ next to the state.
    pub fn end_state(&mut self) -> Result<()> {
        let t = self.texts();
        let now = self.now();
        self.update(|device| {
            device
                .end_current_state(now, &mut OsRng)
                .map_err(|error| invalid(t, error))
        })
    }

    /// The reason handed over when the battery is low, if that is on.
    pub fn battery_reason(&self) -> Result<Option<String>> {
        Ok(self
            .load()?
            .reports
            .battery
            .map(|b| b.reason.as_str().to_owned()))
    }

    /// Turns handing over a state at low battery on with `reason`, or off.
    pub fn set_battery(&mut self, reason: Option<&str>) -> Result<()> {
        let t = self.texts();
        let reason = reason
            .map(|r| Reason::new(r.trim()).map_err(|_| fail!(t, "error-reason-long")))
            .transpose()?;
        self.update(|device| device.set_battery(reason).map_err(|e| invalid(t, e)))
    }

    /// Checks the battery level against the battery setting; a change is
    /// sent with the next fetch.
    pub fn check_battery(
        &mut self,
        percent: u8,
        charging: bool,
    ) -> Result<lz_core::report::BatteryChange> {
        let t = self.texts();
        let now = self.now();
        self.update(|device| {
            device
                .check_battery(percent, charging, now, &mut OsRng)
                .map_err(|e| invalid(t, e))
        })
    }

    /// The token for state reports from other apps as 64 hex digits, if
    /// they are accepted.
    pub fn report_token(&self) -> Result<Option<String>> {
        Ok(self.load()?.reports.token.map(|token| hex(&token)))
    }

    /// Accepts state reports from other apps. An existing token stays; the
    /// token is returned as hex.
    pub fn enable_report_token(&mut self) -> Result<String> {
        let token = self.update(|device| {
            let token = *device.reports.token.get_or_insert_with(new_token);
            Ok(token)
        })?;
        Ok(hex(&token))
    }

    /// Replaces the token; the old one stops working. Turns reports on if
    /// they were off.
    pub fn renew_report_token(&mut self) -> Result<String> {
        let token = self.update(|device| {
            let token = new_token();
            device.reports.token = Some(token);
            Ok(token)
        })?;
        Ok(hex(&token))
    }

    /// Stops accepting state reports from other apps.
    pub fn disable_report_token(&mut self) -> Result<()> {
        self.update(|device| {
            device.reports.token = None;
            Ok(())
        })
    }

    /// Whether `given` is the token that is set. Compared in constant time;
    /// false if none is set or `given` is not 64 hex digits.
    pub fn report_token_matches(&self, given: &str) -> Result<bool> {
        let Some(token) = self.load()?.reports.token else {
            return Ok(false);
        };
        let Some(given) = unhex(given) else {
            return Ok(false);
        };
        Ok(token.ct_eq(&given).into())
    }

    /// Seconds until a reported answer is due, for a device that has to
    /// schedule a wake-up: `Some(0)` if one is due now, `None` if nothing is
    /// pending.
    pub fn report_wait(&self) -> Result<Option<u64>> {
        let now = self.now();
        self.update(|device| {
            device.refresh_arrivals(now);
            Ok(if device.due_reports(now).is_empty() {
                device
                    .next_report_due(now)
                    .map(|due| due.saturating_sub(now))
            } else {
                Some(0)
            })
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
    Set {
        reason: &'a str,
        minutes: u32,
        share: bool,
    },
    Clear {
        share: bool,
    },
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
        let (method, query, body) = match &call {
            StateCall::Show => ("GET", "", None),
            StateCall::Set {
                reason,
                minutes,
                share,
            } => (
                "POST",
                "",
                Some(
                    serde_json::json!({ "reason": reason, "minutes": minutes, "share": share })
                        .to_string(),
                ),
            ),
            StateCall::Clear { share: false } => ("DELETE", "", None),
            StateCall::Clear { share: true } => ("DELETE", "share=true", None),
        };
        let (status, answer) = crate::local::request(socket, method, query, body.as_deref())
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
                let shared = json["shared"].as_bool().unwrap_or_default();
                let handed = json["handed"].as_bool().unwrap_or_default();
                tr!(
                    t,
                    "status-reported-state",
                    reason = &crate::show::reported_state(t, reason),
                    until = &crate::show::when(t, until, self.now()),
                    origin = &crate::show::state_origin(t, shared, handed)
                )
            }
            (
                StateCall::Set {
                    reason,
                    minutes,
                    share,
                },
                204,
            ) => tr!(
                t,
                if *share {
                    "out-state-shared"
                } else {
                    "out-state-set"
                },
                reason = &crate::show::reported_state(t, reason.trim()),
                duration = &crate::show::minutes(t, *minutes)
            ),
            (StateCall::Clear { share: false }, 204) => tr!(t, "out-state-cleared"),
            (StateCall::Clear { share: true }, 204) => tr!(t, "out-state-cleared-everywhere"),
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
