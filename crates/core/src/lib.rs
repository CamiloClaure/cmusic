//! cmusic core: everything except the user interface.
//! Front ends call `engine::start()`, send `Command`s and render `Update`s.

pub mod api;
pub mod auth;
pub mod engine;
pub mod lyrics;
pub mod media;
pub mod persist;
pub mod player;
pub mod queue;
pub mod resolve;
pub mod types;

pub use types::*;
