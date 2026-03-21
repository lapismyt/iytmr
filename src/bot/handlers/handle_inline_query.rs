use std::sync::Arc;

use teloxide::types::InlineQuery;

use crate::{bot::types::BotWrapped, db::DatabaseHelper, downloader::Downloader};

pub async fn handle_inline_query(
    _bot: &BotWrapped,
    _inline_query: InlineQuery,
    _downloader: Arc<Downloader>,
    _db: Arc<DatabaseHelper>,
) -> anyhow::Result<()> {
    Ok(())
}
