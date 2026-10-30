//! What a bot's setup must hold before it's saved, whoever changes it (a client, a managing bot, the host).

use crate::store::{BotConfig, Store};
use anyhow::{Result, bail};

pub(crate) fn validate(store: &Store, cfg: &BotConfig) -> Result<()> {
    if cfg.name.trim().is_empty() {
        bail!("name is required");
    }
    if cfg.is_group() {
        return Ok(());
    }
    if cfg.lead && cfg.hidden {
        bail!("the lead bot can't be hidden");
    }
    if !cfg.cwd.is_empty() && !std::path::Path::new(&cfg.cwd).is_dir() {
        bail!("workspace folder does not exist: {}", cfg.cwd);
    }
    if let Some(profile) = cfg.hermes_profile.as_deref() {
        crate::remote::hermes::connections::parse_profile(profile)?;
    }
    // A bot on another computer runs Hermes there, so its backend needn't be installed here.
    match cfg.connection.as_deref() {
        Some(id) => remote(store, cfg, id)?,
        None if cfg.command.as_deref().map(str::trim).unwrap_or_default().is_empty()
            && !crate::agent::backends::is_known(&cfg.backend) =>
        {
            bail!("unknown backend {}", cfg.backend)
        }
        None => {}
    }
    crate::agent::fallback::check(cfg)
}

/// A bot whose agent runs on another computer's Hermes gateway (see `remote::hermes`): it has no local
/// process, so everything that needs one — fallbacks, leading the team, managing it — is out.
fn remote(store: &Store, cfg: &BotConfig, connection: &str) -> Result<()> {
    if cfg.backend != "hermes" {
        bail!("a bot on a Hermes connection runs as Hermes ({} isn't)", cfg.backend);
    }
    if crate::remote::hermes::connections::get(store, connection).is_none() {
        bail!("unknown Hermes connection {connection}");
    }
    if !cfg.fallbacks.is_empty() {
        bail!("bots on another computer can't fall back to another agent");
    }
    if cfg.lead {
        bail!("the lead bot runs on this computer");
    }
    if cfg.manage_team {
        bail!("bots on another computer can't manage this computer's bots");
    }
    Ok(())
}
