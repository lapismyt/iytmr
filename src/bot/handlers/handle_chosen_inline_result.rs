use std::sync::Arc;

use teloxide::{Bot, types::ChosenInlineResult};

use crate::{bot::types::BotWrapped, db::DatabaseHelper, downloader::Downloader};

pub async fn handle_chosen_inline_result(
    bot: &BotWrapped,
    chosen_inline_result: ChosenInlineResult,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
) -> anyhow::Result<()> {
    Ok(())
}
