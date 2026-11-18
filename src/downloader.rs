use std::{
    env,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::Context as _;
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

/// Target integrated loudness for every track. Matching this across tracks keeps a
/// whole playlist at a comparable average level without touching intra-track dynamics.
const TARGET_LOUDNESS_LUFS: f64 = -14.0;
/// True-peak ceiling. A little below 0 dBTP leaves room for the mp3 encoder's overshoot.
const TARGET_TRUE_PEAK_DBTP: f64 = -1.5;
/// Never boost a quiet track more than this. Larger positive gains mostly raise the
/// noise floor, so keeping quality takes priority over matching the average perfectly.
const MAX_BOOST_DB: f64 = 12.0;
/// Gains below this are inaudible and only cost a needless filter pass.
const AUDIBLE_GAIN_DB: f64 = 0.1;

/// How a video's duration breaks the configured `MIN_DURATION`/`MAX_DURATION` bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationViolationKind {
    TooShort,
    TooLong,
}

/// Error returned by [`Downloader::download`] when the video does not fit the duration
/// bounds. Handlers downcast it to show a localized message instead of a generic error.
#[derive(Debug)]
pub struct DurationViolation {
    pub kind: DurationViolationKind,
    pub duration: f64,
}

impl std::fmt::Display for DurationViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            DurationViolationKind::TooShort => {
                write!(f, "video is too short: {:.0}s", self.duration)
            }
            DurationViolationKind::TooLong => {
                write!(f, "video is too long: {:.0}s", self.duration)
            }
        }
    }
}

impl std::error::Error for DurationViolation {}

/// Returns how `duration` breaks the bounds, if it does.
///
/// Missing bounds mean "no limit", and an unknown duration (`None`) is never rejected.
/// Bounds are inclusive: a video exactly at `min`/`max` is allowed.
pub fn check_duration(
    duration: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
) -> Option<DurationViolationKind> {
    let duration = duration?;

    if max.is_some_and(|max| duration > max) {
        return Some(DurationViolationKind::TooLong);
    }

    if min.is_some_and(|min| duration < min) {
        return Some(DurationViolationKind::TooShort);
    }

    None
}

/// Computes a single constant gain that brings a track's integrated loudness to
/// [`TARGET_LOUDNESS_LUFS`] while preserving its internal dynamics.
///
/// The gain is transparent: it is applied uniformly to the whole track, so quiet and
/// loud passages keep their original relationship. Positive gain is capped by the
/// true-peak headroom and by [`MAX_BOOST_DB`] to avoid clipping and pumping up noise;
/// attenuation is always applied in full. Returns `None` when loudness is unknown
/// (e.g. silence reported as `-inf`), in which case the track is left untouched.
fn compute_normalization_gain(input_i: f64, input_tp: f64) -> Option<f64> {
    if !input_i.is_finite() {
        return None;
    }

    let mut gain = TARGET_LOUDNESS_LUFS - input_i;

    if gain > 0.0 {
        // `input_tp == -inf` (or NaN) yields an infinite/ignored headroom, so only the
        // boost cap applies; `f64::min` already ignores NaN.
        gain = gain.min(TARGET_TRUE_PEAK_DBTP - input_tp).min(MAX_BOOST_DB);
    }

    if !gain.is_finite() {
        return None;
    }

    let gain = (gain * 10.0).round() / 10.0;
    Some(if gain.abs() < AUDIBLE_GAIN_DB {
        0.0
    } else {
        gain
    })
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

        let mut builder = yt_dlp::Downloader::with_new_binaries(
            PathBuf::from(libs_dir.as_ref()),
            PathBuf::from(output_dir.as_ref()),
        )
        .await?
        .with_cache_config(cache_config);

        // YouTube signs the streaming URLs with the requesting IP, so every request
        // (both the yt-dlp binary and the library's own HTTP client) has to leave
        // from the same address. The binary gets `--source-address` on the extractors
        // below, the native downloader is bound here.
        if let Some(ip) = *crate::consts::SEND_THROUGH {
            log::info!("Binding native downloads to source address: {}", ip);
            builder = builder.with_source_address(ip);
        }

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

        // NOTE: yt-dlp CLI arguments (`--source-address`, `--force-ipv4`) are passed to
        // the extractors below — the built-in downloader never sees `Downloader::args`,
        // it is bound through `DownloaderBuilder::with_source_address` instead.
        let downloader = builder.build().await?;

        Ok(Self {
            client: downloader,
            ffmpeg_path,
            quality: yt_dlp::model::AudioQuality::Best,
            codec: yt_dlp::model::AudioCodecPreference::MP3,
            output_dir: PathBuf::from(output_dir.as_ref()),
        })
    }

    fn sanitize_title(title: &str) -> String {
        title
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
            .map(|c| if c == ' ' { '_' } else { c })
            .collect()
    }

    pub async fn download<U: Into<String>>(
        &self,
        url: U,
    ) -> anyhow::Result<(Video, PathBuf, Option<PathBuf>)> {
        let mut extractor =
            yt_dlp::extractor::Youtube::new(self.client.libraries().youtube.clone());
        extractor.with_arg("--force-ipv4".to_string());
        if let Some(ip) = crate::consts::SEND_THROUGH.as_ref() {
            extractor.with_arg(format!("--source-address={}", ip));
        }
        let video = extractor.fetch_video(&url.into()).await?;

        let duration = video.duration.map(|duration| duration as f64);
        if let Some(kind) = check_duration(
            duration,
            *crate::consts::MIN_DURATION,
            *crate::consts::MAX_DURATION,
        ) {
            log::warn!(
                "Refusing to download {}: duration {:.0}s violates the configured bounds",
                video.id,
                duration.unwrap_or_default()
            );
            return Err(DurationViolation {
                kind,
                duration: duration.unwrap_or_default(),
            }
            .into());
        }

        let video_id = video.id.clone();
        let sanitized_title = Downloader::sanitize_title(&video.title);
        let audio_filename = format!("{}_{}.mp3", sanitized_title, video_id);

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

        let audio_path = match self
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
    ) -> anyhow::Result<PathBuf> {
        let mut last_error = None;

        for attempt in 1..=AUDIO_PREPARATION_MAX_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(Self::retry_delay_before_attempt(attempt)).await;
                Self::cleanup_audio_artifacts(&self.output_dir, audio_filename).await;
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

        Self::cleanup_audio_artifacts(&self.output_dir, audio_filename).await;
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
    ) -> anyhow::Result<PathBuf> {
        let mut attempted_format_id = None;
        let (audio_path, format_id) = match self
            .download_audio_with_fallback(video, audio_filename, &mut attempted_format_id)
            .await
        {
            Ok(result) => result,
            Err(err) => {
                // A failed download may have left a stale cache entry pointing at a file
                // that no longer exists; drop it so the retry downloads from scratch.
                self.drop_cached_download(video, attempted_format_id).await;
                return Err(err);
            }
        };

        if let Err(err) = Self::ensure_non_empty_audio(&audio_path).await {
            // The downloaded file is unusable, so its cache entry must not be reused.
            self.drop_cached_download(video, Some(format_id)).await;
            return Err(err);
        }

        let gain_db = self.measure_normalization_gain(&audio_path).await?;

        self.add_metadata_manual(&audio_path, video, thumbnail_path, gain_db)
            .await?;

        if let Err(err) = self
            .verify_audio_integrity(&audio_path, video.duration.map(|duration| duration as f64))
            .await
        {
            let _ = tokio::fs::remove_file(&audio_path).await;
            self.drop_cached_download(video, Some(format_id)).await;
            return Err(anyhow::anyhow!(
                "Audio integrity check failed for {}: {}",
                video.id,
                err
            ));
        }

        Ok(audio_path)
    }

    /// Removes the yt-dlp download cache entry of an attempt that must not be reused.
    ///
    /// Retries delete the output file before preparing the audio again; without dropping
    /// the cache entry, a cache hit would point at that missing (or already corrupt) file
    /// and fail with a missing-file IO error instead of downloading the audio again.
    async fn drop_cached_download(&self, video: &Video, format_id: Option<String>) {
        let Some(format_id) = format_id else {
            return;
        };

        log::debug!(
            "Dropping cached download of format {} for {}",
            format_id,
            video.id
        );

        self.client
            .invalidate_download_cache(&video.id, &format_id)
            .await;
    }

    async fn ensure_non_empty_audio(audio_path: &Path) -> anyhow::Result<()> {
        let metadata = tokio::fs::metadata(audio_path).await.with_context(|| {
            format!(
                "Failed to inspect downloaded audio {}",
                audio_path.display()
            )
        })?;

        if metadata.len() == 0 {
            anyhow::bail!(
                "Audio file is empty after download: {}",
                audio_path.display()
            );
        }

        Ok(())
    }

    async fn cleanup_audio_artifacts(output_dir: &Path, audio_filename: &str) {
        let audio_stem = Path::new(audio_filename)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(audio_filename);
        let artifact_names = [
            audio_filename.to_string(),
            format!("{audio_stem}.temp.mp3"),
            format!("{audio_stem}.norm.mp3"),
        ];
        let fallback_prefix = format!("{audio_stem}.fallback.");
        let mut entries = match tokio::fs::read_dir(output_dir).await {
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
            if !Self::is_audio_artifact_filename(file_name, &artifact_names, &fallback_prefix) {
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
        artifact_names: &[String],
        fallback_prefix: &str,
    ) -> bool {
        artifact_names
            .iter()
            .any(|artifact_name| artifact_name == file_name)
            || file_name.starts_with(fallback_prefix)
    }

    async fn download_audio_with_fallback(
        &self,
        video: &Video,
        audio_filename: &str,
        format_id_out: &mut Option<String>,
    ) -> anyhow::Result<(PathBuf, String)> {
        let audio_path = self.output_dir.join(audio_filename);

        // Try to select and download a standalone audio format
        if let Some(format) = video.select_audio_format(self.quality, self.codec.clone()) {
            let format_id = format.format_id.clone();
            *format_id_out = Some(format_id.clone());
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
        *format_id_out = Some(format_id.clone());
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

    /// Measures the track's loudness and true peak and returns the constant gain that
    /// should be applied to the whole file.
    ///
    /// Analysis is lossless (`-f null`), so it does not add another encoding pass. Any
    /// failure to measure is non-fatal: the track is returned without normalization
    /// rather than failing the whole download.
    async fn measure_normalization_gain(&self, audio_path: &Path) -> anyhow::Result<Option<f64>> {
        let analyze_output = tokio::process::Command::new(&self.ffmpeg_path)
            .arg("-i")
            .arg(audio_path)
            .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
            .output()
            .await?;

        if !analyze_output.status.success() {
            log::warn!(
                "Loudness analysis failed for {}, leaving track unnormalized: {}",
                audio_path.display(),
                String::from_utf8_lossy(&analyze_output.stderr)
                    .chars()
                    .take(500)
                    .collect::<String>()
            );
            return Ok(None);
        }

        let analyze_stderr = String::from_utf8_lossy(&analyze_output.stderr);
        let (Some(json_start), Some(json_end)) =
            (analyze_stderr.find('{'), analyze_stderr.rfind('}'))
        else {
            log::warn!(
                "Could not locate loudnorm JSON for {}, leaving track unnormalized: {}",
                audio_path.display(),
                analyze_stderr.chars().take(500).collect::<String>()
            );
            return Ok(None);
        };
        let json_str = &analyze_stderr[json_start..=json_end];

        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) else {
            log::warn!(
                "Could not parse loudnorm JSON for {}, leaving track unnormalized",
                audio_path.display()
            );
            return Ok(None);
        };

        let number = |key: &str| {
            parsed
                .get(key)
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<f64>().ok())
        };
        let Some(input_i) = number("input_i") else {
            log::warn!(
                "Loudnorm output for {} has no usable input_i, leaving track unnormalized",
                audio_path.display()
            );
            return Ok(None);
        };
        // A missing/unusable true peak only removes the peak cap; an actual `-inf`
        // (silence) parses as negative infinity and leaves unlimited headroom.
        let input_tp = number("input_tp").unwrap_or(f64::NEG_INFINITY);

        let gain = compute_normalization_gain(input_i, input_tp);
        log::info!(
            "Loudness analysis for {}: I={:.1} LUFS, TP={:.1} dBTP, gain={} dB",
            audio_path.display(),
            input_i,
            input_tp,
            gain.map_or_else(|| "none".to_string(), |gain| format!("{gain:+.1}")),
        );

        Ok(gain)
    }

    async fn add_metadata_manual(
        &self,
        audio_path: &Path,
        video: &Video,
        thumbnail_path: Option<&Path>,
        gain_db: Option<f64>,
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

        // Apply one constant gain to the whole track. Unlike `loudnorm`, this never
        // compresses loud/quiet passages against each other, so the author's dynamics
        // survive intact.
        if let Some(gain_db) = gain_db.filter(|gain| *gain != 0.0) {
            cmd.arg("-af").arg(format!("volume={gain_db:.1}dB"));
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
        if let Some(ip) = crate::consts::SEND_THROUGH.as_ref() {
            extractor.with_arg(format!("--source-address={}", ip));
        }
        Ok(extractor.search(query, max_results).await?)
    }

    pub async fn fetch_video_info(&self, url: &str) -> anyhow::Result<Video> {
        let mut extractor =
            yt_dlp::extractor::Youtube::new(self.client.libraries().youtube.clone());
        extractor.with_arg("--force-ipv4".to_string());
        if let Some(ip) = crate::consts::SEND_THROUGH.as_ref() {
            extractor.with_arg(format!("--source-address={}", ip));
        }
        Ok(extractor.fetch_video(url).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{Downloader, DurationViolationKind, check_duration, compute_normalization_gain};

    #[test]
    fn duration_within_bounds_is_allowed() {
        assert_eq!(check_duration(Some(300.0), Some(1.0), Some(600.0)), None);
    }

    #[test]
    fn duration_bounds_are_inclusive() {
        assert_eq!(check_duration(Some(600.0), None, Some(600.0)), None);
        assert_eq!(check_duration(Some(1.0), Some(1.0), None), None);
    }

    #[test]
    fn duration_above_max_is_too_long() {
        assert_eq!(
            check_duration(Some(601.0), None, Some(600.0)),
            Some(DurationViolationKind::TooLong)
        );
    }

    #[test]
    fn duration_below_min_is_too_short() {
        assert_eq!(
            check_duration(Some(0.5), Some(1.0), None),
            Some(DurationViolationKind::TooShort)
        );
    }

    #[test]
    fn unknown_duration_or_missing_bounds_are_never_rejected() {
        assert_eq!(check_duration(None, Some(1.0), Some(600.0)), None);
        assert_eq!(check_duration(Some(9999.0), None, None), None);
    }

    #[test]
    fn conflicting_bounds_prefer_too_long() {
        assert_eq!(
            check_duration(Some(100.0), Some(200.0), Some(50.0)),
            Some(DurationViolationKind::TooLong)
        );
    }

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
        let artifact_names = [
            "video-hash.mp3".to_string(),
            "video-hash.temp.mp3".to_string(),
            "video-hash.norm.mp3".to_string(),
        ];
        let fallback_prefix = "video-hash.fallback.";

        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.mp3",
            &artifact_names,
            fallback_prefix,
        ));
        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.temp.mp3",
            &artifact_names,
            fallback_prefix,
        ));
        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.norm.mp3",
            &artifact_names,
            fallback_prefix,
        ));
        assert!(Downloader::is_audio_artifact_filename(
            "video-hash.fallback.webm",
            &artifact_names,
            fallback_prefix,
        ));
        assert!(!Downloader::is_audio_artifact_filename(
            "other-hash.mp3",
            &artifact_names,
            fallback_prefix,
        ));
    }

    #[tokio::test]
    async fn cleanup_removes_stale_audio_artifacts() {
        let output_dir = TempDir::new().expect("temporary directory must be created");
        let artifacts = [
            "video-hash.mp3",
            "video-hash.temp.mp3",
            "video-hash.norm.mp3",
            "video-hash.fallback.webm",
        ];
        for artifact in artifacts {
            tokio::fs::write(output_dir.path().join(artifact), b"data")
                .await
                .expect("artifact must be written");
        }
        let untouched_path = output_dir.path().join("other-hash.mp3");
        tokio::fs::write(&untouched_path, b"data")
            .await
            .expect("artifact must be written");

        Downloader::cleanup_audio_artifacts(output_dir.path(), "video-hash.mp3").await;

        for artifact in artifacts {
            assert!(
                !output_dir.path().join(artifact).exists(),
                "{} must be removed",
                artifact
            );
        }
        assert!(untouched_path.exists());
    }

    #[tokio::test]
    async fn ensure_non_empty_audio_rejects_missing_file() {
        let output_dir = TempDir::new().expect("temporary directory must be created");

        let err = Downloader::ensure_non_empty_audio(&output_dir.path().join("missing.mp3"))
            .await
            .expect_err("missing file must be rejected");

        assert!(
            err.to_string()
                .contains("Failed to inspect downloaded audio")
        );
    }

    #[tokio::test]
    async fn ensure_non_empty_audio_rejects_empty_download() {
        let output_dir = TempDir::new().expect("temporary directory must be created");
        let audio_path = output_dir.path().join("empty.mp3");
        tokio::fs::write(&audio_path, b"")
            .await
            .expect("empty file must be written");

        let err = Downloader::ensure_non_empty_audio(&audio_path)
            .await
            .expect_err("empty file must be rejected");

        assert!(
            err.to_string()
                .contains("Audio file is empty after download")
        );
    }

    #[tokio::test]
    async fn ensure_non_empty_audio_accepts_downloaded_file() {
        let output_dir = TempDir::new().expect("temporary directory must be created");
        let audio_path = output_dir.path().join("audio.mp3");
        tokio::fs::write(&audio_path, b"audio")
            .await
            .expect("audio file must be written");

        Downloader::ensure_non_empty_audio(&audio_path)
            .await
            .expect("downloaded audio must be accepted");
    }

    #[test]
    fn loud_track_is_attenuated_by_full_difference() {
        assert_eq!(compute_normalization_gain(-8.0, -0.1), Some(-6.0));
    }

    #[test]
    fn attenuation_is_never_capped_by_peaks() {
        assert_eq!(compute_normalization_gain(-6.0, 0.5), Some(-8.0));
    }

    #[test]
    fn quiet_track_with_headroom_is_capped_at_max_boost() {
        // 16 dB needed, peak headroom allows 18.5 dB, but the boost cap is 12 dB.
        assert_eq!(compute_normalization_gain(-30.0, -20.0), Some(12.0));
    }

    #[test]
    fn quiet_track_with_high_peaks_is_capped_by_true_peak_headroom() {
        // 10 dB needed, but only 1.5 dB of headroom below -1.5 dBTP.
        assert_eq!(compute_normalization_gain(-24.0, -3.0), Some(1.5));
    }

    #[test]
    fn track_already_at_target_needs_no_gain() {
        assert_eq!(compute_normalization_gain(-14.0, -2.0), Some(0.0));
        assert_eq!(compute_normalization_gain(-14.04, -2.0), Some(0.0));
    }

    #[test]
    fn silence_is_left_untouched() {
        assert_eq!(
            compute_normalization_gain(f64::NEG_INFINITY, f64::NEG_INFINITY),
            None
        );
    }

    #[test]
    fn non_finite_loudness_is_left_untouched() {
        assert_eq!(compute_normalization_gain(f64::NAN, -2.0), None);
    }

    #[test]
    fn missing_true_peak_only_removes_the_peak_cap() {
        assert_eq!(
            compute_normalization_gain(-40.0, f64::NEG_INFINITY),
            Some(12.0)
        );
    }
}
