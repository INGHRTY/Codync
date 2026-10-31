//! Setting up a bot's profile on the gateway: `hermes profile create <name> --clone`, the `API_SERVER_KEY` that
//! profile's `/p/<name>/` routes answer with, and the bot's Codync tools.
//!
//! The tools are an MCP server on this computer (`/mcp/<bot>`, `mcp::bridge`): a gateway's Hermes can't start a
//! process here, so the profile reaches them over the network with a token for that bot alone.
//!
//! A gateway's profiles are directories on that computer, so this needs shell access there: the connection may
//! name an SSH host. Without one, the user makes the profile and saves its key themselves (`saveConnection`).

use super::connections::{self, Connection};
use crate::hub::Hub;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a profile setup may take (a clone copies skills and config).
const SETUP_TIMEOUT: Duration = Duration::from_mins(2);

/// How long one address may take to answer from the gateway before the next is tried.
const PROBE_TIMEOUT: u8 = 8;

/// A failed install is retried after this long, not on every connection: a computer the gateway can't reach now
/// may be reachable later (a laptop that slept, a tailnet that came back).
const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;

/// What a bot's bridge was installed with: the profile it went into, the addresses tried, and the one that answered
/// (`None`: none did).
#[derive(Serialize, Deserialize)]
struct Installed {
    profile: String,
    tried: Vec<String>,
    url: Option<String>,
    at: i64,
}

impl Installed {
    /// Whether this is still what the gateway holds: the same profile, the same addresses, and either a working one
    /// or a failure recent enough to wait out.
    fn holds(&self, profile: &str, tried: &[String], now: i64) -> bool {
        self.profile == profile && self.tried == tried && (self.url.is_some() || now - self.at < RETRY_AFTER_MS)
    }
}

/// Makes sure `profile` exists on the gateway with a key, and holds this bot's Codync tools. A key saved for it
/// before is reused as it is, so a bot whose profile is already set up costs no shell call. `Some` is something the
/// user should hear: the tools couldn't be installed, and the bot works without them.
pub async fn ensure(hub: &Hub, connection: &Connection, profile: &str, bot: &str) -> Result<Option<String>> {
    profile_key(hub, connection, profile).await?;
    Ok(match bridge(hub, connection, profile, bot).await {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(bot, error = format!("{e:#}"), "the bot on the gateway has no Codync tools");
            Some(format!("This bot has no Codync tools (messages, memory, other bots) on {}: {e:#}", connection.name))
        }
    })
}

/// The profile itself, and the key its `/p/<name>/` routes answer with.
async fn profile_key(hub: &Hub, connection: &Connection, profile: &str) -> Result<()> {
    let store = &hub.store;
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

/// The bot's Codync tools in the profile: the first of this computer's addresses the gateway answers on, written
/// into the profile's `config.yaml`, its token into the profile's `.env`. Nothing is written when no address
/// answers, so a network that can't carry the tools leaves the profile as it was.
async fn bridge(hub: &Hub, connection: &Connection, profile: &str, bot: &str) -> Result<()> {
    let Some(ssh) = connection.ssh.as_deref() else {
        // A connection without one is the user's own setup (they make the profile and save its key): there's
        // nothing to install, and nothing to tell them.
        tracing::info!(bot, "no SSH host on the connection: the bot's Codync tools stay off");
        return Ok(());
    };
    let path = format!("/mcp/{bot}");
    let port = hub.port;
    let tried: Vec<String> = tokio::task::spawn_blocking(move || {
        crate::service::addresses(port).into_iter().map(|address| format!("{address}{path}")).collect()
    })
    .await?;
    if tried.is_empty() {
        bail!("this computer has no address the gateway could reach");
    }
    let record = format!("mcp-bridge:{bot}");
    let now = crate::store::now_ms();
    if hub
        .store
        .kv_get(&record)
        .and_then(|v| serde_json::from_str::<Installed>(&v).ok())
        .is_some_and(|i| i.holds(profile, &tried, now))
    {
        return Ok(());
    }
    let token = crate::mcp::bridge::token(hub, bot)?;
    let (url, failure) = match install(ssh, profile, &tried, &token).await {
        Ok(Some(url)) => (Some(url), None),
        Ok(None) => (None, Some(anyhow!("none of this computer's addresses answers from {ssh}"))),
        Err(e) => (None, Some(e)),
    };
    // A working address holds until the gateway's addresses change; a failure waits [`RETRY_AFTER_MS`], so a
    // gateway that can't reach this computer costs one SSH call and one notice, not one per connection.
    let installed = Installed { profile: profile.to_owned(), tried, url, at: now };
    hub.store.kv_set(&record, &serde_json::to_string(&installed)?)?;
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Writes the bridge into the profile, answering the address that answered from the gateway (`None`: none did).
async fn install(ssh: &str, profile: &str, tried: &[String], token: &str) -> Result<Option<String>> {
    let url = probe(ssh, tried, token).await.with_context(|| format!("reaching this computer from {ssh}"))?;
    let Some(url) = url else { return Ok(None) };
    let (config, env) = read_profile(ssh, profile).await?;
    let config = with_mcp_server(&config, &url)?;
    let env = with_key(&env, token);
    write_profile(ssh, profile, &config, &env)
        .await
        .with_context(|| format!("installing its tools in profile `{profile}` on {ssh}"))?;
    Ok(Some(url))
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

/// Asks the gateway's computer for each address in turn: the first that answers the bot's own token is the one its
/// profile gets (`OK <url>`), so an address that computer can't use is never written down.
async fn probe(ssh: &str, urls: &[String], token: &str) -> Result<Option<String>> {
    let urls = urls.iter().map(|u| crate::shell::quote(u)).collect::<Vec<_>>().join(" ");
    let body = crate::shell::quote(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    // `format!`, never placeholders replaced in turn: a URL or a token could spell another's placeholder.
    let script = format!(
        r#"for c in {urls}; do
  if curl -sf -m {PROBE_TIMEOUT} -o /dev/null -X POST -H 'Authorization: Bearer {token}' \
      -H 'Content-Type: application/json' -d {body} "$c"
  then echo "OK $c"; exit 0; fi
done
echo UNREACHABLE"#
    );
    let out = run_out(ssh, &script).await?;
    let out = out.trim();
    Ok(out.strip_prefix("OK ").map(|url| url.trim().to_owned()).filter(|url| !url.is_empty()))
}

/// The marker between the two files `read_profile` reads in one call.
const SPLIT: &str = "---codync---";

async fn read_profile(ssh: &str, profile: &str) -> Result<(String, String)> {
    let profile = crate::shell::quote(profile);
    let script = format!(
        r#"d="$HOME/.hermes/profiles/"{profile}; cat "$d/config.yaml" 2>/dev/null; printf '\n{SPLIT}\n'; cat "$d/.env" 2>/dev/null"#
    );
    let out = run_out(ssh, &script).await?;
    let (config, env) = out.split_once(&format!("\n{SPLIT}\n")).unwrap_or((out.as_str(), ""));
    Ok((config.to_owned(), env.to_owned()))
}

async fn write_profile(ssh: &str, profile: &str, config: &str, env: &str) -> Result<()> {
    run(ssh, &write_script(profile, config, env)).await
}

/// Both files at once, through base64 so no line of YAML or a token meets the shell's quoting. Each value goes in
/// once, by `format!`: a payload that spells a placeholder of its own can't be mistaken for one.
fn write_script(profile: &str, config: &str, env: &str) -> String {
    use base64::Engine as _;
    let b64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    format!(
        r#"d="$HOME/.hermes/profiles/"{profile}; mkdir -p "$d"
printf '%s' '{config}' | base64 -d > "$d/config.yaml"
printf '%s' '{env}' | base64 -d > "$d/.env"; chmod 600 "$d/.env""#,
        profile = crate::shell::quote(profile),
        config = b64(config),
        env = b64(env),
    )
}

/// `config` with this bot's bridge under `mcp_servers.codync`, replacing an entry an earlier setup left. A
/// `mcp_servers` written other than as a block of its own (`mcp_servers: {other: …}`) is refused rather than
/// appended to, which would leave the file with two keys and lose the profile's own servers.
fn with_mcp_server(config: &str, url: &str) -> Result<String> {
    let entry = [
        "  codync:".to_owned(),
        format!("    url: {url}"),
        "    headers:".to_owned(),
        "      Authorization: Bearer ${MCP_CODYNC_API_KEY}".to_owned(),
    ];
    let mut lines: Vec<String> = Vec::new();
    let mut skipping = false;
    for line in config.lines() {
        if line.trim_end() == "  codync:" {
            skipping = true;
            continue;
        }
        if skipping {
            // The entry's own lines, up to the next key at its level.
            if line.trim().is_empty() || line.starts_with("    ") {
                continue;
            }
            skipping = false;
        }
        lines.push(line.to_owned());
    }
    let mut out: Vec<String> = Vec::new();
    let mut placed = false;
    for line in lines {
        match line.strip_prefix("mcp_servers:").map(str::trim) {
            _ if placed => out.push(line),
            // An empty `mcp_servers` (Hermes writes `{}`): the entry's key takes its place.
            Some("{}") => {
                out.push("mcp_servers:".to_owned());
                out.extend(entry.iter().cloned());
                placed = true;
            }
            // A key of its own, perhaps with a comment on it: the entry goes under it.
            Some(rest) if rest.is_empty() || rest.starts_with('#') => {
                out.push(line);
                out.extend(entry.iter().cloned());
                placed = true;
            }
            // Written some other way, which this can't edit without guessing: leave the profile alone.
            Some(other) => bail!("its config.yaml writes mcp_servers inline ({other})"),
            None => out.push(line),
        }
    }
    if !placed {
        if out.last().is_some_and(|line| !line.trim().is_empty()) {
            out.push(String::new());
        }
        out.push("mcp_servers:".to_owned());
        out.extend(entry);
    }
    let mut text = out.join("\n");
    text.push('\n');
    Ok(text)
}

/// `env` with `MCP_CODYNC_API_KEY` set to the bot's token (`config.yaml` reads it by name), other lines kept.
fn with_key(env: &str, token: &str) -> String {
    let mut out: Vec<String> =
        env.lines().filter(|line| !line.starts_with("MCP_CODYNC_API_KEY=")).map(str::to_owned).collect();
    out.push(format!("MCP_CODYNC_API_KEY={token}"));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// Runs `script` in a login shell on `ssh` (`[user@]host`, or a host alias from `~/.ssh/config`), so the
/// gateway's own `hermes`, on a login PATH, is the one that runs.
async fn run(ssh: &str, script: &str) -> Result<()> {
    run_out(ssh, script).await.map(|_| ())
}

/// The same, answering what the script printed.
async fn run_out(ssh: &str, script: &str) -> Result<String> {
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
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

    #[test]
    fn the_bridge_is_written_under_mcp_servers_and_rewritten_in_place() {
        let url = "http://100.111.188.83:19222/mcp/b1";
        let written = with_mcp_server("model: hermes\nmcp_servers:\n  other:\n    url: http://x\n", url).unwrap();
        assert!(
            written.contains("mcp_servers:\n  codync:\n    url: http://100.111.188.83:19222/mcp/b1\n    headers:"),
            "{written}"
        );
        assert!(written.contains("  other:\n    url: http://x"), "the profile's own servers stay: {written}");
        assert!(written.contains("Authorization: Bearer ${MCP_CODYNC_API_KEY}"));
        // Set up again (a new address): one entry, not two.
        let moved = with_mcp_server(&written, "http://100.101.122.109:19222/mcp/b1").unwrap();
        assert_eq!(moved.matches("  codync:").count(), 1, "{moved}");
        assert!(moved.contains("100.101.122.109"), "{moved}");
        assert!(!moved.contains("100.111.188.83"), "{moved}");
        assert_eq!(with_mcp_server(&moved, "http://100.101.122.109:19222/mcp/b1").unwrap(), moved, "nothing twice");
    }

    #[test]
    fn a_profile_without_servers_gets_them() {
        for config in ["", "model: hermes\n", "model: hermes\nmcp_servers: {}\n"] {
            let written = with_mcp_server(config, "http://host/mcp/b1").unwrap();
            assert_eq!(written.matches("mcp_servers:").count(), 1, "{config:?} → {written}");
            assert!(written.contains("\n  codync:\n"), "{written}");
            // A second run leaves the entry where it is.
            assert_eq!(with_mcp_server(&written, "http://host/mcp/b1").unwrap(), written);
        }
    }

    #[test]
    fn a_config_this_cant_edit_safely_is_left_alone() {
        // Appending a second `mcp_servers:` would lose the profile's own servers.
        let inline = "model: hermes\nmcp_servers: {other: {url: http://x}}\n";
        assert!(with_mcp_server(inline, "http://host/mcp/b1").is_err());
        // A comment after the key is still the key.
        let commented = with_mcp_server("mcp_servers:  # mine\n", "http://host/mcp/b1").unwrap();
        assert!(commented.contains("mcp_servers:  # mine\n  codync:\n"), "{commented}");
    }

    #[test]
    fn a_payload_that_spells_a_placeholder_is_written_whole() {
        // Base64 of text ending in `ENV`/`CONFIG` used to be re-substituted into the other file's place.
        let config = "name: ENVCONFIG\n".repeat(40);
        let env = "MCP_CODYNC_API_KEY=cdcb_x\nENV=1\n";
        let script = write_script("codync-a", &config, env);
        let b64 = |text: &str| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(text)
        };
        assert!(script.contains(&b64(&config)), "the config goes in as it is");
        assert!(script.contains(&b64(env)), "so does the env file");
        assert_eq!(script.matches("base64 -d > \"$d/config.yaml\"").count(), 1);
    }

    #[test]
    fn the_token_lands_in_the_profiles_env() {
        let env = with_key("OPENAI_API_KEY=sk-1\nMCP_CODYNC_API_KEY=old\n", "cdcb_new");
        assert_eq!(env, "OPENAI_API_KEY=sk-1\nMCP_CODYNC_API_KEY=cdcb_new\n");
        assert_eq!(with_key("", "cdcb_new"), "MCP_CODYNC_API_KEY=cdcb_new\n");
    }

    #[test]
    fn a_failed_install_waits_before_the_next_try() {
        let tried = vec!["http://a/mcp/b1".to_owned()];
        let installed = |url: Option<&str>, at: i64| Installed {
            profile: "codync-a".to_owned(),
            tried: tried.clone(),
            url: url.map(str::to_owned),
            at,
        };
        let holds = |i: &Installed, now: i64| i.holds("codync-a", &tried, now);
        assert!(holds(&installed(Some("http://a/mcp/b1"), 0), i64::MAX), "a working address holds");
        assert!(!holds(&installed(None, 0), RETRY_AFTER_MS + 1), "a failure is retried later");
        assert!(holds(&installed(None, 0), 1), "but not on every connection");
        let other = ["http://b/mcp/b1".to_owned()];
        assert!(!installed(Some("http://a/mcp/b1"), 0).holds("codync-a", &other, 1), "a new address");
        // Two bots naming one profile don't count as each other's install.
        assert!(!installed(Some("http://a/mcp/b1"), 0).holds("codync-b", &tried, 1), "another profile");
    }
}
