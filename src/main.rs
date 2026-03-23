use std::sync::Arc;

use teloxide::{Bot, prelude::RequesterExt, types::ParseMode};

use crate::{db::DatabaseHelper, downloader::Downloader};

mod bot;
mod consts;
mod db;
mod downloader;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().unwrap();
    pretty_env_logger::init();

    log::info!("Initializing downloader...");
    let downloader = Arc::new(
        Downloader::new(consts::OUTPUT_DIR, consts::CACHE_DIR, consts::LIBS_DIR)
            .await
            .unwrap(),
    );

    log::info!("Initializing database...");
    let db = Arc::new(DatabaseHelper::new(consts::DB_PATH));

    log::info!("Starting bot...");
    bot::run(
        Bot::from_env().parse_mode(ParseMode::Html),
        downloader.clone(),
        db.clone(),
        Arc::new(crate::bot::types::DataStore::default()),
    )
    .await;
}
