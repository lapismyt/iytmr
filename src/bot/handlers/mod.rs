mod handle_callback_query;
mod handle_chosen_inline_result;
mod handle_command;
mod handle_edited_message;
mod handle_inline_query;
mod handle_text;

pub use handle_callback_query::handle_callback_query;
pub use handle_chosen_inline_result::handle_chosen_inline_result;
pub use handle_command::handle_command;
pub use handle_edited_message::handle_edited_message;
pub use handle_inline_query::handle_inline_query;
pub use handle_text::handle_text;
