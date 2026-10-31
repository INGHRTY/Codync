//! Hermes gateway methods: the connections the user set up, and their profiles' keys.

use super::str_arg;
use crate::hub::Hub;
use crate::remote::hermes::{Client, Connection, connections};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "listConnections" => json!({"connections": connections::load(&hub.store)}),
        // `key` is the gateway's key for the profile in `profile` (its default profile when unset): saved to the
        // vault, never to the connection's own record. Leaving it out keeps the saved one.
        "saveConnection" => {
            crate::market::vault::unlock(hub.clone()).await?;
            let existing = b["id"].as_str().and_then(|id| connections::get(&hub.store, id));
            let id = existing.as_ref().map_or_else(|| uuid::Uuid::new_v4().to_string(), |c| c.id.clone());
            let name = match b["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
                Some(name) => name.to_owned(),
                None => existing.as_ref().map(|c| c.name.clone()).ok_or_else(|| anyhow!("name is required"))?,
            };
            let base_url = match b["baseUrl"].as_str().map(str::trim).filter(|u| !u.is_empty()) {
                Some(url) => connections::parse_base(url)?,
                None => existing.as_ref().map(|c| c.base_url.clone()).ok_or_else(|| anyhow!("baseUrl is required"))?,
            };
            let profile = match b["profile"].as_str().map(str::trim).filter(|p| !p.is_empty()) {
                Some(p) => connections::parse_profile(p)?,
                None => existing.as_ref().map_or_else(|| "default".to_owned(), |c| c.default_profile.clone()),
            };
            if let Some(key) = b["key"].as_str().map(str::trim).filter(|k| !k.is_empty()) {
                save_key(&hub.store, &id, &profile, key)?;
            }
            let ssh = match b["ssh"].as_str().map(str::trim) {
                Some(ssh) if !ssh.is_empty() => Some(ssh.to_owned()),
                // A connection being edited keeps its host unless the user clears it.
                Some(_) => None,
                None => existing.as_ref().and_then(|c| c.ssh.clone()),
            };
            let connection = Connection {
                id,
                name,
                base_url,
                default_profile: profile,
                ssh,
                verified_at: existing.and_then(|c| c.verified_at),
            };
            connections::upsert(&hub.store, connection.clone())?;
            emit(hub)?;
            json!({"connection": connection})
        }
        // The gateway answers this profile's key and can stream session chats.
        "testConnection" => {
            let (connection, profile, client) = probe(hub, &b).await?;
            let (caps, models) = check(&client).await?;
            if !connection.id.is_empty() {
                let mut all = connections::load(&hub.store);
                if let Some(c) = all.iter_mut().find(|c| c.id == connection.id) {
                    c.verified_at = Some(crate::store::now_ms());
                    connections::upsert(&hub.store, c.clone())?;
                }
                emit(hub)?;
            }
            json!({"profile": profile, "models": models, "features": caps["features"]})
        }
        "deleteConnection" => {
            let id = str_arg(&b, "id")?;
            let used =
                hub.store.bots()?.into_iter().any(|row| !row.deleted && row.config.connection.as_deref() == Some(id));
            if used {
                bail!("bots still run on this connection; move or delete them first");
            }
            connections::remove(&hub.store, id)?;
            emit(hub)?;
            json!({})
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

fn save_key(store: &crate::store::Store, connection: &str, profile: &str, key: &str) -> Result<()> {
    if key.len() < 16 {
        bail!("a Hermes gateway key is at least 16 characters");
    }
    let mut keys = connections::keys(store)?;
    keys.set(connection, profile, key);
    connections::write_keys(store, &keys)
}

/// What `testConnection` checks: a saved connection by `id`, its key from the vault; or one the user is typing
/// (`baseUrl`, `key`), whose key is used for this test alone and never saved.
async fn probe(hub: &Arc<Hub>, b: &Value) -> Result<(Connection, String, Client)> {
    if let Some(id) = b["id"].as_str().filter(|id| !id.is_empty()) {
        crate::market::vault::unlock(hub.clone()).await?;
        let c = connections::get(&hub.store, id).ok_or_else(|| anyhow!("unknown connection"))?;
        let profile = b["profile"].as_str().map_or_else(|| c.default_profile.clone(), str::to_owned);
        let profile = connections::parse_profile(&profile)?;
        let client = connections::client(&hub.store, &c, Some(&profile))?;
        return Ok((c, profile, client));
    }
    let base_url = connections::parse_base(str_arg(b, "baseUrl")?)?;
    let profile = b["profile"].as_str().map_or_else(|| Ok("default".to_owned()), connections::parse_profile)?;
    let key = str_arg(b, "key")?.trim().to_owned();
    if key.len() < 16 {
        bail!("a Hermes gateway key is at least 16 characters");
    }
    let c = Connection {
        id: String::new(),
        name: String::new(),
        base_url,
        default_profile: profile.clone(),
        ssh: None,
        verified_at: None,
    };
    let client = Client::new(&c, &profile, key);
    Ok((c, profile, client))
}

/// The gateway's capabilities and models, or why it didn't answer.
async fn check(client: &Client) -> Result<(Value, Vec<String>)> {
    let caps = client.capabilities().await.context("the gateway didn't accept this profile's key")?;
    let models = client.models().await.unwrap_or_default();
    Ok((caps, models))
}

/// Tells every client the connections changed (a whole list is small, so it travels as one).
fn emit(hub: &Hub) -> Result<()> {
    hub.emit_change(|| Ok(json!({"type": "connections", "connections": connections::load(&hub.store)})))
}
