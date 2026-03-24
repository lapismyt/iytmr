use std::{fs, path::PathBuf};

use walkdir::WalkDir;

use crate::consts::{MAX_CACHE_SIZE_MB, MIN_CACHE_SIZE_MB, OUTPUT_DIR};

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
