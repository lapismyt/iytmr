mod handlers;
pub mod types;

use std::sync::Arc;

use teloxide::{
    dispatching::{HandlerExt as _, UpdateFilterExt},
    dptree,
    macros::BotCommands,
    prelude::Dispatcher,
    types::Update,
};

use crate::bot::types::{BotWrapped, DataStore};
use crate::{db::DatabaseHelper, downloader::Downloader};

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
    counters: Arc<DataStore>,
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
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
