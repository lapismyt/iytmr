use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct UserInfo {
    pub last_seen: u64,
    pub dl_count: u32,
    pub register_date: u64,
}

impl Default for UserInfo {
    fn default() -> Self {
        Self {
            last_seen: Utc::now().timestamp() as u64,
            dl_count: 0,
            register_date: Utc::now().timestamp() as u64,
        }
    }
}
