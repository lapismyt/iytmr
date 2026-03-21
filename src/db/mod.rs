use chrono::{DateTime, Utc};
use redb::{ReadableDatabase, ReadableTableMetadata as _, TableDefinition};
use rmp_serde::{decode, encode};

use crate::db::models::UserInfo;

pub mod models;

const TG_ID_TO_USER_INFO: TableDefinition<u64, &[u8]> = TableDefinition::new("tg_id_to_user_info");
const YT_ID_TO_DL_COUNT: TableDefinition<&str, u32> = TableDefinition::new("yt_id_to_dl_count");

pub struct DatabaseHelper {
    db: redb::Database,
}

impl DatabaseHelper {
    pub fn from_redb(db: redb::Database) -> Self {
        Self { db }
    }

    pub fn new(db_path: &str) -> Self {
        let db = redb::Database::create(db_path).unwrap();
        Self { db }
    }

    pub fn get_user_info(&self, tg_id: u64) -> anyhow::Result<models::UserInfo> {
        let Some(read_txn) = self.db.begin_read().ok() else {
            return Err(anyhow::anyhow!("Failed to begin read"));
        };
        let Ok(tg_id_to_user_info) = read_txn.open_table(TG_ID_TO_USER_INFO) else {
            return Err(anyhow::anyhow!("Failed to open table"));
        };
        let Ok(Some(entry)) = tg_id_to_user_info.get(&tg_id) else {
            return Err(anyhow::anyhow!("User not found"));
        };
        let user_info_slice = entry.value();
        let Ok(user_info) = decode::from_slice::<models::UserInfo>(user_info_slice) else {
            return Err(anyhow::anyhow!("Failed to decode user info"));
        };
        Ok(user_info)
    }

    pub fn get_user_dl_count(&self, tg_id: u64) -> u32 {
        let Ok(user_info) = self.get_user_info(tg_id) else {
            return 0;
        };

        user_info.dl_count
    }

    async fn get_user_last_seen(&self, tg_id: u64) -> Option<DateTime<Utc>> {
        let Ok(user_info) = self.get_user_info(tg_id) else {
            return None;
        };

        if let Some(last_seen) = DateTime::from_timestamp(user_info.last_seen as i64, 0) {
            return Some(last_seen);
        }

        None
    }

    pub fn get_total_users_count(&self) -> anyhow::Result<u64> {
        let read_txn = self.db.begin_read()?;
        let tg_id_to_user_info = read_txn.open_table(TG_ID_TO_USER_INFO)?;
        let total_users = tg_id_to_user_info.len()? as u64;

        Ok(total_users)
    }

    pub fn get_video_dl_count(&self, video_id: &str) -> anyhow::Result<u32> {
        let read_txn = self.db.begin_read()?;
        let yt_id_to_dl_count = read_txn.open_table(YT_ID_TO_DL_COUNT)?;
        let count = match yt_id_to_dl_count.get(video_id)? {
            Some(entry) => entry.value(),
            None => 0,
        };

        Ok(count)
    }

    pub fn handle_user_interaction(&self, tg_id: u64) -> anyhow::Result<()> {
        let (mut user_info, is_exists) = match self.get_user_info(tg_id) {
            Ok(info) => (info, true),
            Err(_) => (UserInfo::default(), false),
        };

        if is_exists {
            user_info.last_seen = Utc::now().timestamp() as u64;
        }

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TG_ID_TO_USER_INFO)?;
            table.insert(tg_id, encode::to_vec(&user_info)?.as_slice())?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn increment_video_dl_counter(&self, video_id: &str) -> anyhow::Result<()> {
        let count = self.get_video_dl_count(video_id)?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(YT_ID_TO_DL_COUNT)?;
            table.insert(video_id, count + 1)?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn increment_user_dl_counter(&self, tg_id: u64) -> anyhow::Result<()> {
        let (mut user_info, is_exists) = match self.get_user_info(tg_id) {
            Ok(info) => (info, true),
            Err(_) => (UserInfo::default(), false),
        };

        if !is_exists {
            return Err(anyhow::anyhow!("User not found"));
        }

        user_info.dl_count += 1;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TG_ID_TO_USER_INFO)?;
            table.insert(tg_id, encode::to_vec(&user_info)?.as_slice())?;
        }
        write_txn.commit()?;

        Ok(())
    }
}
