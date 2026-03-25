use std::{fs, path::PathBuf};

use dashmap::DashMap;
use walkdir::WalkDir;

use crate::{
    consts::{MAX_CACHE_SIZE_MB, MIN_CACHE_SIZE_MB, OUTPUT_DIR},
    db::DatabaseHelper,
};

#[derive(Debug)]
pub struct CounterCached {
    count: u64,
    updated_at: std::time::Instant,
}

impl CounterCached {
    pub fn increment(&mut self) {
        self.count += 1;
        self.updated_at = std::time::Instant::now();
    }
}

#[derive(Debug)]
pub struct DataStore {
    pub active_downloads: DashMap<u64, i32>,
    pub downloaded_files_count: CounterCached,
    pub cached_files_count: CounterCached,
    pub total_users_count: CounterCached,
    pub monthly_users_count: CounterCached,
}

impl DataStore {
    pub fn get_downloaded_files_count(&mut self) -> u64 {
        if std::time::Instant::now().duration_since(self.downloaded_files_count.updated_at)
            > std::time::Duration::from_secs(180)
        {
            self.downloaded_files_count.count = get_downloaded_files_count()
                .unwrap_or(self.downloaded_files_count.count as usize)
                as u64;
        }

        self.downloaded_files_count.count
    }

    fn get_downloaded_files_count_raw() -> u64 {
        get_downloaded_files_count().unwrap_or(0) as u64
    }

    pub fn get_cached_files_count<D: AsRef<DatabaseHelper>>(&mut self, db: D) -> u64 {
        if std::time::Instant::now().duration_since(self.cached_files_count.updated_at)
            > std::time::Duration::from_secs(180)
        {
            self.cached_files_count.count = db
                .as_ref()
                .get_cached_files_count()
                .unwrap_or(self.cached_files_count.count);
        }

        self.cached_files_count.count
    }

    fn get_cached_files_count_raw<D: AsRef<DatabaseHelper>>(db: D) -> u64 {
        db.as_ref().get_cached_files_count().unwrap_or(0)
    }

    pub fn get_total_users_count<D: AsRef<DatabaseHelper>>(&mut self, db: D) -> u64 {
        if std::time::Instant::now().duration_since(self.total_users_count.updated_at)
            > std::time::Duration::from_secs(180)
        {
            self.total_users_count.count = db
                .as_ref()
                .get_total_users_count()
                .unwrap_or(self.total_users_count.count);
        }

        self.total_users_count.count
    }

    fn get_total_users_count_raw<D: AsRef<DatabaseHelper>>(db: D) -> u64 {
        db.as_ref().get_total_users_count().unwrap_or(0)
    }

    pub fn get_cached_monthly_users_count<D: AsRef<DatabaseHelper>>(&mut self, db: D) -> u64 {
        if std::time::Instant::now().duration_since(self.monthly_users_count.updated_at)
            > std::time::Duration::from_secs(180)
        {
            self.monthly_users_count.count =
                db.as_ref()
                    .get_monthly_active_users_count()
                    .unwrap_or(self.monthly_users_count.count as usize) as u64;
        }

        self.monthly_users_count.count
    }

    fn get_cached_monthly_users_count_raw<D: AsRef<DatabaseHelper>>(db: D) -> u64 {
        db.as_ref().get_monthly_active_users_count().unwrap_or(0) as u64
    }

    pub fn new<D: AsRef<DatabaseHelper>>(db: D) -> Self {
        Self {
            downloaded_files_count: CounterCached {
                count: DataStore::get_downloaded_files_count_raw(),
                updated_at: std::time::Instant::now(),
            },
            cached_files_count: CounterCached {
                count: DataStore::get_cached_files_count_raw(&db),
                updated_at: std::time::Instant::now(),
            },
            active_downloads: DashMap::new(),
            total_users_count: CounterCached {
                count: DataStore::get_total_users_count_raw(&db),
                updated_at: std::time::Instant::now(),
            },
            monthly_users_count: CounterCached {
                count: DataStore::get_cached_monthly_users_count_raw(&db),
                updated_at: std::time::Instant::now(),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputFolderInfo {
    total_size: u64,
    files: Vec<OutputFile>,
}

#[derive(Debug, Clone)]
pub struct OutputFile {
    path: PathBuf,
    size: u64,
}

pub fn get_output_folder_info() -> anyhow::Result<OutputFolderInfo> {
    let mut files = Vec::new();
    let mut total_size = 0;

    for entry in WalkDir::new(OUTPUT_DIR).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let size = entry.metadata()?.len();
            total_size += size;

            files.push(OutputFile {
                path: entry.path().to_path_buf(),
                size,
            });
        }
    }

    files.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    Ok(OutputFolderInfo { total_size, files })
}

pub fn cache_check() -> anyhow::Result<()> {
    let folder_info = get_output_folder_info()?;
    if folder_info.total_size < *MAX_CACHE_SIZE_MB * 1024 * 1024 {
        return Ok(());
    }

    log::info!(
        "Max cache size exceeded: {} MB, clearing cache",
        folder_info.total_size / (1024 * 1024)
    );

    let mut result_size = folder_info.total_size;
    if result_size == 0 {
        return Err(anyhow::anyhow!("Output folder is empty"));
    }

    let min_limit = *MIN_CACHE_SIZE_MB * 1024 * 1024;

    let _ = folder_info
        .files
        .iter()
        .map_while(|f| {
            if result_size <= min_limit {
                return None;
            }
            if let Err(e) = fs::remove_file(&f.path) {
                log::warn!("Failed to remove file: {}", e);
            } else {
                result_size -= f.size;
            }
            Some(f.size)
        })
        .collect::<Vec<_>>();

    log::info!(
        "Cleared {} MB of cache",
        (folder_info.total_size - result_size) / 1024 / 1024
    );
    Ok(())
}

pub fn get_downloaded_files_count() -> anyhow::Result<usize> {
    let folder_info = get_output_folder_info()?;
    Ok(folder_info.files.len())
}
