use teloxide::{Bot, adaptors::DefaultParseMode};

pub type BotWrapped = DefaultParseMode<Bot>;
