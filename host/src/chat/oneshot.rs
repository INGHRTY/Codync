//! Background jobs on the bot's own harness (dreaming, automatic naming): one prompt to a throwaway agent.

use crate::agent::acp::{self, Incoming};
use crate::agent::bot::{Adapter, Conn, Instructions};
use crate::hub::Hub;
use crate::store::BotConfig;
use anyhow::{Result, anyhow, bail};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_mins(3);

/// Runs one prompt on a throwaway agent of the bot's harness and returns its reply.
/// Claude gets a real system prompt, no tools, no settings and no saved session; an agent with ACP
/// `systemPrompt` gets the instructions there, other harnesses inline. None of them may remember the job.
pub async fn one_shot(hub: &Arc<Hub>, cfg: &BotConfig, system: &str, user: &str) -> Result<String> {
    let cwd = crate::service::data_dir().join("background");
    tokio::fs::create_dir_all(&cwd).await?;
    let cwd = cwd.to_string_lossy().into_owned();
    if hub.store.kv_read(&format!("agent-env:{}", cfg.backend))?.is_some() {
        crate::market::vault::unlock(hub.clone()).await?;
    }
    let env = crate::agent::auth::launch_env(&hub.store, &cfg.backend)?;
    let mut conn = None;
    let mut last_err = None;
    // A Hermes bot's own profile, when this computer has it: the provider and skills it runs with. Never made here.
    let profile = (cfg.backend == "hermes").then(|| crate::agent::hermes_profile::name(cfg));
    let profile = profile.filter(|profile| crate::agent::hermes_profile::exists(profile));
    for candidate in crate::agent::bot::launch_commands(cfg, profile.as_deref())? {
        let started = async {
            let mut command = [candidate.resolve(|_| {}).await?.acp()];
            let mut env = env.clone();
            crate::agent::own_memory::turn_off(&cfg.backend, &cwd, &mut env, &mut command).await;
            crate::agent::bot::start_agent(&command[0], &cwd, &env, Duration::from_mins(1)).await
        };
        match started.await {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let mut conn = conn.ok_or_else(|| last_err.unwrap_or_else(|| anyhow!("no agent to run a background job")))?;
    let result = async {
        let mut params = json!({"cwd": cwd, "mcpServers": []});
        let text = match conn.instructions() {
            Instructions::ClaudeMeta => {
                params["_meta"] = json!({
                    "systemPrompt": system,
                    "claudeCode": {"options": {"tools": [], "persistSession": false, "settingSources": [], "model": "haiku"}},
                });
                user.to_owned()
            }
            Instructions::SystemPrompt => {
                params["systemPrompt"] = crate::agent::bot::system_prompt(system);
                user.to_owned()
            }
            Instructions::FirstMessage => format!("{system}\n\n---\n\n{user}"),
        };
        let res = conn.request("session/new", params).await?;
        let sid = res["sessionId"].as_str().ok_or_else(|| anyhow!("agent returned no sessionId"))?.to_owned();
        // Same model as the bot: the harness default may be another (local) model that
        // would load next to the bot's, or a cloud one the user kept these chats away from.
        if conn.adapter != Adapter::Claude {
            crate::agent::bot::select_model(&mut conn, &res, &sid, cfg).await?;
        }
        // Started, not awaited: the answer is owned, so the frames can be read meanwhile.
        let answer =
            conn.send("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]})).await?;
        let prompt = answer.response();
        tokio::pin!(prompt);
        let deadline = tokio::time::sleep(TIMEOUT);
        tokio::pin!(deadline);
        let mut out = String::new();
        loop {
            let inc = tokio::select! {
                r = &mut prompt => { r.and_then(acp::codebuddy::prompt_result)?; break }
                inc = conn.recv() => inc,
                () = &mut deadline => bail!("the background job timed out"),
            };
            if !collect(&conn, inc, &mut out).await {
                bail!("the background job's agent exited");
            }
        }
        while let Some(inc) = conn.try_recv() {
            collect(&conn, Some(inc), &mut out).await;
        }
        Ok(out)
    }
    .await;
    conn.kill().await;
    result
}

/// Keeps reply text; refuses anything the agent asks of us. False once the agent is gone.
async fn collect(conn: &Conn, inc: Option<Incoming>, out: &mut String) -> bool {
    match inc {
        None | Some(Incoming::Closed { .. }) => false,
        Some(Incoming::Notification { method, params }) => {
            let u = &params["update"];
            if method == "session/update" && u["sessionUpdate"] == "agent_message_chunk" {
                out.push_str(&acp::content_text(&u["content"]));
            }
            true
        }
        Some(Incoming::Request { id, method, .. }) => {
            let _ = if method == "session/request_permission" {
                conn.respond(id, json!({"outcome": {"outcome": "cancelled"}})).await
            } else {
                conn.respond_error(id, -32601, "not supported").await
            };
            true
        }
    }
}
