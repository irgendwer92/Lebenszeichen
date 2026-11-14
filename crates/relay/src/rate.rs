//! Delivery rate limit per mailbox.
//!
//! The arrival times live only in memory and only for one window, so the
//! relay keeps no lasting record of when something was delivered. After a
//! restart the limit starts afresh.

use std::collections::{HashMap, VecDeque};

use crate::ids::MailboxId;
use crate::limits::{MAX_DELIVERIES_PER_WINDOW, RATE_WINDOW_SECS};

/// Recent deliveries per mailbox.
#[derive(Default)]
pub struct RateLimiter {
    deliveries: HashMap<MailboxId, VecDeque<i64>>,
}

impl RateLimiter {
    /// Reserves one delivery at `now`. Returns `false` if the mailbox already
    /// had the maximum number of deliveries within the window.
    pub fn try_acquire(&mut self, mailbox: &MailboxId, now: i64) -> bool {
        let times = self.deliveries.entry(*mailbox).or_default();
        while times.front().is_some_and(|&t| t <= now - RATE_WINDOW_SECS) {
            times.pop_front();
        }
        if times.len() >= MAX_DELIVERIES_PER_WINDOW {
            return false;
        }
        times.push_back(now);
        true
    }

    /// Gives back a reservation made at `at` whose delivery was refused.
    pub fn release(&mut self, mailbox: &MailboxId, at: i64) {
        if let Some(times) = self.deliveries.get_mut(mailbox)
            && let Some(pos) = times.iter().rposition(|&t| t == at)
        {
            times.remove(pos);
        }
    }

    /// Forgets deliveries that left the window.
    pub fn prune(&mut self, now: i64) {
        self.deliveries.retain(|_, times| {
            times.retain(|&t| t > now - RATE_WINDOW_SECS);
            !times.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_slides() {
        let mut limiter = RateLimiter::default();
        let mailbox = [1u8; 32];
        for i in 0..MAX_DELIVERIES_PER_WINDOW as i64 {
            assert!(limiter.try_acquire(&mailbox, 1000 + i));
        }
        assert!(!limiter.try_acquire(&mailbox, 1010));
        limiter.release(&mailbox, 1005);
        assert!(limiter.try_acquire(&mailbox, 1011));
        assert!(!limiter.try_acquire(&mailbox, 1000 + RATE_WINDOW_SECS - 1));
        assert!(limiter.try_acquire(&mailbox, 1000 + RATE_WINDOW_SECS));
        limiter.prune(1000 + 2 * RATE_WINDOW_SECS);
        assert!(limiter.deliveries.is_empty());
    }
}
