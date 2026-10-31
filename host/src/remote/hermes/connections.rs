//! Hermes gateways on other computers: the VPS the user's cloud bots run on.
//!
//! A connection is a base URL and the bearer key that gateway answers with (kv `connections`, the key in
//! the vault). One gateway serves every profile it multiplexes, each under `/p/<profile>/` with **its own**
//! key, so a connection holds one key per profile rather than one key for the gateway.

use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const KV: &str = "connections";
/// The vault slot holding the connection's keys, one per profile.
pub const KEY_SLOT: &str = "hermes";

/// One Hermes gateway, as the user set it up.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    /// `https://vps.example.com` or `http://100.x.y.z:8642`, without a `/p/<profile>` suffix.
    pub base_url: String,
    /// The profile this connection's un-prefixed routes serve; the gateway's own, usually `default`.
    #[serde(default = "default_profile")]
    pub default_profile: String,
    /// How to reach that computer's shell (`user@host`, or a host alias from `~/.ssh/config`), so Codync can make a
    /// bot's profile and its key there (`provision`). `None`: the user sets profiles up themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    /// The last time the gateway answered, in ms; `None` until it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
}

fn default_profile() -> String {
    "default".into()
}

/// A connection's key for one profile.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// `<connection id>/<profile>` -> bearer key: two gateways can both serve a `default` profile, with different
    /// keys. The default profile's key is what the un-prefixed routes take.
    #[serde(default)]
    pub profiles: std::collections::BTreeMap<String, String>,
}

impl Keys {
    fn slot(connection: &str, profile: &str) -> String {
        format!("{connection}/{profile}")
    }

    /// Saves `key` for one profile of one connection.
    pub fn set(&mut self, connection: &str, profile: &str, key: &str) {
        self.profiles.insert(Self::slot(connection, profile), key.to_owned());
    }
}

impl Connection {
    /// Where a profile's routes live: `/p/<profile>` for any profile but the default one.
    pub fn profile_url(&self, profile: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        if profile == self.default_profile || profile.is_empty() {
            base.to_owned()
        } else {
            format!("{base}/p/{profile}")
        }
    }
}

/// Every connection, in the order the user made them.
pub fn load(store: &Store) -> Vec<Connection> {
    let Some(text) = store.kv_get(KV) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|error| {
        tracing::warn!(error = format!("{error:#}"), "couldn't read the saved connections");
        Vec::new()
    })
}

fn save(store: &Store, all: &[Connection]) -> Result<()> {
    store.kv_set(KV, &serde_json::to_string(all)?)
}

pub fn get(store: &Store, id: &str) -> Option<Connection> {
    load(store).into_iter().find(|c| c.id == id)
}

/// Adds or replaces a connection, keeping its keys as they were unless `keys` replaces them.
pub fn upsert(store: &Store, connection: Connection) -> Result<()> {
    let mut all = load(store);
    match all.iter_mut().find(|c| c.id == connection.id) {
        Some(existing) => *existing = connection,
        None => all.push(connection),
    }
    save(store, &all)
}

/// Takes out a connection and the keys it held; other connections keep theirs.
pub fn remove(store: &Store, id: &str) -> Result<()> {
    let all: Vec<Connection> = load(store).into_iter().filter(|c| c.id != id).collect();
    save(store, &all)?;
    let mut keys = keys(store)?;
    let prefix = format!("{id}/");
    keys.profiles.retain(|slot, _| !slot.starts_with(&prefix));
    write_keys(store, &keys)
}

/// The keys saved for one connection.
pub fn keys(store: &Store) -> Result<Keys> {
    match crate::market::vault::read(store, KEY_SLOT)? {
        Some(text) => Ok(serde_json::from_str(&text).context("unreadable Hermes connection keys")?),
        None => Ok(Keys::default()),
    }
}

pub fn write_keys(store: &Store, keys: &Keys) -> Result<()> {
    crate::market::vault::write(store, KEY_SLOT, &serde_json::to_string(keys)?)
}

/// Whether a key is saved for this profile already.
pub fn has_key(store: &Store, connection: &Connection, profile: &str) -> bool {
    keys(store)
        .is_ok_and(|k| k.profiles.get(&Keys::slot(&connection.id, profile)).is_some_and(|k| !k.trim().is_empty()))
}

/// The key one profile of a connection answers with.
pub fn key_for(store: &Store, connection: &Connection, profile: &str) -> Result<String> {
    let profile = if profile.is_empty() { connection.default_profile.as_str() } else { profile };
    keys(store)?
        .profiles
        .get(&Keys::slot(&connection.id, profile))
        .cloned()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| anyhow!("no key saved for profile `{profile}` of {}", connection.name))
}

/// A client for one profile of this connection: its own key, and the URL prefix that profile lives under.
pub fn client(store: &Store, connection: &Connection, profile: Option<&str>) -> Result<super::Client> {
    let profile = profile.unwrap_or(&connection.default_profile);
    let key = key_for(store, connection, profile)?;
    Ok(super::Client::new(connection, profile, key))
}

/// Checks a base URL the user typed: an http(s) origin with no path, no trailing slash.
pub fn parse_base(url: &str) -> Result<String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url).context("that isn't a URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("the URL must start with http:// or https://");
    }
    if parsed.host_str().is_none() {
        bail!("the URL needs a host");
    }
    if !parsed.path().trim_end_matches('/').is_empty() {
        bail!("use the gateway's base URL, without a path (profiles go under /p/<profile>)");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("the base URL can't carry a query or fragment");
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

/// A profile name Hermes accepts as a directory: lowercase letters, digits and `-`.
pub fn parse_profile(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        bail!("a profile name is required");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || name.starts_with('-') {
        bail!("profile names use lowercase letters, digits, `-` and `_`");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_urls_put_named_profiles_under_p() {
        let c = Connection {
            id: "c".into(),
            name: "VPS".into(),
            base_url: "http://100.101.122.109:8642".into(),
            default_profile: "default".into(),
            ssh: None,
            verified_at: None,
        };
        assert_eq!(c.profile_url("default"), "http://100.101.122.109:8642");
        assert_eq!(c.profile_url(""), "http://100.101.122.109:8642");
        assert_eq!(c.profile_url("codync-a"), "http://100.101.122.109:8642/p/codync-a");
    }

    #[test]
    fn base_urls_reject_paths_and_schemes() {
        assert_eq!(parse_base(" http://1.2.3.4:8642/ ").unwrap(), "http://1.2.3.4:8642");
        assert_eq!(parse_base("https://vps.example.com").unwrap(), "https://vps.example.com");
        assert!(parse_base("http://1.2.3.4:8642/p/codync-a").is_err(), "profiles aren't part of the base");
        assert!(parse_base("ftp://1.2.3.4").is_err());
        assert!(parse_base("not a url").is_err());
    }

    #[test]
    fn profile_names_are_lowercase_and_plain() {
        assert_eq!(parse_profile(" Codync-A ").unwrap(), "codync-a");
        assert!(parse_profile("../etc").is_err());
        assert!(parse_profile("").is_err());
    }
}
