use dashmap::DashMap;
use teloxide::{Bot, adaptors::DefaultParseMode};

pub type BotWrapped = DefaultParseMode<Bot>;

#[derive(Default)]
pub struct DataStore {
    pub active_downloads: DashMap<u64, i32>,
}
