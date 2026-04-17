use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use yt_dlp::{
    error::Error as YtDlpError,
    extractor::VideoExtractor,
    model::{format::FormatType, playlist::Playlist, selector::ThumbnailQuality},
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

    fn sha256_hash(video_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(video_id);
        format!("{:x}", hasher.finalize())
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
        let video_id_hash = Downloader::sha256_hash(&video_id);

        let audio_filename = format!("{}.mp3", video_id_hash);

        let audio_path = self
            .download_audio_with_fallback(&video, &audio_filename)
            .await?;

        // Handle thumbnail
        let thumbnail_filename = format!("{}.jpg", video_id_hash);
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

    async fn download_audio_with_fallback(
        &self,
        video: &Video,
        audio_filename: &str,
    ) -> anyhow::Result<PathBuf> {
        match self
            .client
            .download_audio_stream_with_quality(
                video,
                audio_filename,
                self.quality,
                self.codec.clone(),
            )
            .await
        {
            Ok(audio_path) => Ok(audio_path),
            Err(YtDlpError::FormatNotAvailable { format_type, .. })
                if format_type == FormatType::Audio =>
            {
                log::warn!(
                    "No standalone audio format for {}, falling back to muxed video download",
                    video.id
                );
                self.download_muxed_video_and_extract_audio(video, audio_filename)
                    .await
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn download_muxed_video_and_extract_audio(
        &self,
        video: &Video,
        audio_filename: &str,
    ) -> anyhow::Result<PathBuf> {
        let combined_format = video.best_audio_video_format()?;
        let temp_video_path = self.output_dir.join(format!(
            "{}.fallback.{}",
            Path::new(audio_filename)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(&video.id),
            combined_format.download_info.ext.as_str()
        ));
        let audio_path = self.output_dir.join(audio_filename);

        let download_result = self
            .client
            .download_format_to_path(combined_format, &temp_video_path)
            .await;

        if let Err(err) = download_result {
            return Err(err.into());
        }

        let extract_result = self.extract_audio_from_video(&temp_video_path, &audio_path).await;
        let cleanup_result = tokio::fs::remove_file(&temp_video_path).await;

        if let Err(err) = cleanup_result {
            log::warn!(
                "Failed to remove temporary fallback video {}: {}",
                temp_video_path.display(),
                err
            );
        }

        extract_result?;
        Ok(audio_path)
    }

    async fn extract_audio_from_video(
        &self,
        video_path: &Path,
        audio_path: &Path,
    ) -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new(&self.ffmpeg_path);

        cmd.arg("-i")
            .arg(video_path)
            .args(["-vn", "-c:a", "libmp3lame", "-q:a", "2"])
            .arg("-y")
            .arg(audio_path);

        let output = cmd.output().await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "ffmpeg audio extraction failed for {}: {}",
                video_path.display(),
                error
            );
        }

        Ok(())
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
