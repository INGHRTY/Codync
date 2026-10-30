//! Setting up a bot's profile on the gateway: `hermes profile create <name> --clone`, and the `API_SERVER_KEY`
//! that profile's `/p/<name>/` routes answer with.
//!
//! A gateway's profiles are directories on that computer, so this needs shell access there: the connection may
//! name an SSH host. Without one, the user makes the profile and saves its key themselves (`saveConnection`).

use super::connections::{self, Connection};
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use std::time::Duration;

/// How long a profile setup may take (a clone copies skills and config).
const SETUP_TIMEOUT: Duration = Duration::from_mins(2);

/// Makes sure `profile` exists on the gateway with a key. A key saved for it before is reused as it is, so a bot
/// whose profile is already set up costs no shell call.
pub async fn ensure(store: &Store, connection: &Connection, profile: &str) -> Result<()> {
    if connections::has_key(store, connection, profile) {
        return Ok(());
    }
    let Some(ssh) = connection.ssh.as_deref() else {
        bail!(
            "profile `{profile}` has no key yet: give the connection an SSH host so Codync sets it up, or create \
             the profile on that computer and save its key"
        );
    };
    let key = new_key();
    run(ssh, &create_command(profile)).await.with_context(|| format!("creating profile `{profile}` on {ssh}"))?;
    run(ssh, &key_command(profile, &key)).await.with_context(|| format!("saving the key for `{profile}` on {ssh}"))?;
    let mut keys = connections::keys(store)?;
    keys.set(&connection.id, profile, &key);
    connections::write_keys(store, &keys)?;
    // The gateway reads the profile's key when the profile is first served, so this works without a restart; a
    // profile the gateway already serves answers its old key until it's restarted, which is the user's call.
    Ok(())
}

/// A gateway key: 24 random bytes, URL-safe base64.
fn new_key() -> String {
    format!("cdc_{}", crate::remote::crypto::b64(&crate::remote::crypto::random::<24>()))
}

/// `hermes profile create <name> --clone --no-alias`; a profile that already exists is left as it is.
fn create_command(profile: &str) -> String {
    format!(
        "hermes profile create {profile} --clone --no-alias 2>&1 || true; \
         test -d \"$HOME/.hermes/profiles/{profile}\""
    )
}

/// Writes the profile's key into its `.env`, replacing any key it had (the file holds the profile's other
/// settings too, so only that line changes).
fn key_command(profile: &str, key: &str) -> String {
    let env = format!("$HOME/.hermes/profiles/{profile}/.env");
    format!(
        "f={env}; mkdir -p \"$(dirname \"$f\")\"; touch \"$f\"; \
         grep -v '^API_SERVER_KEY=' \"$f\" > \"$f.codync\" || true; \
         printf 'API_SERVER_KEY=%s\\n' '{key}' >> \"$f.codync\"; mv \"$f.codync\" \"$f\"; chmod 600 \"$f\""
    )
}

/// Runs `script` in a login shell on `ssh` (`[user@]host`, or a host alias from `~/.ssh/config`), so the
/// gateway's own `hermes`, on a login PATH, is the one that runs.
async fn run(ssh: &str, script: &str) -> Result<()> {
    let target = target(ssh)?;
    let command = format!("bash -lc {}", crate::shell::quote(script));
    let out = tokio::time::timeout(
        SETUP_TIMEOUT,
        tokio::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "--"])
            .arg(&target)
            .arg(&command)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("ssh {target} didn't answer within {}s", SETUP_TIMEOUT.as_secs()))?
    .with_context(|| format!("running ssh to {target}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ssh {target}: {}", crate::agent::acp::truncate(err.trim(), 400));
    }
    Ok(())
}

/// A host ssh accepts as one argument: a name or address with an optional `user@`, never a flag or a shell word.
fn target(ssh: &str) -> Result<String> {
    let ssh = ssh.trim();
    let (user, host) = ssh.rsplit_once('@').map_or(("", ssh), |(u, h)| (u, h));
    let ok_host = !host.is_empty()
        && !host.starts_with('-')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    let ok_user = user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !ok_host || !ok_user {
        bail!("`{ssh}` isn't an SSH host (use `user@host`, or a host alias from ~/.ssh/config)");
    }
    Ok(ssh.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_targets_are_one_plain_argument() {
        assert_eq!(target("ing@100.101.122.109").unwrap(), "ing@100.101.122.109");
        assert_eq!(target("ing-vps").unwrap(), "ing-vps");
        assert!(target("-oProxyCommand=evil").is_err());
        assert!(target("host; rm -rf /").is_err());
        assert!(target("").is_err());
    }

    #[test]
    fn the_key_line_replaces_the_profiles_own() {
        let script = key_command("codync-a", "cdc_abc");
        assert!(script.contains("grep -v '^API_SERVER_KEY='"), "other settings stay");
        assert!(script.contains("printf 'API_SERVER_KEY=%s\\n' 'cdc_abc'"));
    }

    #[test]
    fn keys_are_long_enough_for_a_gateway() {
        assert!(new_key().len() >= 16);
        assert_ne!(new_key(), new_key());
    }
}
