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

use crate::downloader::Downloader;
use crate::{bot::types::BotWrapped, db::DatabaseHelper};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
enum Command {
    Start,
    Help,
}

pub async fn run(bot: BotWrapped, downloader: Arc<Downloader>, db: Arc<DatabaseHelper>) {
    let handler = dptree::entry()
        .inspect(|_update: Update| {})
        .branch(
            dptree::entry()
                .filter_command::<Command>()
                .endpoint(handlers::handle_command),
        )
        .branch(Update::filter_inline_query().endpoint(handlers::handle_inline_query))
        .branch(
            Update::filter_chosen_inline_result().endpoint(handlers::handle_chosen_inline_result),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![downloader, db])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
