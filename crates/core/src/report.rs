//! Reported states: what an external program reported through the local
//! interface, and when a reported answer is due.
//!
//! A state applies only to this device. For each contact, this device sends
//! a reported answer only if the person allowed it here, and only once the
//! waiting time since the request arrived on this device has passed without
//! a manual answer. Sending is up to the caller, through
//! [`ContactState::report`](crate::contact::ContactState::report).
//!
//! A state may also be shared (M8c): it then goes to the other own devices
//! as `state_shared`, and each of them answers its contacts with it as if
//! it had been reported there ("handed" state). All shared states of the
//! person form one: a newer `state_shared` replaces it everywhere, and
//! `state_cleared` ends it everywhere, while an own state that was never
//! shared stays. The settings per contact and the arrivals never leave the
//! device.

use rand_core::CryptoRngCore;

use crate::contact::next_stamp;
use crate::defaults::REPORTED_ANSWER_DELAY_MINUTES;
use crate::group::{SyncEvent, SyncIgnored};
use crate::store::{Arrival, BatterySetting, Device, ReportSetting, ReportedState};
use crate::wire::{Body, MailboxId, Message, MessageId, Reason, Stamp, StateShared};

/// Longest time a state may apply: one week.
pub const MAX_STATE_MINUTES: u32 = 7 * 24 * 60;

/// Longest waiting time before a reported answer: one day.
pub const MAX_DELAY_MINUTES: u32 = 24 * 60;

/// At this battery level or below the battery state is handed over.
pub const BATTERY_LOW_PERCENT: u8 = 2;

/// At this level or above, or while charging, it is ended again.
pub const BATTERY_OK_PERCENT: u8 = 5;

/// How long the battery state applies if nobody ends it: one day.
pub const BATTERY_STATE_MINUTES: u32 = 24 * 60;

/// What a battery check did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryChange {
    None,
    /// The battery state was set and handed over.
    Shared,
    /// The battery state was ended on all devices.
    Cleared,
}

/// Why a state or setting was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The reason is empty or only white space.
    EmptyReason,
    /// The duration is not between 1 and [`MAX_STATE_MINUTES`].
    Duration,
    /// The waiting time is not between 1 and [`MAX_DELAY_MINUTES`].
    Delay,
    /// No contact with this mailbox.
    UnknownContact,
    /// The state could not be encrypted for the other devices. Does not
    /// happen with a valid group key.
    Encrypt,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyReason => f.write_str("the reason is empty"),
            Self::Duration => write!(f, "the duration must be 1 to {MAX_STATE_MINUTES} minutes"),
            Self::Delay => write!(
                f,
                "the waiting time must be 1 to {MAX_DELAY_MINUTES} minutes"
            ),
            Self::UnknownContact => f.write_str("no such contact"),
            Self::Encrypt => f.write_str("the state could not be encrypted"),
        }
    }
}

impl std::error::Error for Invalid {}

impl ReportSetting {
    /// The setting of a contact nobody changed: not allowed, 15 minutes.
    pub fn default_for(mailbox: MailboxId) -> Self {
        Self {
            mailbox,
            allowed: false,
            delay_minutes: REPORTED_ANSWER_DELAY_MINUTES,
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::default_for(self.mailbox)
    }
}

/// The state that applies now, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurrentState {
    pub reason: Reason,
    /// End of the state, Unix seconds.
    pub until: u64,
    /// Set on this device and handed to the other devices.
    pub shared: bool,
    /// Handed to this device by another one.
    pub handed: bool,
}

impl CurrentState {
    /// Whether the state is the person's shared one, so that ending it here
    /// should end it on all devices.
    pub fn everywhere(&self) -> bool {
        self.shared || self.handed
    }
}

fn new_state(
    reason: Reason,
    minutes: u32,
    now: u64,
    shared: bool,
) -> Result<ReportedState, Invalid> {
    if reason.as_str().trim().is_empty() {
        return Err(Invalid::EmptyReason);
    }
    if !(1..=MAX_STATE_MINUTES).contains(&minutes) {
        return Err(Invalid::Duration);
    }
    Ok(ReportedState {
        reason,
        until: now + u64::from(minutes) * 60,
        since: now,
        shared,
    })
}

impl Device {
    /// Sets the own state for `minutes` from `now`, replacing any earlier
    /// own one. Only this device uses it.
    pub fn set_state(&mut self, reason: Reason, minutes: u32, now: u64) -> Result<(), Invalid> {
        self.reports.state = Some(new_state(reason, minutes, now, false)?);
        Ok(())
    }

    /// Sets the own state as [`Self::set_state`] does and hands it to the
    /// other devices (`state_shared` into the outbox). It replaces the
    /// handed state here, as it does on the other devices. Without other
    /// devices it is only set here.
    pub fn share_state(
        &mut self,
        reason: Reason,
        minutes: u32,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Invalid> {
        let state = new_state(reason, minutes, now, true)?;
        let body = Body::StateShared(StateShared {
            reason,
            until: state.until,
        });
        self.queue_share(body, now, rng)?;
        self.reports.handed = None;
        self.reports.state = Some(state);
        Ok(())
    }

    /// Ends the own state here; a handed state stays.
    pub fn clear_state(&mut self) {
        self.reports.state = None;
    }

    /// Ends the own and the handed state here and the shared state on the
    /// other devices (`state_cleared` into the outbox). An own state that
    /// another device never received stays there.
    pub fn clear_shared_state(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Invalid> {
        self.queue_share(Body::StateCleared, now, rng)?;
        self.reports.state = None;
        self.reports.handed = None;
        Ok(())
    }

    /// Ends the state that applies now: on all devices if it is the shared
    /// one, otherwise only here. For the ✕ next to the state.
    pub fn end_current_state(
        &mut self,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Invalid> {
        match self.current_state(now) {
            Some(state) if state.everywhere() => {
                self.queue_share(Body::StateCleared, now, rng)?;
                let reports = &mut self.reports;
                reports.handed = None;
                if reports.state.is_some_and(|s| s.shared) {
                    reports.state = None;
                }
            }
            Some(_) => self.reports.state = None,
            None => {}
        }
        Ok(())
    }

    /// Turns the battery setting on with `reason`, or off with `None`.
    /// Turning it off leaves a state that was handed over already.
    pub fn set_battery(&mut self, reason: Option<Reason>) -> Result<(), Invalid> {
        self.reports.battery = match reason {
            Some(reason) if reason.as_str().trim().is_empty() => {
                return Err(Invalid::EmptyReason);
            }
            Some(reason) => Some(BatterySetting {
                reason,
                low: self.reports.battery.is_some_and(|b| b.low),
            }),
            None => None,
        };
        Ok(())
    }

    /// Checks the battery level: at [`BATTERY_LOW_PERCENT`] or below, not
    /// charging, the battery state is shared once; at
    /// [`BATTERY_OK_PERCENT`] or above, or while charging, it is ended on
    /// all devices, but only if it is still the shared state here (not
    /// replaced by another device's state or ended in the meantime).
    pub fn check_battery(
        &mut self,
        percent: u8,
        charging: bool,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<BatteryChange, Invalid> {
        let Some(setting) = self.reports.battery else {
            return Ok(BatteryChange::None);
        };
        if !setting.low && !charging && percent <= BATTERY_LOW_PERCENT {
            self.share_state(setting.reason, BATTERY_STATE_MINUTES, now, rng)?;
            self.set_battery_low(true);
            return Ok(BatteryChange::Shared);
        }
        if setting.low && (charging || percent >= BATTERY_OK_PERCENT) {
            let still = self
                .reports
                .state
                .is_some_and(|s| s.shared && s.reason == setting.reason && now < s.until);
            if still {
                self.clear_shared_state(now, rng)?;
            }
            self.set_battery_low(false);
            return Ok(if still {
                BatteryChange::Cleared
            } else {
                BatteryChange::None
            });
        }
        Ok(BatteryChange::None)
    }

    fn set_battery_low(&mut self, low: bool) {
        if let Some(battery) = &mut self.reports.battery {
            battery.low = low;
        }
    }

    /// Queues `state_shared` or `state_cleared` with a stamp newer than any
    /// seen here, and keeps the stamp.
    fn queue_share(
        &mut self,
        body: Body,
        now: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), Invalid> {
        let stamp = next_stamp(self.reports.share_stamp, now, rng);
        let message = Message {
            id: stamp.id,
            sent_at: stamp.at,
            body,
        };
        self.queue_sync(&message, now, rng)
            .map_err(|_| Invalid::Encrypt)?;
        self.reports.share_stamp = stamp;
        Ok(())
    }

    /// Applies `state_shared` (`Some`) or `state_cleared` (`None`) from
    /// another device, if newer than the last one.
    pub(crate) fn apply_shared_state(
        &mut self,
        shared: Option<StateShared>,
        stamp: Stamp,
    ) -> SyncEvent {
        let reports = &mut self.reports;
        if stamp <= reports.share_stamp {
            return SyncEvent::Ignored(SyncIgnored::Outdated);
        }
        reports.share_stamp = stamp;
        if reports.state.is_some_and(|s| s.shared) {
            reports.state = None;
        }
        reports.handed = shared.as_ref().map(|s| ReportedState {
            reason: s.reason,
            until: s.until,
            since: stamp.at,
            shared: false,
        });
        SyncEvent::StateHanded {
            reason: shared.map(|s| s.reason.as_str().to_owned()),
        }
    }

    /// The state that applies at `now`: of the own and the handed one, the
    /// valid one set last; the own one if both were set at the same time.
    pub fn current_state(&self, now: u64) -> Option<CurrentState> {
        let valid = |s: &Option<ReportedState>| s.filter(|s| now < s.until);
        let own = valid(&self.reports.state).map(|s| CurrentState {
            reason: s.reason,
            until: s.until,
            shared: s.shared,
            handed: false,
        });
        let handed = valid(&self.reports.handed).map(|s| CurrentState {
            reason: s.reason,
            until: s.until,
            shared: false,
            handed: true,
        });
        let since = |s: &Option<ReportedState>| s.map_or(0, |s| s.since);
        match (own, handed) {
            (Some(own), Some(handed)) => Some(
                if since(&self.reports.handed) > since(&self.reports.state) {
                    handed
                } else {
                    own
                },
            ),
            (own, handed) => own.or(handed),
        }
    }

    /// The setting for the contact with this own mailbox.
    pub fn report_setting(&self, mailbox: &MailboxId) -> ReportSetting {
        self.reports
            .contacts
            .iter()
            .find(|s| &s.mailbox == mailbox)
            .copied()
            .unwrap_or_else(|| ReportSetting::default_for(*mailbox))
    }

    /// Changes the setting for a contact. Only settings that differ from
    /// the default are stored.
    pub fn set_report_setting(
        &mut self,
        mailbox: &MailboxId,
        allowed: bool,
        delay_minutes: u32,
    ) -> Result<(), Invalid> {
        if !(1..=MAX_DELAY_MINUTES).contains(&delay_minutes) {
            return Err(Invalid::Delay);
        }
        if self.contact_by_mailbox(mailbox).is_none() {
            return Err(Invalid::UnknownContact);
        }
        let setting = ReportSetting {
            mailbox: *mailbox,
            allowed,
            delay_minutes,
        };
        self.reports.contacts.retain(|s| &s.mailbox != mailbox);
        if !setting.is_default() {
            self.reports.contacts.push(setting);
        }
        Ok(())
    }

    /// Brings the arrival times up to date: notes `now` for every open
    /// request that has none yet and drops those of requests that are no
    /// longer open, and drops settings of contacts that are gone.
    ///
    /// Call it right after receiving messages, so `now` is the arrival.
    pub fn refresh_arrivals(&mut self, now: u64) {
        let open: Vec<(MailboxId, MessageId)> = self
            .contacts
            .iter()
            .filter_map(|c| {
                let id = c.state.open_request(now)?;
                Some((c.state.contact.own_mailbox, id))
            })
            .collect();
        let reports = &mut self.reports;
        reports
            .arrivals
            .retain(|a| open.contains(&(a.mailbox, a.request)));
        for (mailbox, request) in open {
            if !reports
                .arrivals
                .iter()
                .any(|a| a.mailbox == mailbox && a.request == request)
            {
                reports.arrivals.push(Arrival {
                    mailbox,
                    request,
                    at: now,
                });
            }
        }
        let contacts = &self.contacts;
        self.reports.contacts.retain(|s| {
            contacts
                .iter()
                .any(|c| c.state.contact.own_mailbox == s.mailbox)
        });
    }

    /// Requests for which a reported answer is due at `now`, with the
    /// reason to send.
    pub fn due_reports(&self, now: u64) -> Vec<(MailboxId, MessageId, Reason)> {
        let Some(state) = self.current_state(now) else {
            return Vec::new();
        };
        self.candidates(now)
            .filter(|&(_, _, due)| due <= now)
            .map(|(mailbox, request, _)| (mailbox, request, state.reason))
            .collect()
    }

    /// The earliest moment after `now` at which a report becomes due with
    /// the current state, if any. For devices that have to schedule a
    /// wake-up; a report due already is in [`Self::due_reports`].
    pub fn next_report_due(&self, now: u64) -> Option<u64> {
        let state = self.current_state(now)?;
        self.candidates(now)
            .map(|(_, _, due)| due)
            .filter(|&due| now < due && due < state.until)
            .min()
    }

    /// Open requests not answered or reported yet, from contacts that
    /// allow reports, with the moment their waiting time ends.
    fn candidates(&self, now: u64) -> impl Iterator<Item = (MailboxId, MessageId, u64)> + '_ {
        self.contacts.iter().filter_map(move |c| {
            let state = &c.state;
            if state.ended || !state.is_paired() {
                return None;
            }
            let mailbox = state.contact.own_mailbox;
            let setting = self.report_setting(&mailbox);
            if !setting.allowed {
                return None;
            }
            let entry = state.incoming.open(&state.own_rules, now)?;
            if entry.reported.is_some() {
                return None;
            }
            let arrival = self
                .reports
                .arrivals
                .iter()
                .find(|a| a.mailbox == mailbox && a.request == entry.id)?;
            let due = arrival.at + u64::from(setting.delay_minutes) * 60;
            Some((mailbox, entry.id, due))
        })
    }
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use super::{Invalid, MAX_DELAY_MINUTES, MAX_STATE_MINUTES};
    use crate::contact::{ContactState, Event};
    use crate::group::tests::{T, device_with_group, pair};
    use crate::store::{Device, ReportSetting};
    use crate::wire::{MailboxId, MessageId, Reason};

    const MIN: u64 = 60;

    fn reason(text: &str) -> Reason {
        Reason::new(text).unwrap()
    }

    /// A device with contact Anna, Anna, and Anna's mailbox on the device.
    fn setup() -> (Device, ContactState, MailboxId) {
        let mut device = device_with_group();
        let (stored, peer) = pair(1, "Anna");
        let mailbox = stored.state.contact.own_mailbox;
        device.contacts.push(stored);
        (device, peer, mailbox)
    }

    /// Anna asks at `at`; the device receives it at `arrival`.
    fn ask(device: &mut Device, anna: &mut ContactState, at: u64, arrival: u64) -> MessageId {
        let (id, envelope) = anna.request(at, &mut OsRng).unwrap();
        let event = device.contacts[0]
            .state
            .receive(&envelope, arrival)
            .unwrap();
        assert_eq!(event, Event::Request(id));
        device.refresh_arrivals(arrival);
        id
    }

    #[test]
    fn defaults_are_off_and_fifteen_minutes() {
        let (device, _, mailbox) = setup();
        assert_eq!(
            device.report_setting(&mailbox),
            ReportSetting {
                mailbox,
                allowed: false,
                delay_minutes: 15
            }
        );
        assert!(device.reports.is_empty());
    }

    #[test]
    fn nothing_is_due_unless_allowed() {
        let (mut device, mut anna, _) = setup();
        device.set_state(reason("asleep"), 480, T).unwrap();
        ask(&mut device, &mut anna, T, T);
        assert!(device.due_reports(T + 60 * MIN).is_empty());
        assert_eq!(device.next_report_due(T), None);
    }

    #[test]
    fn due_exactly_after_the_waiting_time_from_arrival() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.set_state(reason("asleep"), 480, T).unwrap();
        // Sent at T, arrived two hours later: the waiting time starts then.
        let arrival = T + 120 * MIN;
        let id = ask(&mut device, &mut anna, T, arrival);
        assert_eq!(device.next_report_due(arrival), Some(arrival + 15 * MIN));
        assert!(device.due_reports(arrival + 15 * MIN - 1).is_empty());
        assert_eq!(
            device.due_reports(arrival + 15 * MIN),
            vec![(mailbox, id, reason("asleep"))]
        );
    }

    #[test]
    fn a_later_refresh_keeps_the_first_arrival() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.set_state(reason("asleep"), 480, T).unwrap();
        ask(&mut device, &mut anna, T, T);
        device.refresh_arrivals(T + 10 * MIN);
        assert_eq!(device.reports.arrivals.len(), 1);
        assert_eq!(device.reports.arrivals[0].at, T);
        assert_eq!(device.due_reports(T + 15 * MIN).len(), 1);
    }

    #[test]
    fn no_report_without_a_valid_state() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        ask(&mut device, &mut anna, T, T);
        assert!(device.due_reports(T + 20 * MIN).is_empty());

        // Expired before the waiting time is over.
        device.set_state(reason("asleep"), 10, T).unwrap();
        assert_eq!(device.next_report_due(T), None);
        assert!(device.due_reports(T + 20 * MIN).is_empty());

        // Cleared.
        device.set_state(reason("asleep"), 480, T).unwrap();
        device.clear_state();
        assert!(device.due_reports(T + 20 * MIN).is_empty());
    }

    #[test]
    fn a_state_set_after_the_moment_still_counts() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        let id = ask(&mut device, &mut anna, T, T);
        device
            .set_state(reason("asleep"), 60, T + 40 * MIN)
            .unwrap();
        assert_eq!(
            device.due_reports(T + 40 * MIN),
            vec![(mailbox, id, reason("asleep"))]
        );
    }

    #[test]
    fn no_report_after_a_report_or_a_manual_answer() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.set_state(reason("asleep"), 480, T).unwrap();
        let id = ask(&mut device, &mut anna, T, T);
        let now = T + 15 * MIN;
        device.contacts[0]
            .state
            .report(&id, reason("asleep"), now, &mut OsRng)
            .unwrap();
        assert!(device.due_reports(now).is_empty());

        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.set_state(reason("asleep"), 480, T).unwrap();
        let id = ask(&mut device, &mut anna, T, T);
        device.contacts[0]
            .state
            .respond(&id, T + MIN, &mut OsRng)
            .unwrap();
        device.refresh_arrivals(T + MIN);
        assert!(device.due_reports(now).is_empty());
        assert!(device.reports.arrivals.is_empty());
    }

    #[test]
    fn settings_and_state_are_checked() {
        let (mut device, _, mailbox) = setup();
        assert_eq!(
            device.set_state(reason("  "), 60, T),
            Err(Invalid::EmptyReason)
        );
        assert_eq!(device.set_state(reason("x"), 0, T), Err(Invalid::Duration));
        assert_eq!(
            device.set_state(reason("x"), MAX_STATE_MINUTES + 1, T),
            Err(Invalid::Duration)
        );
        assert_eq!(
            device.set_report_setting(&mailbox, true, MAX_DELAY_MINUTES + 1),
            Err(Invalid::Delay)
        );
        assert_eq!(
            device.set_report_setting(&[77; 32], true, 15),
            Err(Invalid::UnknownContact)
        );
        // Back to the default: nothing stored.
        device.set_report_setting(&mailbox, true, 30).unwrap();
        device.set_report_setting(&mailbox, false, 15).unwrap();
        assert!(device.reports.contacts.is_empty());
    }

    #[test]
    fn settings_of_removed_contacts_are_dropped() {
        let (mut device, _, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.contacts.clear();
        device.refresh_arrivals(T);
        assert!(device.reports.contacts.is_empty());
    }

    #[test]
    fn a_handover_does_not_take_reports_along() {
        let (mut device, mut anna, mailbox) = setup();
        device.set_report_setting(&mailbox, true, 15).unwrap();
        device.set_state(reason("asleep"), 480, T).unwrap();
        device.reports.token = Some([5; 32]);
        ask(&mut device, &mut anna, T, T);
        let new = Device::from_handover(device.handover([0; 32]).unwrap(), [9; 16]);
        assert!(new.reports.is_empty());
    }
}
