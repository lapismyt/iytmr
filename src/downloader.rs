use std::path::{Path, PathBuf};

use yt_dlp::{
    extractor::VideoExtractor, metadata::MetadataManager, model::playlist::Playlist,
    model::selector::ThumbnailQuality, prelude::*,
};

pub struct Downloader {
    client: yt_dlp::Downloader,
    metadata_manager: MetadataManager,
    quality: yt_dlp::model::AudioQuality,
    codec: yt_dlp::model::AudioCodecPreference,
}

impl Downloader {
    pub async fn new<P: AsRef<Path>>(
        output_dir: P,
        cache_dir: P,
        libs_dir: P,
    ) -> anyhow::Result<Self> {
        let cache_config = CacheConfig::builder()
            .cache_dir(PathBuf::from(cache_dir.as_ref()))
            .persistent_backend(Some(PersistentBackendKind::Redb))
            .build();

        let downloader = yt_dlp::Downloader::with_new_binaries(
            PathBuf::from(libs_dir.as_ref()),
            PathBuf::from(output_dir.as_ref()),
        )
        .await?
        .with_cache_config(cache_config)
        .build()
        .await?;

        Ok(Self {
            client: downloader,
            metadata_manager: MetadataManager::new(),
            quality: yt_dlp::model::AudioQuality::Best,
            codec: yt_dlp::model::AudioCodecPreference::Opus,
        })
    }

    pub async fn download<U: Into<String>>(&self, url: U) -> anyhow::Result<(Video, PathBuf)> {
        let video = self
            .client
            .youtube_extractor()
            .fetch_video(&url.into())
            .await?;

        let video_id = video.id.clone();
        let audio_filename = format!("{}.mp3", video_id);

        let audio_path = self
            .client
            .download_audio_stream_with_quality(
                &video,
                &audio_filename,
                self.quality,
                self.codec.clone(),
            )
            .await?;

        // Add metadata
        if let Err(e) = self
            .metadata_manager
            .add_metadata(&audio_path, &video)
            .await
        {
            log::error!("Failed to add metadata to {}: {}", audio_path.display(), e);
        }

        // Handle thumbnail
        let thumbnail_filename = format!("{}.jpg", video_id);
        match self
            .client
            .download_thumbnail(&video, ThumbnailQuality::Best, &thumbnail_filename)
            .await
        {
            Ok(thumbnail_path) => {
                if let Err(e) = self
                    .metadata_manager
                    .add_thumbnail_to_file(&audio_path, &thumbnail_path)
                    .await
                {
                    log::error!("Failed to add thumbnail to {}: {}", audio_path.display(), e);
                }
                // Clean up thumbnail file
                let _ = tokio::fs::remove_file(thumbnail_path).await;
            }
            Err(e) => {
                log::warn!("Failed to download thumbnail for {}: {}", video_id, e);
            }
        }

        Ok((video, audio_path))
    }

    pub async fn search(&self, query: &str, max_results: usize) -> anyhow::Result<Playlist> {
        Ok(self
            .client
            .youtube_extractor()
            .search(query, max_results)
            .await?)
    }
}
