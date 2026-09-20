use std::sync::Arc;
use tokio::sync::Mutex;

use teloxide::{Bot, net::default_reqwest_settings, prelude::RequesterExt, types::ParseMode};

use crate::{cache::DataStore, db::DatabaseHelper, downloader::Downloader};

#[macro_use]
extern crate rust_i18n;

i18n!("locales", fallback = "en");

mod bot;
mod cache;
mod consts;
mod db;
mod downloader;
mod parser;

fn build_http_client() -> reqwest::Client {
    let mut builder = default_reqwest_settings();

    if let Some(ip) = *consts::SEND_THROUGH {
        log::info!("Binding HTTP client to local address: {}", ip);
        builder = builder.local_address(ip);
    }

    builder.build().expect("HTTP client must be built correctly")
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv_override();
    pretty_env_logger::init();

    log::info!("Initializing downloader...");
    let downloader = Arc::new(
        Downloader::new(consts::OUTPUT_DIR, consts::CACHE_DIR, consts::LIBS_DIR)
            .await
            .expect("downloader must be initialized correctly"),
    );

    log::info!("Initializing database...");
    let db = Arc::new(DatabaseHelper::new(consts::DB_PATH));

    let client = build_http_client();

    log::info!("Starting bot...");
    bot::run(
        Bot::from_env_with_client(client).parse_mode(ParseMode::Html),
        downloader.clone(),
        db.clone(),
        Arc::new(Mutex::new(DataStore::new(db.clone()))),
    )
    .await;
}
