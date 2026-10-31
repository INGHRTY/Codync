//! A bot's Codync tools over HTTP, for an agent that runs on another computer: a Hermes gateway's bot is given this
//! endpoint (`/mcp/<bot>`) and a token for that bot alone, so it sends messages, reaches its memory and asks other
//! bots, as a local one does. The tools are the local stdio servers' own (`super::describe` and `super::call`); only
//! the transport differs, since nothing on the gateway can start a process here.

use crate::hub::Hub;
use crate::mcp::{self, Server};
use serde_json::{Value, json};
use std::sync::Arc;

/// The token a bot's agent on another computer presents. Made on first use and kept, so the profile on the gateway
/// and this computer agree across restarts.
pub fn token(hub: &Hub, bot: &str) -> anyhow::Result<String> {
    let key = format!("mcp-bridge-token:{bot}");
    if let Some(token) = hub.store.kv_get(&key).filter(|t| !t.is_empty()) {
        return Ok(token);
    }
    let token = format!("cdcb_{}", crate::remote::crypto::b64(&crate::remote::crypto::random::<24>()));
    hub.store.kv_set(&key, &token)?;
    Ok(token)
}

/// Whether `given` is the bot's token. Every bot has its own, so a gateway holding one bot's token reaches only that
/// bot's tools.
pub fn authorized(hub: &Hub, bot: &str, given: &str) -> bool {
    let stored = hub.store.kv_get(&format!("mcp-bridge-token:{bot}")).unwrap_or_default();
    !stored.is_empty() && crate::remote::crypto::ct_eq(given.as_bytes(), stored.as_bytes())
}

/// The servers a bot on another computer gets: what it can do in Codync with no process here. `computer`,
/// `connectors` and `composio` stay on this computer, since each runs something here.
fn servers(bot: &crate::store::BotConfig) -> Vec<Server> {
    let mut servers = vec![Server::Chat, Server::Team, Server::Routines, Server::Memory];
    if bot.manage_team {
        servers.push(Server::Manage);
    }
    servers
}

/// Every tool the bot has, in one list, and the instructions that go with them.
fn catalog(bot: &crate::store::BotConfig) -> (Vec<Server>, Value, String) {
    let servers = servers(bot);
    let mut tools = Vec::new();
    let mut instructions = String::new();
    for server in &servers {
        let (_, about, list) = mcp::describe(*server);
        if let Some(list) = list.as_array() {
            tools.extend(list.iter().cloned());
        }
        if !about.is_empty() {
            instructions.push_str(about);
            instructions.push('\n');
        }
    }
    (servers, Value::Array(tools), instructions.trim_end().to_owned())
}

/// One JSON-RPC message from the bot's agent. `None` for a notification, which has no answer.
pub async fn handle(hub: &Arc<Hub>, bot: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").filter(|id| !id.is_null())?.clone();
    let params = &msg["params"];
    let reply = match msg["method"].as_str().unwrap_or_default() {
        "initialize" => match hub.store.bot(bot).ok().flatten() {
            Some(row) => {
                let (_, _, instructions) = catalog(&row.config);
                Ok(json!({
                    "protocolVersion": params["protocolVersion"].as_str().unwrap_or(mcp::PROTOCOL_VERSION),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "codync", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions,
                }))
            }
            None => Err(json!({"code": -32602, "message": format!("no bot `{bot}` on this computer")})),
        },
        "ping" => Ok(json!({})),
        "tools/list" => match hub.store.bot(bot).ok().flatten() {
            Some(row) => Ok(json!({"tools": catalog(&row.config).1})),
            None => Err(json!({"code": -32602, "message": format!("no bot `{bot}` on this computer")})),
        },
        "tools/call" => match hub.store.bot(bot).ok().flatten() {
            Some(row) => {
                let (servers, _, _) = catalog(&row.config);
                let name = params["name"].as_str().unwrap_or_default();
                match which(&servers, name) {
                    Some(server) => Ok(mcp::call(hub.port, &hub.token, bot, params, server).await),
                    None => Err(json!({"code": -32602, "message": format!("unknown tool {name}")})),
                }
            }
            None => Err(json!({"code": -32602, "message": format!("no bot `{bot}` on this computer")})),
        },
        method => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
    };
    Some(match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// The server a tool belongs to: its name is in that server's list.
fn which(servers: &[Server], name: &str) -> Option<Server> {
    servers.iter().copied().find(|server| {
        mcp::describe(*server)
            .2
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"].as_str() == Some(name)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bridge_bot_lists_every_codync_tool_once() {
        let bot: crate::store::BotConfig = serde_json::from_value(
            json!({"id": "b", "name": "B", "backend": "hermes", "cwd": "/w", "manageTeam": true}),
        )
        .unwrap();
        let (listed, tools, instructions) = catalog(&bot);
        assert!(listed.contains(&Server::Chat) && listed.contains(&Server::Manage));
        let names: Vec<&str> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"send_message"), "{names:?}");
        assert!(names.contains(&"update_bot"), "{names:?}");
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "no tool is listed twice");
        assert_ne!(instructions, "", "the agent reads what these tools are for");
        // A bot that can't manage the team isn't offered the tools for it: they'd only be refused.
        let plain: crate::store::BotConfig = serde_json::from_value(
            json!({"id": "b", "name": "B", "backend": "hermes", "cwd": "/w", "manageTeam": false}),
        )
        .unwrap();
        assert_eq!(which(&servers(&plain), "update_bot"), None);
        assert_eq!(which(&servers(&plain), "send_message"), Some(Server::Chat));
    }
}
