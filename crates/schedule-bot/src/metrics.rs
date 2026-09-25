use std::sync::atomic::{AtomicU64, Ordering};

pub static RATE_LIMITED: AtomicU64 = AtomicU64::new(0);
pub static OUTBOX_SENT: AtomicU64 = AtomicU64::new(0);
pub static OUTBOX_FAILED: AtomicU64 = AtomicU64::new(0);
pub static DAILY_ENQUEUED: AtomicU64 = AtomicU64::new(0);
pub static SCHEDULER_LAST_SUCCESS: AtomicU64 = AtomicU64::new(0);

pub fn read(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}
