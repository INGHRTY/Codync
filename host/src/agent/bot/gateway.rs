//! A bot whose agent runs on a Hermes gateway (`remote::hermes`), as the actor drives it.
//!
//! The actor's turn loop is ACP's: a session, a prompt that answers when the turn is done, updates heard
//! meanwhile, and a cancel. A gateway is that same shape over HTTP — a turn is a **run**: `POST /v1/runs`
//! starts it and names it, `GET /v1/runs/{run}/events` streams its frames (`remote::hermes::events` turns
//! them into the updates the actor already maps), and its last frame answers the prompt. The run belongs to
//! the gateway, not to the stream: a dropped connection is reattached from the last frame applied, and a run
//! still going (or one the gateway finished while we were away) is read from `GET /v1/runs/{run}`. Stop and a
//! card's answer go to the run itself, and the run is recorded so a host restart settles it.

use crate::LockExt;
use crate::agent::acp::{self, Failure, FailureKind, Incoming};
use crate::hub::Hub;
use crate::remote::hermes::events::{self, Events};
use crate::remote::hermes::{self, Client};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// One gateway profile as the actor's agent.
pub(crate) struct Gateway {
    client: Client,
    /// The bot's name, which names its sessions there (Hermes refuses a repeated session title).
    title: String,
    pub(super) rx: mpsc::UnboundedReceiver<Incoming>,
    tx: mpsc::UnboundedSender<Incoming>,
    /// The run the running turn is on, which its start names before anything is streamed.
    run: Arc<Mutex<Option<String>>>,
    /// The running turn's follow task, which waits for the run while the gateway still holds it.
    follow: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        // A turn whose agent goes away stops waiting for its run; the run itself stays the gateway's.
        if let Some(task) = self.follow.locked().take() {
            task.abort();
        }
    }
}

/// A turn started on the gateway, waiting for its run to end.
pub(crate) struct Answer {
    rx: oneshot::Receiver<Result<Value>>,
}

impl Answer {
    pub(super) async fn response(self) -> Result<Value> {
        self.rx.await.unwrap_or_else(|_| Err(Failure::error(FailureKind::Exited, "the gateway's run was lost")))
    }
}

impl Gateway {
    /// Connects to the profile `cfg` runs as, so a wrong key or a gateway that's down fails before a turn waits.
    /// `Some` beside it is something the chat should say: the bot's Codync tools couldn't be set up there.
    pub(super) async fn connect(hub: &Arc<Hub>, cfg: &crate::store::BotConfig) -> Result<(Self, Option<String>)> {
        let store = &hub.store;
        let id = cfg.connection.as_deref().ok_or_else(|| anyhow!("this bot has no Hermes connection"))?;
        let connection = hermes::connections::get(store, id).ok_or_else(|| anyhow!("unknown Hermes connection"))?;
        let profile = hermes::profile_of(cfg);
        // One profile per bot, made on the gateway the first time it's needed (or its key saved by the user), with
        // the bot's Codync tools. Its key lives in the vault, which this needs open.
        crate::market::vault::unlock(hub.clone()).await?;
        let warning = hermes::provision::ensure(hub, &connection, &profile, &cfg.id).await?;
        let client = hermes::connections::client(store, &connection, Some(&profile))?;
        client
            .capabilities()
            .await
            .with_context(|| format!("the Hermes gateway on {} (profile `{profile}`)", connection.name))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let gateway = Self {
            client,
            title: cfg.name.trim().to_owned(),
            rx,
            tx,
            run: Arc::new(Mutex::new(None)),
            follow: Mutex::new(None),
        };
        Ok((gateway, warning))
    }

    /// The bot's name, which names its sessions there.
    pub(super) fn name(&self) -> &str {
        &self.title
    }

    /// Starts a turn and returns the answer that resolves when its run ends. `message` is Hermes' `input`
    /// ([`chat_message`]); `system` is the bot's instructions, which every turn carries, since a gateway
    /// session has no system prompt of its own.
    pub(super) async fn send(
        &mut self,
        session: &str,
        message: &Value,
        system: &str,
        model: Option<&str>,
    ) -> Result<Answer> {
        let run = self.client.start_run(session, message, system, model).await?;
        *self.run.locked() = Some(run.clone());
        let (answer_tx, answer_rx) = oneshot::channel();
        let (tx, client) = (self.tx.clone(), self.client.clone());
        let sid = session.to_owned();
        let task = tokio::spawn(async move {
            let answer = follow(&client, &run, &sid, &tx).await;
            let _ = answer_tx.send(answer);
        });
        *self.follow.locked() = Some(task);
        Ok(Answer { rx: answer_rx })
    }

    /// Answers a card: the choice Hermes offered (`once`, `session`, `always`, `deny`), with the exact prompt
    /// and generation when the event named them (a gateway that holds the run's authority refuses otherwise).
    pub(super) async fn approve(
        &self,
        run: &str,
        choice: &str,
        request: Option<&str>,
        generation: Option<u64>,
    ) -> Result<()> {
        self.client.approve(run, choice, request, generation).await
    }

    /// Stops the running turn: the run is known from the moment it started. When the gateway can't be told (or
    /// heard), this host stops waiting for it rather than hold the turn while the link is down.
    pub(super) async fn stop(&self) -> Result<()> {
        let Some(run) = self.run.locked().clone() else { return Ok(()) };
        let result = self.client.stop(&run).await;
        if result.is_err()
            && let Some(task) = self.follow.locked().take()
        {
            task.abort();
        }
        result
    }

    /// The run the current turn is on, which a host restart settles rather than leaving orphaned.
    pub(super) fn current_run(&self) -> Option<String> {
        self.run.locked().clone()
    }

    /// Stops a run an earlier host process started when the gateway still holds it: its answer can't be
    /// shown here, and a resumed turn must not queue behind work already running in the same session.
    pub(super) async fn settle(&self, run: &str) -> Result<()> {
        let status = self.client.run_status(run).await?;
        if is_terminal(&status) { Ok(()) } else { self.client.stop(run).await }
    }
}

/// Follows one run to its end: its events until a terminal frame, reattaching a stream that drops (the
/// gateway replays what was missed), and asking the run's own status whenever no stream answers. A run the
/// gateway still holds (running, queued, waiting for an approval) keeps executing there, so it is waited
/// for rather than reported lost; only a terminal status answers or fails the turn.
async fn follow(client: &Client, run: &str, session: &str, tx: &mpsc::UnboundedSender<Incoming>) -> Result<Value> {
    let mut events = Events::new(tx.clone(), session);
    // The last frame applied, so a reattach continues instead of replaying the reply.
    let mut after: Option<u64> = None;
    // Time with neither a stream nor a status answering; an answer from either resets it.
    let mut unanswered = Instant::now();
    let mut pause = REATTACH_PAUSE;
    let mut lost: Option<anyhow::Error> = None;
    loop {
        match stream_once(client, run, &mut after, session, &mut events).await {
            Attach::Answered(answer) => return answer,
            Attach::Ended { progressed } => {
                // A stream that delivered frames reattaches straight away; one that keeps ending empty backs off.
                if progressed {
                    unanswered = Instant::now();
                    pause = REATTACH_PAUSE;
                }
            }
            Attach::Refused(error) => lost = Some(error),
        }
        // With no stream, the run's status is the word on whether it is still going or already ended.
        match client.run_status(run).await {
            Ok(status) if is_terminal(&status) => return recovered(&status, &mut events),
            Ok(_) => unanswered = Instant::now(),
            Err(error) => lost = Some(error),
        }
        if unanswered.elapsed() >= RECONNECT_LIMIT {
            return Err(lost.unwrap_or_else(|| anyhow!("the gateway never answered for run {run}")).context(format!(
                "the gateway's connection dropped and run {run} can't be read back; its turn may still be going there"
            )));
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(REATTACH_MAX_PAUSE);
    }
}

/// What one attach of a run's stream did.
enum Attach {
    /// A terminal frame answered the turn.
    Answered(Result<Value>),
    /// A stream attached and ended (or broke) without a terminal frame; the run may still be going.
    Ended { progressed: bool },
    /// The gateway refused the attachment itself, so nothing was read.
    Refused(anyhow::Error),
}

/// Streams one attach of a run and applies its frames. A terminal frame answers the turn; anything less ends
/// the attach, and the run itself carries on at the gateway.
async fn stream_once(
    client: &Client,
    run: &str,
    after: &mut Option<u64>,
    session: &str,
    events: &mut Events,
) -> Attach {
    use futures::StreamExt;
    let mut frames = match client.run_events(run, *after).await {
        Ok(frames) => frames,
        Err(error) => return Attach::Refused(error),
    };
    let mut progressed = false;
    while let Some(Ok(frame)) = frames.next().await {
        progressed = true;
        if let Some(seq) = frame.data["seq"].as_u64() {
            *after = Some(seq);
        }
        match frame.event.as_str() {
            "run.completed" | "run.failed" | "run.cancelled" | "run.interrupted" => {
                return Attach::Answered(terminal(&frame.event, &frame.data, events));
            }
            "error" => {
                let message = frame.data["message"].as_str().unwrap_or("the run failed").to_owned();
                return Attach::Answered(Err(anyhow!("{message}")));
            }
            // A run admitted behind another one ends its stream here; its status below says whether it goes on.
            "run.queued" => {}
            _ => {
                events.frame(&frame.event, &frame.data);
                // The run stream caps a tool result's preview, so a plan is read whole from the session.
                if events::is_plan(&frame.event, &frame.data) {
                    let result = client.todo_result(session, frame.data["tool_call_id"].as_str()).await;
                    if let Err(error) = &result {
                        tracing::debug!(
                            error = format!("{error:#}"),
                            "couldn't read the plan from the session's transcript; the run stream's preview stands in"
                        );
                    }
                    events.plan(result.ok().flatten(), &frame.data);
                }
            }
        }
    }
    Attach::Ended { progressed }
}

/// How quick a reattach starts and the longest it backs off to between tries.
const REATTACH_PAUSE: Duration = Duration::from_millis(400);
const REATTACH_MAX_PAUSE: Duration = Duration::from_secs(5);
/// How long a run may go unobserved (neither a stream nor a status answering) before its turn is reported lost.
const RECONNECT_LIMIT: Duration = Duration::from_secs(600);

/// The states a run never leaves.
const TERMINAL: [&str; 4] = ["completed", "failed", "cancelled", "interrupted"];

fn is_terminal(status: &Value) -> bool {
    status["status"].as_str().is_some_and(|state| TERMINAL.contains(&state))
}

/// The answer a run's last frame gives the actor: ACP's `stopReason` in the shape it reads.
fn terminal(event: &str, d: &Value, events: &mut Events) -> Result<Value> {
    answer(event.strip_prefix("run.").unwrap_or(event), d, events)
}

/// The answer a run in a terminal state gives the actor, from its last frame or its pollable status.
fn answer(state: &str, d: &Value, events: &mut Events) -> Result<Value> {
    match state {
        "completed" => {
            events.finish(d["output"].as_str());
            Ok(json!({"stopReason": "end_turn"}))
        }
        "cancelled" => Ok(json!({"stopReason": "cancelled"})),
        "interrupted" => {
            Err(anyhow!("{}", d["error"].as_str().unwrap_or("the gateway shut down before the run finished")))
        }
        _ => Err(anyhow!("{}", failure(d))),
    }
}

/// The failure a run reports, in Hermes' own words.
fn failure(d: &Value) -> String {
    d["error"]
        .as_str()
        .or_else(|| d["reason"].as_str())
        .filter(|why| !why.is_empty())
        .map_or_else(|| d["turn_exit_reason"].as_str().unwrap_or("the run failed").to_owned(), str::to_owned)
}

/// The run's pollable status as the actor's answer, when its stream was lost: completed reports the reply it
/// delivered, a failed or cancelled run its reason.
fn recovered(status: &Value, events: &mut Events) -> Result<Value> {
    answer(status["status"].as_str().unwrap_or_default(), status, events)
}

/// The turn's ACP prompt as Hermes' `message`: its text, and the images the turn carries as `image_url` data
/// URLs (the only attachments a gateway takes in a request). A turn without images stays plain text.
pub(super) fn chat_message(prompt: &Value) -> Value {
    let text = acp::content_text(prompt);
    let images: Vec<Value> = prompt
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|b| b["type"].as_str() == Some("image"))
        .filter_map(|b| {
            let data = b["data"].as_str().filter(|d| !d.is_empty())?;
            let mime = b["mimeType"].as_str().unwrap_or("image/png");
            Some(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}))
        })
        .collect();
    if images.is_empty() {
        return Value::String(text);
    }
    let mut parts = vec![json!({"type": "text", "text": text})];
    parts.extend(images);
    Value::Array(parts)
}

/// A gateway session's answer to ACP's session verbs, so `session.rs` runs unchanged: a new session is a new
/// gateway session, a load is one the gateway still holds, and a model or Fast mode isn't a session option.
pub(super) async fn session_request(gateway: &Gateway, method: &str, params: &Value) -> Result<Value> {
    match method {
        "session/new" => Ok(json!({"sessionId": gateway.client.create_session(&title(gateway.name())).await?})),
        "session/load" => match params["sessionId"].as_str() {
            Some(sid) => Ok(json!({"sessionId": sid})),
            None => bail!("session/load needs a sessionId"),
        },
        "session/set_config_option" | "session/set_model" => Ok(json!({"note": "the gateway sets its model per turn"})),
        other => bail!("the Hermes gateway has no {other}"),
    }
}

/// The session title a bot's new gateway session gets; Hermes refuses a title it already has.
pub(super) fn title(bot: &str) -> String {
    format!("{bot} · {}", uuid::Uuid::new_v4())
}

/// The kv entry naming the run a resumable turn is on, so the next host process can settle it.
pub(super) fn run_key(bot: &str) -> String {
    format!("turn.run.{bot}")
}

#[cfg(test)]
mod tests;
