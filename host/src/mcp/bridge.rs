//! A bot's Codync tools over HTTP, for an agent that runs on another computer: a Hermes gateway's bot is given this
//! endpoint (`/mcp/<bot>`) and a token for that bot alone, so it sends messages, reaches its memory and asks other
//! bots, as a local one does. The tools are the local stdio servers' own (`super::describe` and `super::call`); only
//! the transport differs, since nothing on the gateway can start a process here.

use crate::hub::Hub;
use crate::mcp::{self, Server};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Tools a bot on another computer isn't offered: they read and write files of this computer's, while its agent
/// works on the gateway's, so they would fail — or, given a path of its own, hand this computer's files out.
const THIS_COMPUTERS_FILES: &[&str] = &["send_file", "publish_artifact", "list_artifacts"];

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

/// Whether `given` is the token of the bot `bot`, which must still exist: every bot has its own, so a gateway
/// holding one bot's token reaches only that bot's tools, and a deleted bot's token reaches nothing.
pub fn authorized(hub: &Hub, bot: &str, given: &str) -> bool {
    let stored = hub.store.kv_get(&format!("mcp-bridge-token:{bot}")).unwrap_or_default();
    !stored.is_empty()
        && crate::remote::crypto::ct_eq(given.as_bytes(), stored.as_bytes())
        && live_bot(hub, bot).is_ok()
}

/// Whether this computer still has the bot `id`.
fn live_bot(hub: &Hub, id: &str) -> Result<(), Value> {
    match hub.store.bot(id) {
        Ok(Some(row)) if !row.deleted => Ok(()),
        Ok(_) => Err(json!({"code": -32602, "message": format!("no bot `{id}` on this computer")})),
        Err(e) => Err(json!({"code": -32603, "message": format!("{e:#}")})),
    }
}

/// The servers a bot on another computer gets: what it can do in Codync with no process here. `computer`,
/// `connectors` and `composio` stay on this computer, since each runs something here, and a bot on a gateway can't
/// manage this computer's bots (the host refuses that when one is made), so `manage` stays too.
fn servers() -> [Server; 4] {
    [Server::Chat, Server::Team, Server::Routines, Server::Memory]
}

/// Every tool the bot has, its instructions, and which server each tool belongs to.
struct Catalog {
    tools: Value,
    instructions: String,
    owners: HashMap<String, Server>,
}

impl Catalog {
    fn of() -> Self {
        let (mut tools, mut instructions, mut owners) = (Vec::new(), String::new(), HashMap::new());
        for server in servers() {
            let (_, about, list) = mcp::describe(server);
            if !about.is_empty() {
                instructions.push_str(about);
                instructions.push('\n');
            }
            for tool in list.as_array().into_iter().flatten() {
                let Some(name) = tool["name"].as_str() else { continue };
                if server == Server::Chat && THIS_COMPUTERS_FILES.contains(&name) {
                    continue;
                }
                owners.insert(name.to_owned(), server);
                tools.push(tool.clone());
            }
        }
        Self { tools: Value::Array(tools), instructions: instructions.trim_end().to_owned(), owners }
    }
}

/// One JSON-RPC message from the bot's agent. `None` for a notification, which has no answer.
pub async fn handle(hub: &Arc<Hub>, bot: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").filter(|id| !id.is_null())?.clone();
    let params = &msg["params"];
    let reply = match msg["method"].as_str().unwrap_or_default() {
        // The one request that needs no bot: a computer checking it can reach this one.
        "ping" => Ok(json!({})),
        "initialize" => live_bot(hub, bot).map(|()| {
            json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or(mcp::PROTOCOL_VERSION),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "codync", "version": env!("CARGO_PKG_VERSION")},
                "instructions": Catalog::of().instructions,
            })
        }),
        "tools/list" => live_bot(hub, bot).map(|()| json!({"tools": Catalog::of().tools})),
        "tools/call" => {
            // The tool's owner, once the bot is known to be here: `Server` holds no borrow of the catalog.
            let route = live_bot(hub, bot).and_then(|()| {
                let owners = Catalog::of().owners;
                let name = params["name"].as_str().unwrap_or_default();
                owners
                    .get(name)
                    .copied()
                    .ok_or_else(|| json!({"code": -32602, "message": format!("unknown tool {name}")}))
            });
            match route {
                Ok(server) => Ok(mcp::call(hub.port, &hub.token, bot, params, server).await),
                Err(e) => Err(e),
            }
        }
        method => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
    };
    Some(match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hub on an in-memory store, with its identity in a temp folder the test removes.
    fn hub() -> (Arc<Hub>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("codync-bridge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let hub = Hub::new(
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("store"),
            "test".into(),
            crate::remote::identity::Identity::load_or_create(&dir).expect("identity"),
            "test".into(),
            19222,
        );
        (hub, dir)
    }

    fn add_bot(hub: &Hub, id: &str) {
        let mut cfg: crate::store::BotConfig =
            serde_json::from_value(json!({"id": id, "name": "B", "backend": "hermes", "cwd": "/w"})).unwrap();
        cfg.connection = Some("c".into());
        hub.store.save_bot(&cfg).expect("bot");
    }

    #[test]
    fn a_bridge_bot_lists_every_codync_tool_once() {
        let tools = Catalog::of().tools;
        let names: Vec<&str> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"send_message"), "{names:?}");
        assert!(names.contains(&"search_memory"), "{names:?}");
        assert!(names.contains(&"message_bot"), "{names:?}");
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "no tool is listed twice");
        // These read this computer's files, which the bot's agent can't see from the gateway.
        for local in THIS_COMPUTERS_FILES {
            assert!(!names.contains(local), "{local} would read this computer's files: {names:?}");
        }
        assert_ne!(Catalog::of().instructions, "", "the agent reads what these tools are for");
    }

    #[tokio::test]
    async fn a_token_answers_for_its_own_live_bot_alone() {
        let (hub, dir) = hub();
        add_bot(&hub, "a");
        add_bot(&hub, "b");
        let token = token(&hub, "a").expect("a token");
        assert!(authorized(&hub, "a", &token));
        assert!(!authorized(&hub, "b", &token), "another bot's path refuses it");
        assert!(!authorized(&hub, "a", "cdcb_wrong"));
        assert!(!authorized(&hub, "nobody", &token), "no such bot");
        // A deleted bot's token reaches nothing, even before its token is cleared.
        hub.store.delete_bot("a").expect("delete");
        assert!(!authorized(&hub, "a", &token));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_request_from_a_deleted_bot_is_refused_and_notifications_are_not_answered() {
        let (hub, dir) = hub();
        add_bot(&hub, "a");
        hub.store.delete_bot("a").expect("delete");
        let ask = |method: &str| json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
        let error = handle(&hub, "a", &ask("tools/list")).await.expect("an answer")["error"].clone();
        assert!(error["message"].as_str().unwrap().contains("no bot `a`"), "{error}");
        // A notification (no id) is accepted with nothing to say, and an unknown method says so.
        assert!(handle(&hub, "a", &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await.is_none());
        let unknown = handle(&hub, "a", &ask("resources/list")).await.expect("an answer");
        assert_eq!(unknown["error"]["code"], -32601);
        // A reachability check answers whatever the bot's state.
        let pong = handle(&hub, "a", &ask("ping")).await.expect("an answer");
        assert_eq!(pong["result"], json!({}));
        let _ = std::fs::remove_dir_all(dir);
    }
}
