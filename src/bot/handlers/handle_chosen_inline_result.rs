use std::sync::Arc;

use teloxide::{
    payloads::{EditMessageReplyMarkupInlineSetters, SendAudioSetters},
    prelude::Requester,
    types::{
        ChatId, ChosenInlineResult, InlineKeyboardButton, InlineKeyboardButtonKind,
        InlineKeyboardMarkup, InputFile, InputMedia, InputMediaAudio, Me,
    },
};

use crate::{
    bot::types::{BotWrapped, DataStore},
    cache::cache_check,
    consts::{MAX_USER_PARALLEL_DOWNLOADS, TRASH_CHAT_ID},
    db::DatabaseHelper,
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

pub async fn handle_chosen_inline_result(
    bot: BotWrapped,
    chosen_inline_result: ChosenInlineResult,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
    data_store: Arc<DataStore>,
    me: Me,
) -> anyhow::Result<()> {
    let Some(inline_message_id) = chosen_inline_result.inline_message_id.as_deref() else {
        log::error!("No inline message id for chosen inline result");
        return Ok(());
    };

    let user_id = chosen_inline_result.from.id.0;

    {
        let count = data_store
            .active_downloads
            .get(&user_id)
            .map(|v| *v)
            .unwrap_or(0);
        if count >= *MAX_USER_PARALLEL_DOWNLOADS as i32 {
            bot.edit_message_text_inline(
                inline_message_id,
                format!(
                    "Error: You already have {} active downloads. Please wait.",
                    *MAX_USER_PARALLEL_DOWNLOADS
                ),
            )
            .await
            .ok();
            return Ok(());
        }
    }

    data_store
        .active_downloads
        .entry(user_id)
        .and_modify(|v| *v += 1)
        .or_insert(1);

    let Ok(video_id) = decode_temporary_id(&chosen_inline_result.result_id) else {
        log::error!(
            "Failed to decode result id: {}",
            chosen_inline_result.result_id
        );
        bot.edit_message_text_inline(inline_message_id, "Error: Failed to decode result id")
            .await
            .ok();

        data_store
            .active_downloads
            .entry(user_id)
            .and_modify(|v| *v -= 1);
        return Ok(());
    };

    log::info!("Downloading {}...", video_id);

    let Ok((video, download_path, thumbnail_path)) = downloader.download(&video_id).await else {
        log::error!("Failed to download {}", video_id);
        bot.edit_message_text_inline(inline_message_id, "Error: Failed to download audio")
            .await
            .ok();
        data_store
            .active_downloads
            .entry(user_id)
            .and_modify(|v| *v -= 1);
        return Ok(());
    };

    let (title, performer) = get_title_and_perfomer(&video.title, video.uploader.as_deref());

    log::info!("Downloaded {}, getting file id", video_id);

    let mut send_audio = bot
        .send_audio(ChatId(*TRASH_CHAT_ID), InputFile::file(&download_path))
        .title(title)
        .performer(performer);

    if let Some(path) = &thumbnail_path {
        send_audio = send_audio.thumbnail(InputFile::file(path));
    }

    let tmp_msg = match send_audio.await {
        Ok(msg) => msg,
        Err(e) => {
            log::error!("Failed to get file id for audio: {}", e);
            data_store
                .active_downloads
                .entry(user_id)
                .and_modify(|v| *v -= 1);
            return Ok(());
        }
    };

    let audio = match tmp_msg.audio() {
        Some(audio) => audio,
        None => {
            log::error!("Failed to get audio from message");
            data_store
                .active_downloads
                .entry(user_id)
                .and_modify(|v| *v -= 1);
            return Ok(());
        }
    };

    log::info!("Got file id for audio: {}", &audio.file.id);

    data_store
        .active_downloads
        .entry(user_id)
        .and_modify(|v| *v -= 1);

    let mut input_media_audio = InputMediaAudio::new(InputFile::file_id(audio.file.id.clone()))
        .caption(format!("Downloaded with @{}", me.username()))
        .title(video.title)
        .performer(video.uploader.unwrap_or("???".to_string()))
        .duration(video.duration.unwrap_or(1) as u16);

    if let Some(path) = &thumbnail_path {
        input_media_audio = input_media_audio.thumbnail(InputFile::file(path));
    } else {
        input_media_audio = input_media_audio.thumbnail(InputFile::url(url::Url::parse(
            format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", video_id).as_str(),
        )?));
    }

    if let Err(e) = bot
        .edit_message_media_inline(inline_message_id, InputMedia::Audio(input_media_audio))
        .await
    {
        log::error!("Failed to edit message media inline: {:?}", e);
    } else {
        log::info!("Edited message media inline successfully");
    };

    // Clean up thumbnail
    if let Some(path) = thumbnail_path {
        let _ = tokio::fs::remove_file(path).await;
    }

    if let Err(e) = bot
        .edit_message_reply_markup_inline(inline_message_id)
        .reply_markup(InlineKeyboardMarkup::new([[InlineKeyboardButton::new(
            "YouTube",
            InlineKeyboardButtonKind::Url(reqwest::Url::parse(
                video
                    .webpage_url
                    .unwrap_or(format!("https://www.youtube.com/watch?v={}", video_id))
                    .as_str(),
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

    if let Err(e) = bot.delete_message(tmp_msg.chat.id, tmp_msg.id).await {
        log::error!("Failed to delete temp message: {:?}", e);
    }

    cache_check().unwrap_or_else(|err| log::error!("Unable to check and clear cache: {:?}", err));

    Ok(())
}
