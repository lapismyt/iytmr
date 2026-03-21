use std::path::{Path, PathBuf};

use yt_dlp::{
    extractor::{ExtractorBase, VideoExtractor},
    model::playlist::Playlist,
    prelude::*,
};

pub struct Downloader {
    client: yt_dlp::Downloader,
    output_dir: PathBuf,
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
            output_dir: PathBuf::from(output_dir.as_ref()),
            quality: yt_dlp::model::AudioQuality::Best,
            codec: yt_dlp::model::AudioCodecPreference::Opus,
        })
    }

    pub async fn download(&self, url: &str) -> anyhow::Result<PathBuf> {
        let video = self.client.youtube_extractor().fetch_video(url).await?;

        Ok(self
            .client
            .download_audio_stream_with_quality(
                &video,
                format!("{}.mp3", video.id),
                self.quality,
                self.codec.clone(),
            )
            .await?)
    }

    pub async fn get_video_metadata(&self, url: &str) -> anyhow::Result<Video> {
        Ok(self
            .client
            .youtube_extractor()
            .fetch_video_metadata(url)
            .await?)
    }

    pub async fn search(&self, query: &str, max_results: usize) -> anyhow::Result<Playlist> {
        Ok(self
            .client
            .youtube_extractor()
            .search(query, max_results)
            .await?)
    }
}
