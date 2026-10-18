use std::{fs, sync::Arc};
use tokio::sync::Mutex;

use anyhow::Context as _;
use rand::RngExt;
use teloxide::{
    payloads::{EditMessageReplyMarkupInlineSetters, SendAudioSetters},
    prelude::Requester,
    types::{
        ChatId, ChosenInlineResult, FileId, InlineKeyboardButton, InlineKeyboardButtonKind,
        InlineKeyboardMarkup, InputFile, InputMedia, InputMediaAudio,
    },
    utils::html,
};

use crate::{
    bot::{
        detect_locale,
        errors::{log_request_error, retry_network_with_backoff},
        types::BotWrapped,
        video_url,
    },
    cache::{DataStore, cache_check},
    consts::{
        ADVERTISE_CHANCE, ADVERTISE_NAME, ADVERTISE_URL, MAX_USER_PARALLEL_DOWNLOADS, TRASH_CHAT_ID,
    },
    db::{DatabaseHelper, models::SavedVideo},
    downloader::Downloader,
    parser::get_title_and_perfomer,
};

fn decode_temporary_id<S: Into<String>>(result_id: S) -> anyhow::Result<String> {
    let string = result_id.into();
    let parts: Vec<&str> = string.split(':').collect();
    if parts.len() != 3 {
        return Err(anyhow::anyhow!("Incorrect result id"));
    }

    Ok(parts[0].to_string())
}

/// Builds the reply markup shown after a track is sent.
///
/// `roll` is the precomputed advert draw in `0..100`; an advertise button is shown only
/// when it is lower than `advertise_chance`. The keyboard is empty otherwise.
fn get_keyboard(
    advertise: Option<(&str, &str)>,
    advertise_chance: u8,
    roll: u8,
) -> anyhow::Result<InlineKeyboardMarkup> {
    let Some((advertise_name, advertise_url)) = advertise else {
        return Ok(InlineKeyboardMarkup::default());
    };

    if roll >= advertise_chance {
        return Ok(InlineKeyboardMarkup::default());
    }

    Ok(InlineKeyboardMarkup::new([[InlineKeyboardButton::new(
        advertise_name,
        InlineKeyboardButtonKind::Url(url::Url::parse(advertise_url)?),
    )]]))
}

/// Builds the caption of a sent track with a link to its YouTube page.
fn get_caption(locale: &str, video_id: &str, performer: &str, title: &str) -> String {
    let escaped_performer = html::escape(performer);
    let escaped_title = html::escape(title);
    let caption = t!(
        "chosen.caption",
        locale = locale,
        performer = escaped_performer.as_str(),
        title = escaped_title.as_str()
    );
    let source_link = html::link(
        &video_url(video_id),
        t!("chosen.source", locale = locale).as_ref(),
    );

    format!("{}{}", caption, source_link)
}

pub async fn download_video(
    bot: &BotWrapped,
    downloader: Arc<Downloader>,
    video_id: &str,
    data_store: &mut DataStore,
) -> anyhow::Result<SavedVideo> {
    download_video_inner(bot, downloader, video_id, data_store).await
}

async fn download_video_inner(
    bot: &BotWrapped,
    downloader: Arc<Downloader>,
    video_id: &str,
    data_store: &mut DataStore,
) -> anyhow::Result<SavedVideo> {
    log::info!("Downloading {}...", video_id);

    let (video, download_path, thumbnail_path) = match downloader
        .download(format!("https://www.youtube.com/watch?v={}", video_id))
        .await
    {
        Ok((video, download_path, thumbnail_path)) => (video, download_path, thumbnail_path),
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to download {}: {}", video_id, e));
        }
    };

    let track_title_performer = get_title_and_perfomer(&video.title, video.uploader.as_deref());

    let (title, performer) = (track_title_performer.title, track_title_performer.performer);

    log::info!("Downloaded {}, getting file id", video_id);

    match tokio::fs::metadata(&download_path).await {
        Ok(meta) => log::info!(
            "Sending audio to trash chat: size={}MB, path={}",
            meta.len() as f64 / 1_048_576.0,
            download_path.display()
        ),
        Err(e) => log::warn!(
            "Cannot read file metadata for {}: {}",
            download_path.display(),
            e
        ),
    }

    let tmp_msg = retry_network_with_backoff("Sending audio to trash chat", || {
        let mut send_audio = bot
            .send_audio(ChatId(*TRASH_CHAT_ID), InputFile::file(&download_path))
            .title(&title)
            .performer(&performer);

        if let Some(path) = &thumbnail_path {
            send_audio = send_audio.thumbnail(InputFile::file(path));
        }

        send_audio
    })
    .await
    .context("Failed to get file ID for audio")?;

    let audio = match tmp_msg.audio() {
        Some(audio) => audio,
        None => {
            return Err(anyhow::anyhow!("Failed to get audio from message"));
        }
    };

    log::info!("Got file id for audio: {}", &audio.file.id);

    if let Err(e) = bot.delete_message(tmp_msg.chat.id, tmp_msg.id).await {
        log_request_error("Failed to delete temporary message", &e);
    }

    let result_video = crate::db::models::SavedVideo {
        file_id: audio.file.id.0.clone(),
        title,
        performer,
        duration: audio.duration.seconds(),
        thumbnail: thumbnail_path.unwrap_or_default(),
        expires_at: chrono::Utc::now() + chrono::TimeDelta::days(7),
        path: download_path,
        video_id: video_id.to_string(),
    };

    data_store.save_file_id(result_video.clone());
    data_store.cached_files_count.increment();
    data_store.downloaded_files_count.increment();

    Ok(result_video)
}

pub async fn handle_chosen_inline_result(
    bot: BotWrapped,
    chosen_inline_result: ChosenInlineResult,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
    data_store: Arc<Mutex<DataStore>>,
) -> anyhow::Result<()> {
    let Some(inline_message_id) = chosen_inline_result.inline_message_id.clone() else {
        log::error!("No inline message id for chosen inline result");
        return Ok(());
    };

    let locale = detect_locale(&chosen_inline_result.from);
    let user_id = chosen_inline_result.from.id.0;

    let Ok(video_id) = decode_temporary_id(&chosen_inline_result.result_id) else {
        log::error!(
            "Failed to decode result id: {}",
            chosen_inline_result.result_id
        );
        if let Err(error) = bot
            .edit_message_text_inline(
                &inline_message_id,
                t!("chosen.error_decode_id", locale = locale),
            )
            .await
        {
            log_request_error("Failed to report invalid inline result ID", &error);
        }

        return Ok(());
    };

    let video = {
        if let Some(cached) = data_store.lock().await.get_file_id(&video_id) {
            log::info!("Found cached file_id for {}", video_id);
            cached
        } else {
            let count = data_store
                .lock()
                .await
                .active_downloads
                .get(&user_id)
                .map(|v| *v)
                .unwrap_or(0);
            if count >= *MAX_USER_PARALLEL_DOWNLOADS as i32 {
                if let Err(error) = bot
                    .edit_message_text_inline(
                        &inline_message_id,
                        t!(
                            "chosen.error_rate_limit",
                            locale = locale,
                            max_downloads = (*MAX_USER_PARALLEL_DOWNLOADS).to_string()
                        ),
                    )
                    .await
                {
                    log_request_error("Failed to report download rate limit", &error);
                }
                return Ok(());
            }

            let id = {
                let mut data_store_guard = data_store.lock().await;

                data_store_guard
                    .active_downloads
                    .entry(user_id)
                    .and_modify(|v| *v += 1)
                    .or_insert(1);

                let id = download_video(&bot, downloader, &video_id, &mut data_store_guard).await;

                data_store_guard
                    .active_downloads
                    .entry(user_id)
                    .and_modify(|v| *v -= 1);

                id
            };

            match id {
                Ok(video) => video,
                Err(err) => {
                    let mut error_message = t!(
                        "chosen.error_download",
                        locale = locale,
                        error = format!("{:?}", err)
                    );

                    if err.to_string().contains("Sign in to confirm your age") {
                        let bot_age = chrono::Utc::now()
                            - chrono::DateTime::from_timestamp_secs(1738875600)
                                .expect("bot creation timestamp must be valid");

                        error_message = t!(
                            "chosen.error_age_restricted",
                            locale = locale,
                            bot_age = format!("{:.5}", bot_age.num_days() as f64 / 365.25)
                        );
                    }

                    if let Err(error) = bot
                        .edit_message_text_inline(&inline_message_id, error_message)
                        .await
                    {
                        log_request_error("Failed to report download error", &error);
                    }
                    return Err(err);
                }
            }
        }
    };

    let text = get_caption(locale, &video_id, &video.performer, &video.title);

    let mut input_media_audio =
        InputMediaAudio::new(InputFile::file_id(FileId::from(video.file_id)))
            .caption(text)
            .title(video.title)
            .performer(video.performer)
            .duration(video.duration as u16);

    if let Ok(is_exists) = fs::exists(&video.thumbnail)
        && is_exists
    {
        input_media_audio = input_media_audio.thumbnail(InputFile::file(video.thumbnail));
    }

    if let Err(e) = bot
        .edit_message_media_inline(&inline_message_id, InputMedia::Audio(input_media_audio))
        .await
    {
        log_request_error("Failed to edit inline message media", &e);
    } else {
        log::info!("Edited message media inline successfully");
    };

    let advertise_roll = {
        let mut rng = rand::rng();
        rng.random_range(0..100)
    };

    let advertise = ADVERTISE_NAME.as_deref().zip(ADVERTISE_URL.as_deref());

    if let Err(e) = bot
        .edit_message_reply_markup_inline(&inline_message_id)
        .reply_markup(get_keyboard(advertise, *ADVERTISE_CHANCE, advertise_roll)?)
        .await
    {
        log_request_error("Failed to edit inline message reply markup", &e);
    } else {
        log::info!("Edited message reply markup successfully");
    };

    db.increment_user_dl_counter(&user_id)?;
    db.increment_video_dl_counter(&video_id)?;

    log::info!("Incremented user and video download counters");

    cache_check().unwrap_or_else(|err| log::error!("Unable to check and clear cache: {:?}", err));

    Ok(())
}

#[cfg(test)]
mod tests {
    use teloxide::types::InlineKeyboardButtonKind;

    use super::{decode_temporary_id, get_caption, get_keyboard};

    const ADVERTISE: Option<(&str, &str)> = Some(("Ad", "https://example.com/ad"));

    #[test]
    fn decode_temporary_id_keeps_only_the_video_id() {
        assert_eq!(
            decode_temporary_id("dQw4w9WgXcQ:0.9.0:123456").expect("id must be decoded"),
            "dQw4w9WgXcQ"
        );
    }

    #[test]
    fn decode_temporary_id_rejects_malformed_ids() {
        assert!(decode_temporary_id("dQw4w9WgXcQ:0.9.0").is_err());
        assert!(decode_temporary_id("dQw4w9WgXcQ:0.9.0:123456:extra").is_err());
    }

    #[test]
    fn keyboard_is_empty_without_advertise() {
        let keyboard = get_keyboard(None, 100, 0).expect("keyboard must be built");

        assert!(keyboard.inline_keyboard.is_empty());
    }

    #[test]
    fn keyboard_is_empty_when_advertise_chance_is_rejected() {
        let keyboard = get_keyboard(ADVERTISE, 10, 10).expect("keyboard must be built");

        assert!(keyboard.inline_keyboard.is_empty());
    }

    #[test]
    fn keyboard_is_empty_when_advertise_chance_is_zero() {
        let keyboard = get_keyboard(ADVERTISE, 0, 0).expect("keyboard must be built");

        assert!(keyboard.inline_keyboard.is_empty());
    }

    #[test]
    fn keyboard_shows_only_the_advertise_button() {
        let keyboard = get_keyboard(ADVERTISE, 100, 99).expect("keyboard must be built");

        assert_eq!(keyboard.inline_keyboard.len(), 1);
        let button = &keyboard.inline_keyboard[0][0];
        assert_eq!(button.text, "Ad");
        assert_eq!(keyboard.inline_keyboard[0].len(), 1);
        match &button.kind {
            InlineKeyboardButtonKind::Url(url) => {
                assert_eq!(url.as_str(), "https://example.com/ad")
            }
            kind => panic!("advertise button must be a URL button, got {kind:?}"),
        }
    }

    #[test]
    fn caption_links_to_the_source_and_has_no_footer() {
        let caption = get_caption("en", "dQw4w9WgXcQ", "Performer", "Title");

        assert_eq!(
            caption,
            "<code>Performer — Title</code>\n\
             <a href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\">source</a>"
        );
        assert!(!caption.contains("Downloaded with"));
    }

    #[test]
    fn caption_is_translated() {
        assert!(get_caption("ru", "video-id", "P", "T").contains(">источник</a>"));
        assert!(get_caption("uk", "video-id", "P", "T").contains(">джерело</a>"));
    }

    #[test]
    fn caption_escapes_track_metadata() {
        let caption = get_caption("en", "video-id", "A&B", "<Title>");

        assert!(caption.contains("<code>A&amp;B — &lt;Title&gt;</code>"));
    }
}
