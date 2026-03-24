use std::path::{Path, PathBuf};

use yt_dlp::{
    extractor::VideoExtractor, model::playlist::Playlist, model::selector::ThumbnailQuality,
    prelude::*,
};

pub struct Downloader {
    client: yt_dlp::Downloader,
    ffmpeg_path: PathBuf,
    quality: yt_dlp::model::AudioQuality,
    codec: yt_dlp::model::AudioCodecPreference,
    output_dir: PathBuf,
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

        let ffmpeg_path = libs_dir.as_ref().join("ffmpeg");

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
            ffmpeg_path,
            quality: yt_dlp::model::AudioQuality::Best,
            codec: yt_dlp::model::AudioCodecPreference::MP3,
            output_dir: PathBuf::from(output_dir.as_ref()),
        })
    }

    pub async fn download<U: Into<String>>(
        &self,
        url: U,
    ) -> anyhow::Result<(Video, PathBuf, Option<PathBuf>)> {
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

        // Handle thumbnail
        let thumbnail_filename = format!("{}.jpg", video_id);
        let thumbnail_path = match self
            .client
            .download_thumbnail(
                &video,
                ThumbnailQuality::Best,
                self.output_dir.join(thumbnail_filename),
            )
            .await
        {
            Ok(path) => Some(path),
            Err(e) => {
                log::warn!("Failed to download thumbnail for {}: {}", video_id, e);
                None
            }
        };

        // Create cropped thumbnail for Telegram and metadata
        let cropped_thumbnail_path = if let Some(path) = &thumbnail_path {
            match self.create_cropped_thumbnail(path).await {
                Ok(cropped_path) => Some(cropped_path),
                Err(e) => {
                    log::error!("Failed to create cropped thumbnail: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Add metadata manually via ffmpeg
        if let Err(e) = self
            .add_metadata_manual(&audio_path, &video, cropped_thumbnail_path.as_deref())
            .await
        {
            log::error!("Failed to add metadata to {}: {}", audio_path.display(), e);
        }

        // Clean up original thumbnail file if it exists
        if let Some(path) = thumbnail_path {
            let _ = tokio::fs::remove_file(path).await;
        }

        Ok((video, audio_path, cropped_thumbnail_path))
    }

    async fn create_cropped_thumbnail(&self, original_path: &Path) -> anyhow::Result<PathBuf> {
        let cropped_path = original_path.with_extension("thumb.jpg");
        let mut cmd = tokio::process::Command::new(&self.ffmpeg_path);

        cmd.arg("-i")
            .arg(original_path)
            .arg("-vf")
            .arg("crop='min(iw,ih):min(iw,ih)',scale=512:512")
            .arg("-y")
            .arg(&cropped_path);

        let output = cmd.output().await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg thumbnail crop failed: {}", error);
        }

        Ok(cropped_path)
    }

    async fn add_metadata_manual(
        &self,
        audio_path: &Path,
        video: &Video,
        thumbnail_path: Option<&Path>,
    ) -> anyhow::Result<()> {
        let temp_path = audio_path.with_extension("temp.mp3");
        let mut cmd = tokio::process::Command::new(&self.ffmpeg_path);

        // Input audio
        cmd.arg("-i").arg(audio_path);

        // Input thumbnail if exists
        if let Some(thumb) = thumbnail_path {
            cmd.arg("-i").arg(thumb);
        }

        // Mapping and Codecs
        if thumbnail_path.is_some() {
            // Map audio from first input, video from second
            cmd.args(["-map", "0:a", "-map", "1:v"]);
            // Re-encode to mp3 for the audio stream, copy for the thumbnail image (already processed)
            cmd.args(["-c:a", "libmp3lame", "-q:a", "2", "-c:v", "copy"]);
            // Metadata for the video stream to mark it as cover art
            cmd.args([
                "-metadata:s:v",
                "title=Album cover",
                "-metadata:s:v",
                "comment=Cover (front)",
            ]);
        } else {
            // Just re-encode audio to mp3
            cmd.args(["-c:a", "libmp3lame", "-q:a", "2"]);
        }

        // ID3v2 version for better compatibility
        cmd.args(["-id3v2_version", "3"]);

        // General Metadata
        cmd.arg("-metadata").arg(format!("title={}", video.title));
        if let Some(uploader) = &video.uploader {
            cmd.arg("-metadata").arg(format!("artist={}", uploader));
        }
        if let Some(channel) = &video.channel {
            cmd.arg("-metadata").arg(format!("album={}", channel));
        }
        if let Some(date) = &video.upload_date {
            cmd.arg("-metadata").arg(format!("date={}", date));
        }

        // Overwrite and output
        cmd.arg("-y").arg(&temp_path);

        let output = cmd.output().await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg failed: {}", error);
        }

        tokio::fs::rename(temp_path, audio_path).await?;
        Ok(())
    }

    pub async fn search(&self, query: &str, max_results: usize) -> anyhow::Result<Playlist> {
        Ok(self
            .client
            .youtube_extractor()
            .search(query, max_results)
            .await?)
    }
}
