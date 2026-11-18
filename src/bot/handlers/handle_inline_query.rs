use std::{str::FromStr, sync::Arc};

use chrono::Utc;
use teloxide::{
    payloads::AnswerInlineQuerySetters,
    prelude::Requester,
    types::{
        InlineKeyboardButton, InlineKeyboardButtonKind, InlineKeyboardMarkup, InlineQuery,
        InlineQueryResult, InlineQueryResultArticle, InputMessageContent, InputMessageContentText,
        LinkPreviewOptions, Me,
    },
    utils::html,
};
use yt_dlp::model::playlist::PlaylistEntry;

use crate::{
    bot::{
        detect_locale, errors::log_request_error, format_duration, types::BotWrapped, video_url,
    },
    consts::{
        BLANK_PLACEHOLDER, INLINE_CACHE_TIME, MAX_DURATION, MAX_RESULTS, MIN_DURATION,
        NO_RESULTS_ID, TOO_LONG_ID, TOO_SHORT_ID, VERSION,
    },
    downloader::{Downloader, DurationViolationKind, check_duration},
    parser::get_title_and_perfomer,
};

fn get_temporary_id(hashable: &str) -> String {
    format!(
        "{}:{}:{}",
        hashable,
        VERSION,
        Utc::now().timestamp() / 60 / 60
    )
}

fn is_totally_invisible(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_whitespace() || c.is_control() || ('\u{200B}'..'\u{200D}').contains(&c))
}

fn is_youtube_url(query: &str) -> bool {
    let q = query.trim();
    q.starts_with("https://www.youtube.com/")
        || q.starts_with("http://www.youtube.com/")
        || q.starts_with("https://youtube.com/")
        || q.starts_with("http://youtube.com/")
        || q.starts_with("https://youtu.be/")
        || q.starts_with("http://youtu.be/")
        || q.starts_with("https://m.youtube.com/")
        || q.starts_with("http://m.youtube.com/")
}

/// Builds the placeholder shown while the track is downloading, with the track name
/// linked to its YouTube page.
fn downloading_message_text(locale: &str, video_id: &str, performer: &str, title: &str) -> String {
    let track = html::link(&video_url(video_id), &format!("{} — {}", performer, title));

    t!("inline.downloading", locale = locale, track = track).to_string()
}

/// Builds an article that only inserts an explanatory text and can never be downloaded.
fn notice_article(bot: &BotWrapped, id: String, title: String, text: String) -> InlineQueryResult {
    InlineQueryResult::Article(InlineQueryResultArticle {
        id,
        title,
        input_message_content: InputMessageContent::Text(InputMessageContentText {
            message_text: text,
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
    })
}

/// Builds the "Too short"/"Too long" article for a video that breaks the duration bounds.
fn duration_notice(
    bot: &BotWrapped,
    locale: &str,
    kind: DurationViolationKind,
) -> InlineQueryResult {
    let (id, title, text) = match kind {
        DurationViolationKind::TooShort => (
            (*TOO_SHORT_ID).clone(),
            t!("inline.too_short_title", locale = locale),
            t!(
                "inline.too_short_text",
                locale = locale,
                min_duration = format_duration((*MIN_DURATION).unwrap_or_default())
            ),
        ),
        DurationViolationKind::TooLong => (
            (*TOO_LONG_ID).clone(),
            t!("inline.too_long_title", locale = locale),
            t!(
                "inline.too_long_text",
                locale = locale,
                max_duration = format_duration((*MAX_DURATION).unwrap_or_default())
            ),
        ),
    };

    notice_article(bot, id, title.to_string(), text.to_string())
}

fn playlist_entry_to_inline_query_result_article(
    bot: &BotWrapped,
    vid: &PlaylistEntry,
    locale: &str,
) -> InlineQueryResultArticle {
    let thumbnail_url = format!("https://i.ytimg.com/vi/{}/maxresdefault.jpg", vid.id);
    let track_title_performer = get_title_and_perfomer(&vid.title, vid.uploader.as_deref());

    let (title, performer) = (track_title_performer.title, track_title_performer.performer);

    let mut article = InlineQueryResultArticle::new(
        get_temporary_id(&vid.id),
        match is_totally_invisible(&title) {
            true => BLANK_PLACEHOLDER.to_string(),
            false => title.clone(),
        },
        InputMessageContent::Text(InputMessageContentText {
            message_text: downloading_message_text(
                locale,
                &vid.id,
                performer.as_str(),
                title.as_str(),
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
        t!("inline.loading", locale = locale),
        InlineKeyboardButtonKind::CallbackData("loading".to_string()),
    )]]))
    .description(performer);

    if let Ok(thumbnail_url) = url::Url::from_str(&thumbnail_url) {
        article = article.thumbnail_url(thumbnail_url);
    }

    if let Ok(video_url) = url::Url::from_str(&vid.url) {
        article = article.url(video_url);
    }

    article
}

pub async fn handle_inline_query(
    bot: BotWrapped,
    inline_query: InlineQuery,
    downloader: Arc<Downloader>,
    me: Me,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();

    let locale = detect_locale(&inline_query.from);
    let query = inline_query.query;

    if query.is_empty() {
        if let Err(error) = bot
            .answer_inline_query(
                inline_query.id,
                Vec::<InlineQueryResult>::from([InlineQueryResult::Article(
                    InlineQueryResultArticle::new(
                        get_temporary_id("type_query"),
                        t!("inline.type_query_hint", locale = locale),
                        InputMessageContent::Text(InputMessageContentText::new(t!(
                            "start.title",
                            locale = locale,
                            bot_username = me.username()
                        ))),
                    ),
                )]),
            )
            .await
        {
            log_request_error("Failed to answer empty inline query", &error);
        }

        return Ok(());
    }

    if is_youtube_url(&query) {
        let video = match downloader.fetch_video_info(query.trim()).await {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to fetch video info for URL: {}: {}", query, e);
                return Ok(());
            }
        };

        if let Some(kind) = check_duration(
            video.duration.map(|duration| duration as f64),
            *MIN_DURATION,
            *MAX_DURATION,
        ) {
            let results = vec![duration_notice(&bot, locale, kind)];

            if let Err(e) = bot
                .answer_inline_query(inline_query.id, results)
                .cache_time(*INLINE_CACHE_TIME)
                .await
            {
                log_request_error("Failed to answer inline query", &e);
            }

            return Ok(());
        }

        let entry = PlaylistEntry {
            id: video.id.clone(),
            title: video.title.clone(),
            url: video
                .webpage_url
                .clone()
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={}", video.id)),
            index: None,
            duration: video.duration.map(|d| d as f64),
            thumbnail: video.thumbnail.clone(),
            uploader: video.uploader.clone(),
            channel_id: video.channel_id.clone(),
            availability: video.availability.clone(),
        };

        let results: Vec<InlineQueryResult> = vec![InlineQueryResult::Article(
            playlist_entry_to_inline_query_result_article(&bot, &entry, locale),
        )];

        if let Err(e) = bot
            .answer_inline_query(inline_query.id, results)
            .cache_time(*INLINE_CACHE_TIME)
            .await
        {
            log_request_error("Failed to answer inline query", &e);
        }

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
            InlineQueryResult::Article(playlist_entry_to_inline_query_result_article(
                &bot, vid, locale,
            ))
        })
        .collect();

    if results.is_empty() {
        results.push(notice_article(
            &bot,
            (*NO_RESULTS_ID).clone(),
            t!("inline.no_results_title", locale = locale).to_string(),
            t!("inline.no_results_text", locale = locale).to_string(),
        ));
    }

    if let Err(e) = bot
        .answer_inline_query(inline_query.id, results)
        .cache_time(*INLINE_CACHE_TIME)
        .await
    {
        log_request_error("Failed to answer inline query", &e);
    };

    let elapsed = start_time.elapsed();
    log::debug!("Inline query handled in {:?}", elapsed);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::downloading_message_text;

    #[test]
    fn downloading_message_links_the_track_to_its_youtube_page() {
        let text = downloading_message_text("en", "dQw4w9WgXcQ", "Performer", "Title");

        assert!(text.contains(
            "<a href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\">Performer — Title</a>"
        ));
        assert!(!text.contains("%{"));
    }

    #[test]
    fn downloading_message_escapes_track_metadata() {
        let text = downloading_message_text("en", "video-id", "A&B", "<Title>");

        assert!(text.contains("A&amp;B — &lt;Title&gt;"));
        assert!(!text.contains("<Title>"));
    }

    #[test]
    fn downloading_message_is_translated() {
        assert!(downloading_message_text("ru", "video-id", "P", "T").contains("Скачивание"));
        assert!(downloading_message_text("uk", "video-id", "P", "T").contains("Завантаження"));
    }

    #[test]
    fn duration_notice_texts_are_localized() {
        for locale in ["en", "ru", "uk"] {
            let too_long = t!(
                "inline.too_long_text",
                locale = locale,
                max_duration = "10:00"
            )
            .to_string();
            let too_short = t!(
                "inline.too_short_text",
                locale = locale,
                min_duration = "0:01"
            )
            .to_string();

            assert!(too_long.contains("10:00"), "locale {locale}: {too_long}");
            assert!(too_short.contains("0:01"), "locale {locale}: {too_short}");
            assert!(
                !too_long.contains("%{"),
                "locale {locale} has raw placeholders"
            );
            assert!(
                !too_short.contains("%{"),
                "locale {locale} has raw placeholders"
            );
        }
    }
}
