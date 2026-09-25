pub mod archive;
pub mod domain;
pub mod format;
pub mod handlers;
pub mod metrics;
pub mod rate_limit;
pub mod store;

use archive::SourceArchive;
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use rate_limit::RateLimiter;
use store::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub bootstrap_token: Option<String>,
    pub timezone: Tz,
    pub bot_username: String,
    pub rate_limiter: RateLimiter,
    pub archive: Option<SourceArchive>,
}

impl AppState {
    pub fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.timezone).date_naive()
    }
}
