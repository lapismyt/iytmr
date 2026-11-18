pub mod errors;
mod handlers;
pub mod types;

use std::sync::Arc;
use tokio::sync::Mutex;

use teloxide::{
    dispatching::{HandlerExt as _, UpdateFilterExt},
    dptree,
    macros::BotCommands,
    prelude::Dispatcher,
    types::{Update, User},
};

use crate::{bot::types::BotWrapped, cache::DataStore};
use crate::{db::DatabaseHelper, downloader::Downloader};

pub fn detect_locale(user: &User) -> &str {
    user.language_code.as_deref().unwrap_or("en")
}

/// Builds the YouTube watch URL of a video.
pub fn video_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={}", video_id)
}

/// Formats a duration limit as `m:ss` for user-facing messages.
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase")]
enum Command {
    Start,
    Help,
    Stats,
}

pub async fn run(
    bot: BotWrapped,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
    counters: Arc<Mutex<DataStore>>,
) {
    let db_for_inspect = db.clone();

    let handler = dptree::entry()
        .inspect(move |update: Update| {
            if let Some(user) = update.from()
                && let Err(e) = db_for_inspect.handle_user_interaction(&user.id.0)
            {
                log::warn!("Failed to handle user interaction: {:?}", e);
            }
        })
        .branch(
            Update::filter_message()
                .branch(
                    dptree::entry()
                        .filter_command::<Command>()
                        .endpoint(handlers::handle_command),
                )
                .branch(dptree::entry().endpoint(handlers::handle_text)),
        )
        .branch(Update::filter_inline_query().endpoint(handlers::handle_inline_query))
        .branch(
            Update::filter_chosen_inline_result().endpoint(handlers::handle_chosen_inline_result),
        )
        .branch(Update::filter_callback_query().endpoint(handlers::handle_callback_query))
        .branch(Update::filter_edited_message().endpoint(handlers::handle_edited_message));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![downloader, db, counters])
        .error_handler(Arc::new(|error: anyhow::Error| async move {
            errors::log_error("Dispatcher handler failed", &error);
        }))
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

#[cfg(test)]
mod tests {
    use super::{format_duration, video_url};

    #[test]
    fn video_url_points_to_the_youtube_watch_page() {
        assert_eq!(
            video_url("dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn format_duration_renders_minutes_and_padded_seconds() {
        assert_eq!(format_duration(600.0), "10:00");
        assert_eq!(format_duration(90.0), "1:30");
        assert_eq!(format_duration(5.0), "0:05");
        assert_eq!(format_duration(0.4), "0:00");
    }
}
