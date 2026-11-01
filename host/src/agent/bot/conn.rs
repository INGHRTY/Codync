//! The agent one bot's actor drives: an ACP process on this computer, or a Hermes gateway's profile on another
//! one (`remote::hermes`).
//!
//! Both answer the verbs the actor's turn loop needs — make or load a session, send a prompt, take a cancel,
//! answer a permission — so the loop doesn't branch. What a transport can't do (a process to kill, a session
//! option) differs, and each says so in what it advertises.

use super::gateway::{self, Gateway};
use crate::agent::acp::{self, Acp, Incoming};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// What the agent can do with sessions besides creating them.
// Each flag is one capability the agent advertised at `initialize`, independent of the others.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub(crate) struct SessionCaps {
    /// `session/load`: resume a session after a restart.
    pub(crate) load: bool,
    /// `session/close`: an idle session can be released while the agent keeps running.
    pub(crate) close: bool,
    /// ACP `systemPrompt`: instructions on `session/new` and `load` that the agent keeps at system level.
    pub(crate) system_prompt: bool,
    /// ACP `additionalDirectories`: folders outside `cwd` the agent may work in, here the memory folders.
    pub(crate) additional_directories: bool,
}

/// Which adapter answered `initialize`, where Codync treats it apart from the others.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Adapter {
    /// claude-agent-acp: takes `_meta.systemPrompt` and `_meta.claudeCode.options`, and `/compact <instructions>`.
    Claude,
    /// codex-acp: `/compact` (its keep-list is in `CODEX_CONFIG`).
    Codex,
    Other,
}

/// How the bot's instructions (identity and memory) reach the agent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Instructions {
    /// Claude's adapter: `_meta.systemPrompt`, swapped in by loading the session again.
    ClaudeMeta,
    /// ACP `systemPrompt` (append), which the agent keeps across compaction; a gateway's `system_message`.
    SystemPrompt,
    /// A `<bot-profile>` block before the first message, sent again after a compaction.
    FirstMessage,
}

/// A prompt already written, waiting for its turn to end.
pub(crate) enum Answer {
    Acp(acp::Answer),
    Gateway(gateway::Answer),
}

impl Answer {
    pub(crate) async fn response(self) -> Result<Value> {
        match self {
            Self::Acp(a) => a.response().await,
            Self::Gateway(a) => a.response().await,
        }
    }
}

enum Transport {
    Acp { acp: Arc<Acp>, rx: mpsc::UnboundedReceiver<Incoming> },
    Gateway(Gateway),
}

/// The exact prompt a card answers, when the event named one (an authority gateway refuses without it).
fn request_of(id: &Value) -> Option<&str> {
    id["request"].as_str().filter(|request| !request.is_empty())
}

pub(crate) struct Conn {
    transport: Transport,
    pub(crate) sessions: SessionCaps,
    pub(crate) adapter: Adapter,
    /// Sessions already created or loaded in this process, and when each was last used.
    pub(crate) loaded: HashMap<String, Instant>,
    pub(crate) started: Instant,
    /// A `session/new` or `session/load` is waiting for its answer. Still set when a routine's time limit cut it short:
    /// the agent may then open that session, tools and all, under an id the host never learns (`drop_unanswered`).
    pub(crate) opening: bool,
    /// Each live session's Fast mode option, as the agent last listed it.
    pub(crate) fast_options: HashMap<String, Value>,
    /// The agent reports compactions (`compaction_update`); until it does, a fall in usage stands in for them.
    pub(crate) reports_compaction: bool,
    /// Each session's context use and window size at its last `usage_update`.
    pub(crate) usage: HashMap<String, (u64, u64)>,
}

impl Conn {
    /// An agent running as a process here, after its `initialize` answer.
    pub(crate) fn acp(acp: Arc<Acp>, rx: mpsc::UnboundedReceiver<Incoming>, init: &Value) -> Self {
        Self {
            transport: Transport::Acp { acp, rx },
            sessions: SessionCaps {
                load: init["agentCapabilities"]["loadSession"].as_bool().unwrap_or(false),
                close: init["agentCapabilities"]["sessionCapabilities"]["close"].is_object(),
                system_prompt: init["agentCapabilities"]["sessionCapabilities"]["systemPrompt"].is_object(),
                additional_directories: init["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"]
                    .is_object(),
            },
            adapter: super::launch::adapter(init),
            loaded: HashMap::new(),
            started: Instant::now(),
            opening: false,
            fast_options: HashMap::new(),
            reports_compaction: false,
            usage: HashMap::new(),
        }
    }

    /// A Hermes gateway's profile: its sessions are durable on that computer, and it has no per-session options.
    pub(crate) fn gateway(gateway: Gateway) -> Self {
        Self {
            transport: Transport::Gateway(gateway),
            // `system_prompt`: the instructions travel as the turn's own `system_message`, never as a profile block
            // in the message (see `turn.rs`).
            sessions: SessionCaps { load: true, close: false, system_prompt: true, additional_directories: false },
            adapter: Adapter::Other,
            loaded: HashMap::new(),
            started: Instant::now(),
            opening: false,
            fast_options: HashMap::new(),
            reports_compaction: false,
            usage: HashMap::new(),
        }
    }

    /// Whether this agent is a Hermes gateway, whose turn carries its instructions itself.
    pub(crate) fn is_gateway(&self) -> bool {
        matches!(self.transport, Transport::Gateway(_))
    }

    pub(crate) fn instructions(&self) -> Instructions {
        // Claude's adapter rebuilds the standard field without the `_meta` object's options, so Claude stays on it.
        if self.adapter == Adapter::Claude {
            Instructions::ClaudeMeta
        } else if self.sessions.system_prompt {
            Instructions::SystemPrompt
        } else {
            Instructions::FirstMessage
        }
    }

    /// The process leader, for the idle check that keeps a dev server running; a gateway has no process here.
    pub(crate) fn pid(&self) -> Option<u32> {
        match &self.transport {
            Transport::Acp { acp, .. } => acp.pid(),
            Transport::Gateway(_) => None,
        }
    }

    /// The gateway run the current turn is on, which the host records to settle it after a restart.
    pub(crate) fn current_run(&self) -> Option<String> {
        match &self.transport {
            Transport::Gateway(g) => g.current_run(),
            Transport::Acp { .. } => None,
        }
    }

    /// Settles a run the previous host process started (see `Gateway::settle`); an ACP turn has no such run.
    pub(crate) async fn settle_run(&self, run: &str) -> Result<()> {
        match &self.transport {
            Transport::Gateway(g) => g.settle(run).await,
            Transport::Acp { .. } => Ok(()),
        }
    }

    /// Stops the agent and what it started. A gateway's sessions are durable there, so nothing stops.
    pub(crate) async fn kill(&self) {
        if let Transport::Acp { acp, .. } = &self.transport {
            acp.kill().await;
        }
    }

    /// The next thing the agent says, waiting for it when there is nothing yet.
    pub(crate) async fn recv(&mut self) -> Option<Incoming> {
        match &mut self.transport {
            Transport::Acp { rx, .. } => rx.recv().await,
            Transport::Gateway(g) => g.rx.recv().await,
        }
    }

    pub(crate) fn try_recv(&mut self) -> Option<Incoming> {
        match &mut self.transport {
            Transport::Acp { rx, .. } => rx.try_recv().ok(),
            Transport::Gateway(g) => g.rx.try_recv().ok(),
        }
    }

    /// Writes a request and returns its pending answer; a gateway turn starts here.
    pub(crate) async fn send(&mut self, method: &str, params: Value) -> Result<Answer> {
        match &mut self.transport {
            Transport::Acp { acp, .. } => Ok(Answer::Acp(acp.send(method, params).await?)),
            Transport::Gateway(g) => {
                if method != "session/prompt" {
                    bail!("the Hermes gateway has no {method}");
                }
                let session =
                    params["sessionId"].as_str().ok_or_else(|| anyhow!("session/prompt needs a sessionId"))?;
                let message = gateway::chat_message(&params["prompt"]);
                // Hermes' own field for a turn's instructions; the ACP `systemPrompt` shape never reaches here.
                let system = params["instructions"].as_str().unwrap_or_default().to_owned();
                let model = params["model"].as_str().map(str::to_owned);
                Ok(Answer::Gateway(g.send(session, &message, &system, model.as_deref()).await?))
            }
        }
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match &mut self.transport {
            Transport::Acp { acp, .. } => acp.request(method, params).await,
            Transport::Gateway(g) => gateway::session_request(g, method, &params).await,
        }
    }

    /// Answers a request the agent made: a permission card's choice.
    pub(crate) async fn respond(&self, id: Value, result: Value) -> Result<()> {
        match &self.transport {
            Transport::Acp { acp, .. } => acp.respond(id, result).await,
            Transport::Gateway(g) => {
                let run = id["run"].as_str().unwrap_or_default();
                let choice = result["outcome"]["optionId"].as_str().unwrap_or("deny");
                g.approve(run, choice, request_of(&id), id["generation"].as_u64()).await
            }
        }
    }

    pub(crate) async fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<()> {
        match &self.transport {
            Transport::Acp { acp, .. } => acp.respond_error(id, code, message).await,
            // A card the user dismissed: Hermes resolves a run's approval with a deny.
            Transport::Gateway(g) => match id["run"].as_str() {
                Some(run) if !run.is_empty() => {
                    g.approve(run, "deny", request_of(&id), id["generation"].as_u64()).await
                }
                _ => Ok(()),
            },
        }
    }

    /// A notification to the agent: `session/cancel` is the gateway's Stop.
    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<()> {
        match &self.transport {
            Transport::Acp { acp, .. } => acp.notify(method, params).await,
            Transport::Gateway(g) => {
                if method != "session/cancel" {
                    bail!("the Hermes gateway has no {method}");
                }
                g.stop().await
            }
        }
    }
}
