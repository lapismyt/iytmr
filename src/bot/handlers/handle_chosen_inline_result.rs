use std::{
    fs,
    sync::{Arc, Mutex},
};

use chrono::{TimeDelta, Utc};
use teloxide::{
    payloads::{EditMessageReplyMarkupInlineSetters, SendAudioSetters},
    prelude::Requester,
    types::{
        ChatId, ChosenInlineResult, FileId, InlineKeyboardButton, InlineKeyboardButtonKind,
        InlineKeyboardMarkup, InputFile, InputMedia, InputMediaAudio, Me,
    },
};

use crate::{
    bot::types::BotWrapped,
    cache::{DataStore, cache_check},
    consts::{MAX_USER_PARALLEL_DOWNLOADS, TRASH_CHAT_ID},
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

pub async fn download_video(
    bot: &BotWrapped,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
    video_id: &str,
    data_store: Arc<Mutex<DataStore>>,
) -> anyhow::Result<SavedVideo> {
    if let Some(saved_video) = db.get_saved_video(video_id) {
        log::info!("Found saved video for {}", video_id);
        return Ok(saved_video);
    }

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

    let (title, performer) = get_title_and_perfomer(&video.title, video.uploader.as_deref());

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

    let result_video = SavedVideo {
        file_id: audio.file.id.0.clone(),
        title,
        performer,
        duration: audio.duration.seconds(),
        thumbnail: thumbnail_path.unwrap_or_default(),
        expires_at: Utc::now() + TimeDelta::days(7),
        path: download_path,
        video_id: video_id.to_string(),
    };

    if let Err(e) = db.save_video(&result_video) {
        log::error!("Failed to save video: {:?}", e);
    }

    data_store.lock().unwrap().cached_files_count.increment();
    data_store
        .lock()
        .unwrap()
        .downloaded_files_count
        .increment();

    Ok(result_video)
}

pub async fn handle_chosen_inline_result(
    bot: BotWrapped,
    chosen_inline_result: ChosenInlineResult,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
    data_store: Arc<Mutex<DataStore>>,
    me: Me,
) -> anyhow::Result<()> {
    let Some(inline_message_id) = chosen_inline_result.inline_message_id.as_deref() else {
        log::error!("No inline message id for chosen inline result");
        return Ok(());
    };

    let user_id = chosen_inline_result.from.id.0;

    let Ok(video_id) = decode_temporary_id(&chosen_inline_result.result_id) else {
        log::error!(
            "Failed to decode result id: {}",
            chosen_inline_result.result_id
        );
        bot.edit_message_text_inline(inline_message_id, "Error: Failed to decode result id")
            .await
            .ok();

        return Ok(());
    };

    let video = match db.get_saved_video(&video_id) {
        Some(file_id) => file_id,
        None => {
            {
                let count = data_store
                    .lock()
                    .unwrap()
                    .active_downloads
                    .get(&user_id)
                    .map(|v| *v)
                    .unwrap_or(0);
                if count >= *MAX_USER_PARALLEL_DOWNLOADS as i32 {
                    bot.edit_message_text_inline(
                        inline_message_id,
                        format!(
                            "💥 Error: You already have {} active downloads. Please wait.",
                            *MAX_USER_PARALLEL_DOWNLOADS
                        ),
                    )
                    .await
                    .ok();
                    return Ok(());
                }
            }

            data_store
                .lock()
                .unwrap()
                .active_downloads
                .entry(user_id)
                .and_modify(|v| *v += 1)
                .or_insert(1);

            let id =
                download_video(&bot, downloader, db.clone(), &video_id, data_store.clone()).await;

            data_store
                .lock()
                .unwrap()
                .active_downloads
                .entry(user_id)
                .and_modify(|v| *v -= 1);

            match id {
                Ok(video) => video,
                Err(e) => {
                    bot.edit_message_text_inline(
                        inline_message_id,
                        format!("💥 Error: Failed to download video: {:?}", e),
                    )
                    .await
                    .ok();
                    return Err(e);
                }
            }
        }
    };

    let mut input_media_audio =
        InputMediaAudio::new(InputFile::file_id(FileId::from(video.file_id)))
            .caption(format!("✨ Downloaded with @{}", me.username()))
            .title(video.title)
            .performer(video.performer)
            .duration(video.duration as u16);

    if let Ok(exists) = fs::exists(&video.thumbnail) {
        if exists {
            input_media_audio = input_media_audio.thumbnail(InputFile::file(video.thumbnail));
        }
    }

    if let Err(e) = bot
        .edit_message_media_inline(inline_message_id, InputMedia::Audio(input_media_audio))
        .await
    {
        log::error!("Failed to edit message media inline: {:?}", e);
    } else {
        log::info!("Edited message media inline successfully");
    };

    if let Err(e) = bot
        .edit_message_reply_markup_inline(inline_message_id)
        .reply_markup(InlineKeyboardMarkup::new([[InlineKeyboardButton::new(
            "YouTube",
            InlineKeyboardButtonKind::Url(reqwest::Url::parse(
                format!("https://www.youtube.com/watch?v={}", video_id).as_str(),
            )?),
        )]]))
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
