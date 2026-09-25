pub mod domain;
pub mod handlers;
pub mod store;

use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use store::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub bootstrap_token: Option<String>,
    pub timezone: Tz,
}

impl AppState {
    pub fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.timezone).date_naive()
    }
}
