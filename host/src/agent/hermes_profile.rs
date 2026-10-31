//! A local Hermes bot's own profile. Every bot on this computer's Hermes runs as a profile of its own
//! (`hermes -p codync-… acp`), so its memory, skills and settings aren't the user's default profile's, and two bots
//! don't share them. A profile that can't be made falls back to the default one, and the bot's chat says so.

use crate::store::BotConfig;
use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
use std::time::Duration;

/// How long creating one may take: a clone copies the default profile's config and skills.
const CREATE_TIMEOUT: Duration = Duration::from_mins(2);

/// The profile a local Hermes bot runs as: the same name it has on a gateway.
pub fn name(cfg: &BotConfig) -> String {
    crate::remote::hermes::profile_of(cfg)
}

/// This computer's Hermes home: `HERMES_HOME`, else `~/.hermes`.
fn home() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".hermes")))
}

fn dir(profile: &str) -> Option<PathBuf> {
    home().map(|home| home.join("profiles").join(profile))
}

/// Whether the bot's profile is already on this computer.
pub fn exists(profile: &str) -> bool {
    dir(profile).is_some_and(|dir| dir.is_dir())
}

/// Makes sure the bot's profile exists, creating it from the default profile the first time. Returns the profile to
/// run as; a profile Hermes refuses to make is this call's error, and the bot then runs in the default one.
pub async fn ensure(cfg: &BotConfig) -> Result<String> {
    let profile = name(cfg);
    if exists(&profile) {
        return Ok(profile);
    }
    let dir = dir(&profile).ok_or_else(|| anyhow!("this computer has no home folder to keep Hermes profiles in"))?;
    let hermes = crate::agent::backends::harness("hermes")
        .and_then(crate::agent::backends::installed_cli)
        .ok_or_else(|| anyhow!("Hermes isn't installed on this computer"))?;
    let made = profile.clone();
    let out = tokio::time::timeout(
        CREATE_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(hermes).args(["profile", "create", &made, "--clone", "--no-alias"]).output()
        }),
    )
    .await;
    // A profile another start of this bot made meanwhile is fine.
    if exists(&profile) {
        return Ok(profile);
    }
    let out = match out {
        Err(_) => return Err(anyhow!("creating the profile `{profile}` took over {}s", CREATE_TIMEOUT.as_secs())),
        Ok(Err(e)) => return Err(e).context("waiting for `hermes profile create`"),
        Ok(Ok(Err(e))) => return Err(e).context("running `hermes profile create`"),
        Ok(Ok(Ok(out))) => out,
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "`hermes profile create {profile}` failed: {}",
            crate::agent::acp::truncate(err.trim(), 300)
        ));
    }
    Err(anyhow!("`hermes profile create {profile}` left no profile in {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(named: Option<&str>) -> BotConfig {
        let mut value = json!({"id": "6f1c2a9e-3b44-4c8d-9e21-0a7b5c3d9f10", "name": "B", "backend": "hermes",
            "cwd": "/work/app"});
        if let Some(profile) = named {
            value["hermesProfile"] = profile.into();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_bot_runs_as_its_own_profile_named_after_its_id() {
        assert_eq!(name(&bot(None)), "codync-6f1c2a9e3b44");
        assert_eq!(name(&bot(Some("Reviewer"))), "reviewer");
        // An empty name is no name.
        assert_eq!(name(&bot(Some("  "))), "codync-6f1c2a9e3b44");
    }
}
