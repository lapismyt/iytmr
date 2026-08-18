use std::{
    env,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use sha2::{Digest, Sha256};
use url::Url;
use yt_dlp::{
    VideoSelection,
    client::{ProxyConfig, ProxyType},
    error::Error as YtDlpError,
    extractor::{ExtractorConfig, VideoExtractor},
    model::{playlist::Playlist, selector::ThumbnailQuality},
    prelude::*,
};

pub struct Downloader {
    client: yt_dlp::Downloader,
    ffmpeg_path: PathBuf,
    quality: yt_dlp::model::AudioQuality,
    codec: yt_dlp::model::AudioCodecPreference,
    output_dir: PathBuf,
}

const AUDIO_PREPARATION_MAX_ATTEMPTS: usize = 10;

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

        let mut builder = yt_dlp::Downloader::with_new_binaries(
            PathBuf::from(libs_dir.as_ref()),
            PathBuf::from(output_dir.as_ref()),
        )
        .await?
        .with_cache_config(cache_config);

        if let Ok(proxy_url_str) = env::var("YT_DLP_PROXY") {
            let proxy_url = Url::from_str(&proxy_url_str)?;

            match proxy_url.scheme().to_lowercase().as_str() {
                "http" => {
                    log::info!("Using HTTP proxy: {}", proxy_url_str);
                    builder = builder.with_proxy(ProxyConfig::new(ProxyType::Http, proxy_url));
                }
                "https" => {
                    log::info!("Using HTTPS proxy: {}", proxy_url_str);
                    builder = builder.with_proxy(ProxyConfig::new(ProxyType::Https, proxy_url));
                }
                "socks5" | "socks5h" => {
                    log::info!("Using SOCKS5 proxy: {}", proxy_url_str);
                    builder = builder.with_proxy(ProxyConfig::new(ProxyType::Socks5, proxy_url));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unsupported proxy scheme"));
                }
            }
        }

        let mut downloader = builder.build().await?;
        downloader.add_arg("--force-ipv4");

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
        let mut extractor =
            yt_dlp::extractor::Youtube::new(self.client.libraries().youtube.clone());
        extractor.with_arg("--force-ipv4".to_string());
        let video = extractor.fetch_video(&url.into()).await?;

        let video_id = video.id.clone();
        let video_id_hash = Downloader::sha256_hash(&video_id);

        let audio_filename = format!("{}.mp3", video_id_hash);

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

        let (audio_path, _) = match self
            .prepare_audio_with_retries(&video, &audio_filename, cropped_thumbnail_path.as_deref())
            .await
        {
            Ok(result) => result,
            Err(err) => {
                if let Some(path) = &thumbnail_path {
                    let _ = tokio::fs::remove_file(path).await;
                }
                if let Some(path) = &cropped_thumbnail_path {
                    let _ = tokio::fs::remove_file(path).await;
                }
                return Err(err);
            }
        };

        // Clean up original thumbnail file if it exists
        if let Some(path) = thumbnail_path {
            let _ = tokio::fs::remove_file(path).await;
        }

        Ok((video, audio_path, cropped_thumbnail_path))
    }

    async fn prepare_audio_with_retries(
        &self,
        video: &Video,
        audio_filename: &str,
        thumbnail_path: Option<&Path>,
    ) -> anyhow::Result<(PathBuf, String)> {
        let mut last_error = None;

        for attempt in 1..=AUDIO_PREPARATION_MAX_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(Self::retry_delay_before_attempt(attempt)).await;
                self.cleanup_audio_artifacts(audio_filename).await;
            }

            match self
                .prepare_audio(video, audio_filename, thumbnail_path)
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => {
                    log::warn!(
                        "Audio preparation attempt {}/{} failed for {}: {:#}",
                        attempt,
                        AUDIO_PREPARATION_MAX_ATTEMPTS,
                        video.id,
                        err
                    );
                    last_error = Some(err);
                }
            }
        }

        self.cleanup_audio_artifacts(audio_filename).await;
        Err(last_error.expect("audio preparation attempts must produce an error"))
    }

    fn retry_delay_before_attempt(attempt: usize) -> Duration {
        Duration::from_secs(5 * (1 << (attempt - 2)))
    }

    async fn prepare_audio(
        &self,
        video: &Video,
        audio_filename: &str,
        thumbnail_path: Option<&Path>,
    ) -> anyhow::Result<(PathBuf, String)> {
        let (audio_path, format_id) = self
            .download_audio_with_fallback(video, audio_filename)
            .await?;

        self.add_metadata_manual(&audio_path, video, thumbnail_path)
            .await?;

        if let Err(err) = self
            .verify_audio_integrity(&audio_path, video.duration.map(|duration| duration as f64))
            .await
        {
            let _ = tokio::fs::remove_file(&audio_path).await;
            self.client
                .invalidate_download_cache(&video.id, &format_id)
                .await;
            return Err(anyhow::anyhow!(
                "Audio integrity check failed for {}: {}",
                video.id,
                err
            ));
        }

        Ok((audio_path, format_id))
    }

    async fn cleanup_audio_artifacts(&self, audio_filename: &str) {
        let audio_stem = Path::new(audio_filename)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(audio_filename);
        let temp_audio_filename = format!("{audio_stem}.temp.mp3");
        let fallback_prefix = format!("{audio_stem}.fallback.");
        let mut entries = match tokio::fs::read_dir(&self.output_dir).await {
            Ok(entries) => entries,
            Err(err) => {
                log::warn!(
                    "Failed to inspect audio artifacts for {}: {}",
                    audio_filename,
                    err
                );
                return;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !Self::is_audio_artifact_filename(
                file_name,
                audio_filename,
                &temp_audio_filename,
                &fallback_prefix,
            ) {
                continue;
            }

            if let Err(err) = tokio::fs::remove_file(entry.path()).await {
                log::warn!(
                    "Failed to remove partial audio artifact {}: {}",
                    entry.path().display(),
                    err
                );
            }
        }
    }

    fn is_audio_artifact_filename(
        file_name: &str,
        audio_filename: &str,
        temp_audio_filename: &str,
        fallback_prefix: &str,
    ) -> bool {
        file_name == audio_filename
            || file_name == temp_audio_filename
            || file_name.starts_with(fallback_prefix)
    }

    async fn download_audio_with_fallback(
        &self,
        video: &Video,
        audio_filename: &str,
    ) -> anyhow::Result<(PathBuf, String)> {
        let audio_path = self.output_dir.join(audio_filename);

        // Try to select and download a standalone audio format
        if let Some(format) = video.select_audio_format(self.quality, self.codec.clone()) {
            let format_id = format.format_id.clone();
            match self
                .client
                .download_format_to_path(format, &audio_path)
                .await
            {
                Ok(path) => return Ok((path, format_id)),
                Err(YtDlpError::FormatNotAvailable { .. }) => {
                    log::warn!(
                        "Selected audio format {} unavailable for {}, falling back to muxed",
                        format_id,
                        video.id
                    );
                }
                Err(err) => return Err(err.into()),
            }
        }

        // Fallback: download muxed video and extract audio
        log::warn!(
            "No standalone audio format for {}, falling back to muxed video download",
            video.id
        );
        let combined_format = video.best_audio_video_format()?;
        let format_id = combined_format.format_id.clone();
        let path = self
            .download_muxed_video_and_extract_audio(video, audio_filename, combined_format)
            .await?;
        Ok((path, format_id))
    }

    async fn download_muxed_video_and_extract_audio(
        &self,
        video: &Video,
        audio_filename: &str,
        combined_format: &yt_dlp::model::format::Format,
    ) -> anyhow::Result<PathBuf> {
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

        let extract_result = self
            .extract_audio_from_video(&temp_video_path, &audio_path)
            .await;
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

    async fn get_audio_duration(&self, audio_path: &Path) -> anyhow::Result<f64> {
        let output = tokio::process::Command::new(&self.ffmpeg_path)
            .arg("-i")
            .arg(audio_path)
            .output()
            .await?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines() {
            if let Some(rest) = line.trim().strip_prefix("Duration:") {
                let time_str = rest.split(',').next().unwrap_or(rest).trim();
                let parts: Vec<&str> = time_str.split(':').collect();
                if parts.len() == 3 {
                    let h: f64 = parts[0].parse()?;
                    let m: f64 = parts[1].parse()?;
                    let s: f64 = parts[2].parse()?;
                    return Ok(h * 3600.0 + m * 60.0 + s);
                }
            }
        }

        anyhow::bail!(
            "Could not parse duration from ffmpeg output for {}",
            audio_path.display()
        )
    }

    async fn verify_audio_integrity(
        &self,
        audio_path: &Path,
        expected_duration: Option<f64>,
    ) -> anyhow::Result<()> {
        let metadata = tokio::fs::metadata(audio_path).await?;
        if metadata.len() == 0 {
            anyhow::bail!("Audio file is empty: {}", audio_path.display());
        }

        if let Some(expected) = expected_duration {
            let actual = self.get_audio_duration(audio_path).await?;
            if (actual - expected).abs() > 1.0 {
                let _ = tokio::fs::remove_file(audio_path).await;
                anyhow::bail!(
                    "Audio duration mismatch: expected {:.1}s, got {:.1}s",
                    expected,
                    actual
                );
            }
        }

        Ok(())
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
        let mut extractor =
            yt_dlp::extractor::Youtube::new(self.client.libraries().youtube.clone());
        extractor.with_arg("--force-ipv4".to_string());
        Ok(extractor.search(query, max_results).await?)
    }

    pub async fn fetch_video_info(&self, url: &str) -> anyhow::Result<Video> {
        let mut extractor =
            yt_dlp::extractor::Youtube::new(self.client.libraries().youtube.clone());
        extractor.with_arg("--force-ipv4".to_string());
        Ok(extractor.fetch_video(url).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::Downloader;
    use std::time::Duration;

    #[test]
    fn retry_backoff_is_exponential() {
        assert_eq!(
            Downloader::retry_delay_before_attempt(2),
            Duration::from_secs(5)
        );
        assert_eq!(
            Downloader::retry_delay_before_attempt(3),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn cleanup_targets_only_audio_artifacts_for_the_video() {
        let audio_filename = "video-hash.mp3";
        let temp_audio_filename = "video-hash.temp.mp3";
        let fallback_prefix = "video-hash.fallback.";

        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.mp3",
            audio_filename,
            temp_audio_filename,
            fallback_prefix,
        ));
        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.temp.mp3",
            audio_filename,
            temp_audio_filename,
            fallback_prefix,
        ));
        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.fallback.webm",
            audio_filename,
            temp_audio_filename,
            fallback_prefix,
        ));
        assert!(!Downloader::is_audio_artifact_filename(
            "other-hash.mp3",
            audio_filename,
            temp_audio_filename,
            fallback_prefix,
        ));
    }
}
