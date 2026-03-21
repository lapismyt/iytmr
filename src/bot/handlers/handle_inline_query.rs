use std::sync::Arc;

use teloxide::{Bot, types::InlineQuery};

use crate::{db::DatabaseHelper, downloader::Downloader};

pub async fn handle_inline_query(
    bot: &BotWrapped,
    inline_query: InlineQuery,
    downloader: Arc<Downloader>,
    db: Arc<DatabaseHelper>,
) -> anyhow::Result<()> {
    Ok(())
}
