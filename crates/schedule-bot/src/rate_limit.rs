use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(10);
const USER_LIMIT: usize = 6;
const CHAT_LIMIT: usize = 24;
const WARNING_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    User(i64),
    Chat(i64),
}

#[derive(Default)]
struct State {
    requests: HashMap<Key, VecDeque<Instant>>,
    last_warning: HashMap<i64, Instant>,
}

/// In-process sliding-window limiter shared across private and group handlers.
#[derive(Clone, Default)]
pub struct RateLimiter(Arc<Mutex<State>>);

impl RateLimiter {
    pub fn allow(&self, user_id: i64, chat_id: i64) -> bool {
        let now = Instant::now();
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        prune(&mut state, now);

        let user_key = Key::User(user_id);
        let chat_key = Key::Chat(chat_id);
        let user_requests = state.requests.get(&user_key).map_or(0, VecDeque::len);
        let chat_requests = state.requests.get(&chat_key).map_or(0, VecDeque::len);
        if user_requests >= USER_LIMIT || chat_requests >= CHAT_LIMIT {
            return false;
        }

        state.requests.entry(user_key).or_default().push_back(now);
        state.requests.entry(chat_key).or_default().push_back(now);
        true
    }

    /// Return true at most once per user during the warning cooldown.
    pub fn should_warn(&self, user_id: i64) -> bool {
        let now = Instant::now();
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        match state.last_warning.get(&user_id) {
            Some(last) if now.duration_since(*last) < WARNING_COOLDOWN => false,
            _ => {
                state.last_warning.insert(user_id, now);
                true
            }
        }
    }
}

fn prune(state: &mut State, now: Instant) {
    state.requests.retain(|_, requests| {
        while requests
            .front()
            .is_some_and(|request| now.duration_since(*request) >= WINDOW)
        {
            requests.pop_front();
        }
        !requests.is_empty()
    });
    state
        .last_warning
        .retain(|_, last| now.duration_since(*last) < WARNING_COOLDOWN);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_one_user_across_multiple_chats() {
        let limiter = RateLimiter::default();
        for index in 0..USER_LIMIT {
            assert!(limiter.allow(10, index as i64));
        }
        assert!(!limiter.allow(10, 999));
    }

    #[test]
    fn limits_total_traffic_in_one_group() {
        let limiter = RateLimiter::default();
        for user_id in 0..CHAT_LIMIT as i64 {
            assert!(limiter.allow(user_id, -100));
        }
        assert!(!limiter.allow(999, -100));
    }

    #[test]
    fn warning_is_suppressed_during_cooldown() {
        let limiter = RateLimiter::default();
        assert!(limiter.should_warn(42));
        assert!(!limiter.should_warn(42));
    }
}
