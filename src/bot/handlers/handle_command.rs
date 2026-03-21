use std::sync::Arc;

use anyhow::anyhow;
use teloxide::{
    dispatching::dialogue::GetChatId,
    prelude::Requester,
    types::Update,
};

use crate::{
    bot::{Command, types::BotWrapped},
    db::DatabaseHelper,
};

pub async fn handle_command(
    bot: &BotWrapped,
    update: Update,
    command: Command,
    db: Arc<DatabaseHelper>,
) -> anyhow::Result<()> {
    if let Some(user) = update.from() {
        if let Err(e) = db.handle_user_interaction(user.id.0) {
            log::warn!("Failed to handle user interaction: {:?}", e);
        }
    }

    let chat_id = update.chat_id().ok_or(anyhow!("No chat ID found"))?;

    match command {
        Command::Start => {
            if let Err(e) = bot
                .send_message(chat_id, include_str!("../resources/start.html"))
                .await
            {
                log::warn!("Failed to send start message: {:?}", e);
            }
        }
        Command::Help => {
            if let Err(e) = bot
                .send_message(chat_id, include_str!("../resources/help.html"))
                .await
            {
                log::warn!("Failed to send help message: {:?}", e);
            }
        }
    };

    Ok(())
}
