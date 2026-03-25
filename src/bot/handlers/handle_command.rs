use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use teloxide::{
    prelude::Requester,
    types::{ChatId, Message, UserId},
};

use crate::{
    bot::{Command, types::BotWrapped},
    cache::DataStore,
    db::DatabaseHelper,
};

async fn handle_stats(
    bot: BotWrapped,
    chat_id: ChatId,
    user_id: UserId,
    db: Arc<DatabaseHelper>,
    data_store: Arc<Mutex<DataStore>>,
) -> anyhow::Result<()> {
    let mut register_date_str = "???".to_string();

    if let Some(register_date) = db.get_user_register_date(&user_id.0) {
        register_date_str = register_date.format("%Y-%m-%d").to_string();
    };

    let dl_count = db.get_user_dl_count(&user_id.0);
    let total_dl_count = db.get_total_dl_count().unwrap_or(0);
    let total_users = data_store.lock().unwrap().get_total_users_count(&db);
    let monthly_active_users = data_store
        .lock()
        .unwrap()
        .get_cached_monthly_users_count(&db);
    let cached_files_count = data_store.lock().unwrap().get_cached_files_count(&db);
    let downloaded_files_count = data_store.lock().unwrap().get_downloaded_files_count();

    let message = format!(
        include_str!("../resources/stats.html"),
        total_users,
        monthly_active_users,
        downloaded_files_count,
        cached_files_count,
        total_dl_count,
        dl_count,
        register_date_str,
    );

    bot.send_message(chat_id, message).await?;

    Ok(())
}

pub async fn handle_command(
    bot: BotWrapped,
    message: Message,
    command: Command,
    db: Arc<DatabaseHelper>,
    data_store: Arc<Mutex<DataStore>>,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();

    let chat = message.chat;
    let user = message.from.ok_or(anyhow!("No user ID found"))?;

    match command {
        Command::Start => {
            if let Err(e) = bot
                .send_message(chat.id, include_str!("../resources/start.html"))
                .await
            {
                log::warn!("Failed to send start message: {:?}", e);
            }
        }
        Command::Help => {
            if let Err(e) = bot
                .send_message(chat.id, include_str!("../resources/help.html"))
                .await
            {
                log::warn!("Failed to send help message: {:?}", e);
            }
        }
        Command::Stats => handle_stats(bot, chat.id, user.id, db, data_store).await?,
    };

    let elapsed = start_time.elapsed();
    log::debug!("Command {:?} handled in {:?}", command, elapsed);

    Ok(())
}
