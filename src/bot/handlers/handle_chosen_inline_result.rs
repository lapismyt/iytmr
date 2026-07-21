use std::{fs, sync::Arc, time::Duration};
use tokio::sync::Mutex;

use rand::RngExt;
use teloxide::{
    payloads::{EditMessageReplyMarkupInlineSetters, SendAudioSetters},
    prelude::Requester,
    types::{
        ChatId, ChosenInlineResult, FileId, InlineKeyboardButton, InlineKeyboardButtonKind,
        InlineKeyboardMarkup, InputFile, InputMedia, InputMediaAudio,
    },
};

use crate::{
    bot::{detect_locale, types::BotWrapped},
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

fn get_keyboard(video_id: &str) -> anyhow::Result<InlineKeyboardMarkup> {
    let youtube_button = InlineKeyboardButton::new(
        "YouTube",
        InlineKeyboardButtonKind::Url(reqwest::Url::parse(
            format!("https://www.youtube.com/watch?v={}", video_id).as_str(),
        )?),
    );

    let num: u8 = {
        let mut rng = rand::rng();
        rng.random_range(0..100)
    };

    let Some(adv_name) = &*ADVERTISE_NAME else {
        return Ok(InlineKeyboardMarkup::new([[youtube_button]]));
    };

    let Some(adv_url) = &*ADVERTISE_URL else {
        return Ok(InlineKeyboardMarkup::new([[youtube_button]]));
    };

    if num > *ADVERTISE_CHANCE {
        return Ok(InlineKeyboardMarkup::new([[youtube_button]]));
    }

    Ok(InlineKeyboardMarkup::new([
        [youtube_button],
        [InlineKeyboardButton::new(
            adv_name,
            InlineKeyboardButtonKind::Url(reqwest::Url::parse(adv_url.as_str())?),
        )],
    ]))
}

pub async fn download_video(
    bot: &BotWrapped,
    downloader: Arc<Downloader>,
    video_id: &str,
    data_store: &mut DataStore,
) -> anyhow::Result<SavedVideo> {
    let max_retries = 3u8;
    let mut last_err = None;

    for attempt in 1..=max_retries {
        if attempt > 1 {
            data_store.file_id_cache.remove(video_id);
        }

        match download_video_inner(bot, downloader.clone(), video_id, data_store).await {
            Ok(video) => return Ok(video),
            Err(e) => {
                log::warn!(
                    "Download attempt {}/{} failed for {}: {}",
                    attempt,
                    max_retries,
                    video_id,
                    e
                );
                last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("All {} download attempts failed for {}", max_retries, video_id)))
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

    let mut send_audio = bot
        .send_audio(ChatId(*TRASH_CHAT_ID), InputFile::file(&download_path))
        .title(&title)
        .performer(&performer);

    if let Some(path) = &thumbnail_path {
        send_audio = send_audio.thumbnail(InputFile::file(path));
    }

    let tmp_msg = match send_audio.await {
        Ok(msg) => msg,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to get file id for audio: {}", e));
        }
    };

    let audio = match tmp_msg.audio() {
        Some(audio) => audio,
        None => {
            return Err(anyhow::anyhow!("Failed to get audio from message"));
        }
    };

    log::info!("Got file id for audio: {}", &audio.file.id);

    if let Err(e) = bot.delete_message(tmp_msg.chat.id, tmp_msg.id).await {
        log::error!("Failed to delete temp message: {:?}", e);
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
        bot.edit_message_text_inline(
            &inline_message_id,
            t!("chosen.error_decode_id", locale = locale),
        )
        .await
        .ok();

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
                bot.edit_message_text_inline(
                    &inline_message_id,
                    t!(
                        "chosen.error_rate_limit",
                        locale = locale,
                        max_downloads = (*MAX_USER_PARALLEL_DOWNLOADS).to_string()
                    ),
                )
                .await
                .ok();
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

                    bot.edit_message_text_inline(&inline_message_id, error_message)
                        .await
                        .ok();
                    return Err(err);
                }
            }
        }
    };

    let mut text = t!(
        "chosen.caption",
        locale = locale,
        performer = video.performer.as_str(),
        title = video.title.as_str()
    )
    .to_string();

    if let Ok(me) = bot.get_me().await {
        text += t!(
            "chosen.footer",
            locale = locale,
            bot_username = me.username()
        )
        .as_ref();
    }

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
        log::error!("Failed to edit message media inline: {:?}", e);
    } else {
        log::info!("Edited message media inline successfully");
    };

    if let Err(e) = bot
        .edit_message_reply_markup_inline(&inline_message_id)
        .reply_markup(get_keyboard(&video_id)?)
        .await
    {
        log::error!("Failed to edit message reply markup: {:?}", e);
    } else {
        log::info!("Edited message reply markup successfully");
    };

    db.increment_user_dl_counter(&user_id)?;
    db.increment_video_dl_counter(&video_id)?;

    log::info!("Incremented user and video download counters");

    cache_check().unwrap_or_else(|err| log::error!("Unable to check and clear cache: {:?}", err));

    Ok(())
}
