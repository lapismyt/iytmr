use chrono::{DateTime, TimeDelta, Utc};
use redb::{ReadableDatabase, ReadableTable as _, ReadableTableMetadata as _, TableDefinition};
use rmp_serde::{decode, encode};

use crate::db::models::{SavedVideo, UserInfo};

pub mod models;

const TG_ID_TO_USER_INFO: TableDefinition<u64, &[u8]> = TableDefinition::new("tg_id_to_user_info");
const YT_ID_TO_DL_COUNT: TableDefinition<&str, u32> = TableDefinition::new("yt_id_to_dl_count");
const VIDEO_ID_TO_SAVED_VIDEO: TableDefinition<&str, &[u8]> =
    TableDefinition::new("video_id_to_tg_file_id");

pub struct DatabaseHelper {
    db: redb::Database,
}

impl DatabaseHelper {
    pub fn new(db_path: &str) -> Self {
        let db = redb::Database::create(db_path).unwrap();
        Self { db }
    }

    pub fn get_user_info(&self, tg_id: &u64) -> anyhow::Result<models::UserInfo> {
        let Some(read_txn) = self.db.begin_read().ok() else {
            return Err(anyhow::anyhow!("Failed to begin read"));
        };
        let Ok(tg_id_to_user_info) = read_txn.open_table(TG_ID_TO_USER_INFO) else {
            return Err(anyhow::anyhow!("Failed to open table"));
        };
        let Ok(Some(entry)) = tg_id_to_user_info.get(tg_id) else {
            return Err(anyhow::anyhow!("User not found"));
        };
        let user_info_slice = entry.value();
        let Ok(user_info) = decode::from_slice::<models::UserInfo>(user_info_slice) else {
            return Err(anyhow::anyhow!("Failed to decode user info"));
        };
        Ok(user_info)
    }

    pub fn get_user_dl_count(&self, tg_id: &u64) -> u32 {
        let Ok(user_info) = self.get_user_info(tg_id) else {
            return 0;
        };

        user_info.dl_count
    }

    pub fn get_total_dl_count(&self) -> anyhow::Result<u32> {
        let read_txn = self.db.begin_read()?;
        let tg_id_to_user_info = read_txn.open_table(TG_ID_TO_USER_INFO)?;
        let Ok(entries) = tg_id_to_user_info.iter() else {
            return Ok(0);
        };
        let total_dl_count: u32 = entries
            .filter_map(|entry| {
                let Ok(entry) = entry else {
                    return None;
                };
                let Ok(user_info) = decode::from_slice::<models::UserInfo>(entry.1.value()) else {
                    return None;
                };
                Some(user_info.dl_count)
            })
            .sum();

        Ok(total_dl_count)
    }

    // pub fn get_user_last_seen(&self, tg_id: &u64) -> Option<DateTime<Utc>> {
    //     let Ok(user_info) = self.get_user_info(tg_id) else {
    //         return None;
    //     };

    //     if let Some(last_seen) = DateTime::from_timestamp(user_info.last_seen as i64, 0) {
    //         return Some(last_seen);
    //     }

    //     None
    // }

    pub fn get_monthly_active_users_count(&self) -> anyhow::Result<usize> {
        let read_txn = self.db.begin_read()?;
        let tg_id_to_user_info = read_txn.open_table(TG_ID_TO_USER_INFO)?;
        let Ok(entries) = tg_id_to_user_info.iter() else {
            return Ok(0);
        };
        let total_users_count: usize = entries
            .filter_map(|entry| {
                let Ok(entry) = entry else {
                    return None;
                };
                let Ok(user_info) = decode::from_slice::<models::UserInfo>(entry.1.value()) else {
                    return None;
                };
                if DateTime::from_timestamp(user_info.last_seen as i64, 0).unwrap_or_default()
                    < (Utc::now() - TimeDelta::days(30))
                {
                    return None;
                }
                Some(1)
            })
            .collect::<Vec<_>>()
            .len();

        Ok(total_users_count)
    }

    pub fn get_user_register_date(&self, tg_id: &u64) -> Option<DateTime<Utc>> {
        let Ok(user_info) = self.get_user_info(tg_id) else {
            return None;
        };

        if let Some(register_date) = DateTime::from_timestamp(user_info.register_date as i64, 0) {
            return Some(register_date);
        }

        None
    }

    pub fn get_total_users_count(&self) -> anyhow::Result<u64> {
        let read_txn = self.db.begin_read()?;
        let tg_id_to_user_info = read_txn.open_table(TG_ID_TO_USER_INFO)?;
        let total_users = tg_id_to_user_info.len()?;

        Ok(total_users)
    }

    fn create_video_dl_count(&self, video_id: &str) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut yt_id_to_dl_count = write_txn.open_table(YT_ID_TO_DL_COUNT)?;
            yt_id_to_dl_count.insert(video_id, &0)?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn get_video_dl_count(&self, video_id: &str) -> anyhow::Result<u32> {
        let read_txn = self.db.begin_read()?;
        let Ok(yt_id_to_dl_count) = read_txn.open_table(YT_ID_TO_DL_COUNT) else {
            self.create_video_dl_count(video_id)?;
            return Ok(0);
        };
        let count = match yt_id_to_dl_count.get(video_id)? {
            Some(entry) => entry.value(),
            None => 0,
        };

        Ok(count)
    }

    pub fn handle_user_interaction(&self, tg_id: &u64) -> anyhow::Result<()> {
        let (mut user_info, is_exists) = match self.get_user_info(tg_id) {
            Ok(info) => (info, true),
            Err(_) => (UserInfo::default(), false),
        };

        user_info.last_seen = Utc::now().timestamp() as u64;

        if !is_exists {
            user_info.register_date = Utc::now().timestamp() as u64;
        }

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TG_ID_TO_USER_INFO)?;
            table.insert(tg_id, encode::to_vec(&user_info)?.as_slice())?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn increment_video_dl_counter<V: Into<String>>(&self, video_id: V) -> anyhow::Result<()> {
        let video_id = video_id.into();
        let count = self.get_video_dl_count(&video_id)?;

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(YT_ID_TO_DL_COUNT)?;
            table.insert(video_id.as_str(), count + 1)?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn increment_user_dl_counter(&self, tg_id: &u64) -> anyhow::Result<()> {
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

    pub fn save_video(&self, saved_video: &SavedVideo) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(VIDEO_ID_TO_SAVED_VIDEO)?;
            table.insert(
                saved_video.video_id.as_str(),
                encode::to_vec(saved_video)?.as_slice(),
            )?;
        }
        write_txn.commit()?;

        Ok(())
    }

    pub fn get_saved_video<V: Into<String>>(&self, video_id: V) -> Option<SavedVideo> {
        let video_id = video_id.into();
        let read_txn = self.db.begin_read().ok()?;
        let table = read_txn.open_table(VIDEO_ID_TO_SAVED_VIDEO).ok()?;
        let value: SavedVideo =
            decode::from_slice(table.get(video_id.as_str()).ok().flatten()?.value()).ok()?;

        match value.is_expired() {
            true => None,
            false => Some(value),
        }
    }
}
