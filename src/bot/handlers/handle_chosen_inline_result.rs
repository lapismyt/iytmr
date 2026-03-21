use std::sync::Arc;

use teloxide::types::ChosenInlineResult;

use crate::{bot::types::BotWrapped, db::DatabaseHelper, downloader::Downloader};

pub async fn handle_chosen_inline_result(
    _bot: &BotWrapped,
    _chosen_inline_result: ChosenInlineResult,
    _downloader: Arc<Downloader>,
    _db: Arc<DatabaseHelper>,
) -> anyhow::Result<()> {
    Ok(())
}
