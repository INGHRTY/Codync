//! Hermes gateways on other computers: connections, the API client, and the profiles a bot runs as.

pub mod client;
pub mod connections;
pub mod events;
pub mod provision;

pub use client::Client;
pub use connections::Connection;

/// The Hermes profile a bot runs as, on either side: its own name, or one from its id. One profile per bot keeps
/// the bot's memory, skills and sessions apart from every other bot's (`hermes profile create … --clone`).
pub fn profile_of(cfg: &crate::store::BotConfig) -> String {
    match cfg.hermes_profile.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(profile) => profile.to_ascii_lowercase(),
        None => format!("codync-{}", cfg.id.chars().filter(char::is_ascii_hexdigit).take(12).collect::<String>()),
    }
}
