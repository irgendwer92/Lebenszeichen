//! Request rules: at most one open request, minimum interval after a manual
//! response, weekly limit.
//!
//! Both sides keep a [`Ledger`] per contact and direction and run the same
//! check. The person asked uses it on incoming requests with their own rules
//! and is the one that counts. The asking side uses it on its own sent
//! requests with the rules of the other side, only to lock the button and
//! show when the next request is possible.
//!
//! All times are Unix seconds. The time of a request is its sending time, so
//! that both sides count the same moment even when delivery was late.

use serde::{Deserialize, Serialize};

use crate::wire::{MessageId, Reason, Rules};

/// The weekly limit counts requests within this sliding window.
pub const WEEK_SECS: u64 = 7 * 24 * 60 * 60;

/// One request as seen by either side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(with = "crate::hex")]
    pub id: MessageId,
    /// Sending time of the request.
    pub at: u64,
    /// Time of the manual response, once there is one.
    pub answered_at: Option<u64>,
    /// Reason of the reported response, once one was sent or received.
    pub reported: Option<Reason>,
}

/// History of requests in one direction between two people.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Requests of the last week and any still open, oldest first.
    pub entries: Vec<Entry>,
    /// Time of the latest manual response, kept beyond the week because the
    /// minimum interval may be longer.
    pub last_answer: Option<u64>,
}

impl Ledger {
    /// Earliest time at which the next request is allowed.
    ///
    /// A request at `now` is allowed if the result is at most `now`. Returns
    /// [`u64::MAX`] if the weekly limit is zero.
    pub fn next_allowed(&self, rules: &Rules, now: u64) -> u64 {
        // Each of the three conditions, once met, stays met until the next
        // request or response. So the latest of them is the answer.
        let open = self.open(rules, now).map_or(0, |e| expiry(e, rules));
        let interval = self.last_answer.map_or(0, |at| {
            at.saturating_add(minutes(rules.min_interval_minutes))
        });
        let limit = usize::from(rules.weekly_limit);
        let in_week: Vec<u64> = self
            .entries
            .iter()
            .map(|e| e.at)
            .filter(|&at| now < at.saturating_add(WEEK_SECS))
            .collect();
        let weekly = match in_week.len().checked_sub(limit) {
            None => 0,
            Some(_) if limit == 0 => u64::MAX,
            // Entries are sorted, so after this one leaves the window, fewer
            // than `limit` remain.
            Some(excess) => in_week[excess].saturating_add(WEEK_SECS),
        };
        open.max(interval).max(weekly)
    }

    /// The open request, if any: not manually answered and not expired.
    pub fn open(&self, rules: &Rules, now: u64) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.answered_at.is_none() && now < expiry(e, rules))
    }

    /// Looks up a request that has not expired, answered or not.
    pub fn current_mut(&mut self, id: &MessageId, rules: &Rules, now: u64) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| &e.id == id && now < expiry(e, rules))
    }

    /// Records a request that was accepted or sent at `at`.
    pub fn record(&mut self, id: MessageId, at: u64) {
        let pos = self.entries.partition_point(|e| e.at <= at);
        self.entries.insert(
            pos,
            Entry {
                id,
                at,
                answered_at: None,
                reported: None,
            },
        );
    }

    /// Marks a request as manually answered at `at`. Returns `false` for an
    /// unknown or already answered request.
    pub fn answer(&mut self, id: &MessageId, at: u64) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| &e.id == id && e.answered_at.is_none())
        else {
            return false;
        };
        entry.answered_at = Some(at);
        self.last_answer = Some(self.last_answer.map_or(at, |last| last.max(at)));
        true
    }

    /// Forgets requests that neither count for the weekly limit nor are open.
    pub fn prune(&mut self, rules: &Rules, now: u64) {
        self.entries.retain(|e| {
            let in_week = now < e.at.saturating_add(WEEK_SECS);
            let open = e.answered_at.is_none() && now < expiry(e, rules);
            in_week || open
        });
    }
}

fn minutes(m: u32) -> u64 {
    u64::from(m) * 60
}

fn expiry(entry: &Entry, rules: &Rules) -> u64 {
    entry.at.saturating_add(minutes(rules.expiry_minutes))
}

#[cfg(test)]
mod tests {
    use super::{Ledger, WEEK_SECS};
    use crate::wire::Rules;

    const T: u64 = 1_794_600_000;
    const HOUR: u64 = 3600;

    fn answer(ledger: &mut Ledger, index: usize, at: u64) {
        let id = ledger.entries[index].id;
        assert!(ledger.answer(&id, at));
    }

    #[test]
    fn empty_ledger_allows_a_request() {
        assert!(Ledger::default().next_allowed(&Rules::DEFAULT, T) <= T);
    }

    #[test]
    fn open_request_blocks_until_expiry() {
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        assert_eq!(
            ledger.next_allowed(&Rules::DEFAULT, T + HOUR),
            T + 24 * HOUR
        );
        assert!(ledger.open(&Rules::DEFAULT, T + 24 * HOUR).is_none());
        assert!(ledger.next_allowed(&Rules::DEFAULT, T + 24 * HOUR) <= T + 24 * HOUR);
    }

    #[test]
    fn manual_answer_starts_minimum_interval() {
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        answer(&mut ledger, 0, T + HOUR);
        assert!(ledger.open(&Rules::DEFAULT, T + HOUR).is_none());
        assert_eq!(ledger.next_allowed(&Rules::DEFAULT, T + HOUR), T + 7 * HOUR);
    }

    #[test]
    fn reported_answer_keeps_request_open() {
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        ledger.entries[0].reported = Some(crate::wire::Reason::new("schläft").unwrap());
        assert!(ledger.open(&Rules::DEFAULT, T + HOUR).is_some());
    }

    #[test]
    fn weekly_limit_counts_sliding_seven_days() {
        let rules = Rules {
            min_interval_minutes: 0,
            expiry_minutes: 60,
            weekly_limit: 3,
        };
        let mut ledger = Ledger::default();
        for (i, at) in [T, T + 10 * HOUR, T + 20 * HOUR].into_iter().enumerate() {
            assert!(ledger.next_allowed(&rules, at) <= at);
            ledger.record([i as u8; 16], at);
            answer(&mut ledger, i, at + 1);
        }
        // The fourth request waits until the first one leaves the window.
        assert_eq!(ledger.next_allowed(&rules, T + 30 * HOUR), T + WEEK_SECS);
        assert!(ledger.next_allowed(&rules, T + WEEK_SECS) <= T + WEEK_SECS);
    }

    #[test]
    fn unanswered_expired_requests_count_too() {
        let rules = Rules {
            min_interval_minutes: 0,
            expiry_minutes: 60,
            weekly_limit: 1,
        };
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        assert_eq!(ledger.next_allowed(&rules, T + 2 * HOUR), T + WEEK_SECS);
    }

    #[test]
    fn weekly_limit_zero_never_allows() {
        let rules = Rules {
            weekly_limit: 0,
            ..Rules::DEFAULT
        };
        assert_eq!(Ledger::default().next_allowed(&rules, T), u64::MAX);
    }

    #[test]
    fn minimum_interval_may_exceed_a_week() {
        let rules = Rules {
            min_interval_minutes: 10 * 24 * 60,
            ..Rules::DEFAULT
        };
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        answer(&mut ledger, 0, T);
        ledger.prune(&rules, T + 8 * 24 * HOUR);
        assert!(ledger.entries.is_empty());
        assert_eq!(
            ledger.next_allowed(&rules, T + 8 * 24 * HOUR),
            T + 10 * 24 * HOUR
        );
    }

    #[test]
    fn prune_keeps_open_and_recent_requests() {
        let rules = Rules {
            expiry_minutes: 10 * 24 * 60,
            ..Rules::DEFAULT
        };
        let mut ledger = Ledger::default();
        ledger.record([1; 16], T);
        ledger.record([2; 16], T + HOUR);
        answer(&mut ledger, 1, T + 2 * HOUR);
        ledger.prune(&rules, T + 8 * 24 * HOUR);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].id, [1; 16]);
    }

    #[test]
    fn record_keeps_entries_sorted() {
        let mut ledger = Ledger::default();
        ledger.record([2; 16], T + 5);
        ledger.record([1; 16], T);
        assert_eq!(ledger.entries[0].id, [1; 16]);
    }
}
