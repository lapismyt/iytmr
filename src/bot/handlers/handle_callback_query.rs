use teloxide::{prelude::Requester, types::CallbackQuery};

use crate::bot::types::BotWrapped;

pub async fn handle_callback_query(
    bot: BotWrapped,
    callback_query: CallbackQuery,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();

    let Some(callback_data) = callback_query.data else {
        return Ok(());
    };

    if callback_data == "loading" {
        bot.answer_callback_query(callback_query.id).await?;
    }

    let elapsed = start_time.elapsed();
    log::debug!("Inline query handled in {:?}", elapsed);

    Ok(())
}
