//! Reported states: what an external program reported through the local
//! interface, and when a reported answer is due.
//!
//! A state applies only to this device. For each contact, this device sends
//! a reported answer only if the person allowed it here, and only once the
//! waiting time since the request arrived on this device has passed without
//! a manual answer. Sending is up to the caller, through
//! [`ContactState::report`](crate::contact::ContactState::report).
//!
//! Everything here is local and never leaves the device.

use crate::defaults::REPORTED_ANSWER_DELAY_MINUTES;
use crate::store::{Arrival, Device, ReportSetting, ReportedState};
use crate::wire::{MailboxId, MessageId, Reason};

/// Longest time a state may apply: one week.
pub const MAX_STATE_MINUTES: u32 = 7 * 24 * 60;

/// Longest waiting time before a reported answer: one day.
pub const MAX_DELAY_MINUTES: u32 = 24 * 60;

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

impl Device {
    /// Sets the state for `minutes` from `now`, replacing any earlier one.
    pub fn set_state(&mut self, reason: Reason, minutes: u32, now: u64) -> Result<(), Invalid> {
        if reason.as_str().trim().is_empty() {
            return Err(Invalid::EmptyReason);
        }
        if !(1..=MAX_STATE_MINUTES).contains(&minutes) {
            return Err(Invalid::Duration);
        }
        self.reports.state = Some(ReportedState {
            reason,
            until: now + u64::from(minutes) * 60,
        });
        Ok(())
    }

    /// Ends the state now.
    pub fn clear_state(&mut self) {
        self.reports.state = None;
    }

    /// The state, if one applies at `now`.
    pub fn current_state(&self, now: u64) -> Option<&ReportedState> {
        self.reports.state.as_ref().filter(|s| now < s.until)
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
