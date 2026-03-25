use std::path::PathBuf;

use chrono::{DateTime, Utc};
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

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedVideo {
    pub file_id: String,
    pub title: String,
    pub performer: String,
    pub duration: u32,
    pub thumbnail: PathBuf,
    pub expires_at: DateTime<Utc>,
    pub path: PathBuf,
    pub video_id: String,
}

impl SavedVideo {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}
