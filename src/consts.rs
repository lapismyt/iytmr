use std::sync::LazyLock;

pub const OUTPUT_DIR: &str = "output";
pub const LIBS_DIR: &str = "libs";
pub const CACHE_DIR: &str = "cache";
pub const DB_PATH: &str = "iytmr.redb";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const NO_RESULTS_ID: LazyLock<String> = LazyLock::new(|| format!("no_results:{}", VERSION));

pub const MAX_RESULTS: LazyLock<usize> = LazyLock::new(|| {
    if let Ok(max_results) = std::env::var("MAX_RESULTS")
        && let Ok(max_results) = max_results.parse::<usize>()
    {
        log::info!("MAX_RESULTS set to {}", max_results);
        return max_results;
    }

    log::warn!("MAX_RESULTS is not a valid number, defaulting to 5");
    5
});

pub const MIN_DURATION: LazyLock<Option<f64>> = LazyLock::new(|| {
    if let Ok(min_duration) = std::env::var("MIN_DURATION") {
        if let Ok(min_duration) = min_duration.parse::<f64>() {
            log::info!("MIN_DURATION set to {}", min_duration);
            return Some(min_duration);
        } else {
            log::warn!("MIN_DURATION is not a valid number");
        }
    }

    None
});

pub const MAX_DURATION: LazyLock<Option<f64>> = LazyLock::new(|| {
    if let Ok(max_duration) = std::env::var("MAX_DURATION") {
        if let Ok(max_duration) = max_duration.parse::<f64>() {
            log::info!("MAX_DURATION set to {}", max_duration);
            return Some(max_duration);
        } else {
            log::warn!("MAX_DURATION is not a valid number");
        }
    }

    None
});

pub const TRASH_CHAT_ID: LazyLock<i64> = LazyLock::new(|| {
    std::env::var("TRASH_CHAT_ID")
        .unwrap()
        .parse::<i64>()
        .unwrap()
});

pub const MAX_USER_PARALLEL_DOWNLOADS: LazyLock<usize> = LazyLock::new(|| {
    if let Ok(max_downloads) = std::env::var("MAX_USER_PARALLEL_DOWNLOADS") {
        if let Ok(max_downloads) = max_downloads.parse::<usize>() {
            log::info!("MAX_USER_PARALLEL_DOWNLOADS set to {}", max_downloads);
            return max_downloads;
        } else {
            log::warn!("MAX_USER_PARALLEL_DOWNLOADS is not a valid number");
        }
    }

    log::warn!("MAX_USER_PARALLEL_DOWNLOADS is not set, defaulting to 2");
    2
});

pub const BLANK_PLACEHOLDER: LazyLock<String> = LazyLock::new(|| {
    if let Ok(placeholder) = std::env::var("BLANK_PLACEHOLDER") {
        log::info!("BLANK_PLACEHOLDER set to {}", placeholder);
        return placeholder;
    }

    "[[ BLANK ]]".to_string()
});

pub const MIN_CACHE_SIZE_MB: LazyLock<u64> = LazyLock::new(|| {
    if let Ok(min_cache_size) = std::env::var("MIN_CACHE_SIZE_MB")
        && let Ok(min_cache_size) = min_cache_size.parse::<u64>()
    {
        log::info!("MIN_CACHE_SIZE_MB set to {}", min_cache_size);
        return min_cache_size;
    }

    log::warn!("MIN_CACHE_SIZE_MB is not a valid number, defaulting to 512");
    512
});

pub const MAX_CACHE_SIZE_MB: LazyLock<u64> = LazyLock::new(|| {
    if let Ok(max_cache_size) = std::env::var("MAX_CACHE_SIZE_MB")
        && let Ok(max_cache_size) = max_cache_size.parse::<u64>()
    {
        log::info!("MAX_CACHE_SIZE_MB set to {}", max_cache_size);
        return max_cache_size;
    }

    log::warn!("MAX_CACHE_SIZE_MB is not a valid number, defaulting to 1024");
    1024
});
