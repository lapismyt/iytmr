use std::{str::FromStr, sync::Arc};

use chrono::Utc;
use teloxide::{
    prelude::Requester,
    types::{
        InlineKeyboardButton, InlineKeyboardButtonKind, InlineKeyboardMarkup, InlineQuery,
        InlineQueryResult, InlineQueryResultArticle, InputMessageContent, InputMessageContentText,
        LinkPreviewOptions,
    },
};
use yt_dlp::model::playlist::PlaylistEntry;

use crate::{
    bot::types::BotWrapped,
    consts::{BLANK_PLACEHOLDER, MAX_DURATION, MAX_RESULTS, MIN_DURATION, NO_RESULTS_ID, VERSION},
    downloader::Downloader,
};

fn get_temporary_id(hashable: &str) -> String {
    format!(
        "{}:{}:{}",
        hashable,
        VERSION,
        Utc::now().timestamp() / 60 / 60
    )
}

fn playlist_entry_to_inline_query_result_article(
    bot: &BotWrapped,
    vid: &PlaylistEntry,
) -> InlineQueryResultArticle {
    let thumbnail_url = format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", vid.id);

    let mut article = InlineQueryResultArticle::new(
        get_temporary_id(&vid.id),
        match vid.title.is_empty() {
            true => BLANK_PLACEHOLDER.to_string(),
            false => vid.title.clone(),
        },
        InputMessageContent::Text(InputMessageContentText {
            message_text: format!(
                "<b>Downloading \"{} — {}\"...</b>",
                vid.title,
                vid.uploader.clone().unwrap_or("None".to_string())
            ),
            parse_mode: Some(bot.parse_mode()),
            entities: None,
            link_preview_options: Some(LinkPreviewOptions {
                is_disabled: false,
                url: Some(thumbnail_url.clone()),
                prefer_small_media: false,
                prefer_large_media: true,
                show_above_text: true,
            }),
        }),
    )
    .reply_markup(InlineKeyboardMarkup::new([[InlineKeyboardButton::new(
        "Loading...",
        InlineKeyboardButtonKind::CallbackData("loading".to_string()),
    )]]));

    if let Some(uploader) = &vid.uploader {
        article = article.description(uploader);
    }

    if let Ok(thumbnail_url) = reqwest::Url::from_str(&thumbnail_url) {
        article = article.thumbnail_url(thumbnail_url);
    }

    if let Ok(video_url) = reqwest::Url::from_str(&vid.url) {
        article = article.url(video_url);
    }

    article
}

pub async fn handle_inline_query(
    bot: BotWrapped,
    inline_query: InlineQuery,
    downloader: Arc<Downloader>,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();

    let query = inline_query.query;
    if query.is_empty() {
        return Ok(());
    }

    let Ok(results) = downloader.search(&query, *MAX_RESULTS).await else {
        log::error!("Failed to search for inline query: {}", query);
        return Ok(());
    };

    let playlist = results.filter_by_duration(*MIN_DURATION, *MAX_DURATION);
    let mut results: Vec<InlineQueryResult> = playlist
        .into_iter()
        .map(|vid| {
            InlineQueryResult::Article(playlist_entry_to_inline_query_result_article(&bot, vid))
        })
        .collect();

    if results.is_empty() {
        results.push(InlineQueryResult::Article(InlineQueryResultArticle {
            id: NO_RESULTS_ID.to_string(),
            title: "No results".to_string(),
            input_message_content: InputMessageContent::Text(InputMessageContentText {
                message_text: "No results found for your query.".to_string(),
                parse_mode: Some(bot.parse_mode()),
                entities: None,
                link_preview_options: None,
            }),
            reply_markup: None,
            url: None,
            description: None,
            thumbnail_url: None,
            thumbnail_width: None,
            thumbnail_height: None,
        }));
    }

    if let Err(e) = bot.answer_inline_query(inline_query.id, results).await {
        log::error!("Failed to answer inline query: {}", e);
    };

    let elapsed = start_time.elapsed();
    log::debug!("Inline query handled in {:?}", elapsed);

    Ok(())
}
