//! A bot whose agent runs on a Hermes gateway (`remote::hermes`), as the actor drives it.
//!
//! The actor's turn loop is ACP's: a session, a prompt that answers when the turn is done, updates heard
//! meanwhile, and a cancel. A gateway is that same shape over HTTP — `POST /api/sessions/{id}/chat/stream`
//! streams a run's frames (`remote::hermes::events` turns them into the updates the actor already maps), the
//! run's last frame answers the prompt, and Stop and a card's answer go to `/v1/runs/{run}`.

use crate::LockExt;
use crate::agent::acp::{Failure, FailureKind, Incoming};
use crate::hub::Hub;
use crate::remote::hermes::events::Events;
use crate::remote::hermes::{self, Client};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// One gateway profile as the actor's agent.
pub(crate) struct Gateway {
    client: Client,
    /// The bot's name, which names its sessions there (Hermes refuses a repeated session title).
    title: String,
    pub(super) rx: mpsc::UnboundedReceiver<Incoming>,
    tx: mpsc::UnboundedSender<Incoming>,
    /// The run the running turn is on, once its first frame named it.
    run: Arc<Mutex<Option<String>>>,
    /// Stop was asked for: the stream stops the run as soon as it knows its id.
    stopping: Arc<AtomicBool>,
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
    pub(super) async fn connect(hub: &Arc<Hub>, cfg: &crate::store::BotConfig) -> Result<Self> {
        let store = &hub.store;
        let id = cfg.connection.as_deref().ok_or_else(|| anyhow!("this bot has no Hermes connection"))?;
        let connection = hermes::connections::get(store, id).ok_or_else(|| anyhow!("unknown Hermes connection"))?;
        let profile = hermes::profile_of(cfg);
        // One profile per bot, made on the gateway the first time it's needed (or its key saved by the user). Its
        // key lives in the vault, which this needs open.
        crate::market::vault::unlock(hub.clone()).await?;
        hermes::provision::ensure(store, &connection, &profile).await?;
        let client = hermes::connections::client(store, &connection, Some(&profile))?;
        client
            .capabilities()
            .await
            .with_context(|| format!("the Hermes gateway on {} (profile `{profile}`)", connection.name))?;
        let (tx, rx) = mpsc::unbounded_channel();
        Ok(Self {
            client,
            title: cfg.name.trim().to_owned(),
            rx,
            tx,
            run: Arc::new(Mutex::new(None)),
            stopping: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The bot's profile on the gateway.
    pub(super) fn profile(&self) -> &str {
        &self.title
    }

    /// Starts a turn and returns the answer that resolves when its run ends. `system` is the bot's instructions,
    /// which every turn carries: a gateway session has no system prompt of its own.
    pub(super) async fn send(
        &mut self,
        session: &str,
        message: &str,
        system: &str,
        model: Option<&str>,
    ) -> Result<Answer> {
        let mut body = json!({"message": message, "system_message": system});
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            body["model"] = model.into();
        }
        let mut frames = self.client.chat_stream(session, body).await?;
        *self.run.locked() = None;
        self.stopping.store(false, Ordering::Release);
        let (answer_tx, answer_rx) = oneshot::channel();
        let (tx, client) = (self.tx.clone(), self.client.clone());
        let (run, stopping) = (self.run.clone(), self.stopping.clone());
        let sid = session.to_owned();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut events = Events::new(tx, &sid);
            let mut answer: Result<Value> = Err(anyhow!("the run ended without a terminal event"));
            // A run whose stream stops right after its message is done still answered the turn.
            let mut message_done = false;
            while let Some(frame) = frames.next().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(e) => {
                        answer = Err(e);
                        break;
                    }
                };
                match frame.event.as_str() {
                    "run.started" => {
                        let Some(id) = frame.data["run_id"].as_str() else { continue };
                        *run.locked() = Some(id.to_owned());
                        if stopping.load(Ordering::Acquire) {
                            let _ = client.stop(id).await;
                        }
                    }
                    // A message's `assistant.completed` comes before the run's own end frame; only the latter
                    // answers the turn.
                    "assistant.completed" => message_done = true,
                    "run.completed" | "run.failed" | "run.cancelled" => {
                        answer = terminal(&frame.event, &frame.data);
                        break;
                    }
                    "error" => {
                        let message = frame.data["message"].as_str().unwrap_or("the run failed").to_owned();
                        answer = Err(anyhow!("{message}"));
                        break;
                    }
                    _ => events.frame(&frame.event, &frame.data),
                }
            }
            if message_done && answer.is_err() {
                // The stream closed before the run's end frame: the reply was delivered, so the turn stands.
                answer = Ok(json!({"stopReason": "end_turn"}));
            }
            let _ = answer_tx.send(answer);
        });
        Ok(Answer { rx: answer_rx })
    }

    /// Answers a card: the choice Hermes offered (`once`, `session`, `always`, `deny`).
    pub(super) async fn approve(&self, run: &str, choice: &str) -> Result<()> {
        self.client.approve(run, choice).await
    }

    /// Stops the running turn, now or as soon as its id is known.
    pub(super) async fn stop(&self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        let run = self.run.locked().clone();
        match run {
            Some(run) => self.client.stop(&run).await,
            None => Ok(()),
        }
    }
}

/// The answer a run's last frame gives the actor: ACP's `stopReason` in the shape it reads.
fn terminal(event: &str, d: &Value) -> Result<Value> {
    let reason = match event {
        "run.completed" => "end_turn",
        "run.cancelled" => "cancelled",
        _ => {
            let why = d["error"].as_str().or_else(|| d["turn_exit_reason"].as_str()).unwrap_or("the run failed");
            return Err(anyhow!("{why}"));
        }
    };
    Ok(json!({"stopReason": reason}))
}

/// A gateway session's answer to ACP's session verbs, so `session.rs` runs unchanged: a new session is a new
/// gateway session, a load is one the gateway still holds, and a model or Fast mode isn't a session option.
pub(super) async fn session_request(gateway: &Gateway, method: &str, params: &Value) -> Result<Value> {
    match method {
        "session/new" => Ok(json!({"sessionId": gateway.client.create_session(&title(gateway.profile())).await?})),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::hermes::Connection;
    use axum::Router;
    use axum::response::sse::{Event, Sse};
    use axum::routing::{get, post};
    use std::convert::Infallible;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn a_finished_run_ends_the_turn_and_a_failed_one_errors() {
        assert_eq!(terminal("run.completed", &json!({})).unwrap()["stopReason"], "end_turn");
        assert_eq!(terminal("run.cancelled", &json!({})).unwrap()["stopReason"], "cancelled");
        assert!(terminal("run.failed", &json!({"turn_exit_reason": "max_iterations_reached(60/60)"})).is_err());
    }

    #[test]
    fn session_titles_are_unique() {
        assert!(title("VPS Bot").starts_with("VPS Bot · "));
        assert_ne!(title("VPS Bot"), title("VPS Bot"));
    }

    /// A gateway that answers the session verbs and streams `frames` for a chat: its base URL and the calls it got.
    async fn fake_gateway(frames: Vec<(&'static str, Value)>) -> (String, Arc<StdMutex<Vec<String>>>) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let streamed: Vec<Result<Event, Infallible>> =
            frames.into_iter().map(|(event, data)| Ok(Event::default().event(event).data(data.to_string()))).collect();
        let app = Router::new()
            .route(
                "/v1/capabilities",
                get(|| async { axum::Json(json!({"features": {"session_chat_streaming": true}})) }),
            )
            .route("/api/sessions", post(|| async { axum::Json(json!({"session": {"id": "s1"}})) }))
            .route(
                "/api/sessions/{id}/chat/stream",
                post(move || async move { Sse::new(futures::stream::iter(streamed)) }),
            )
            .route(
                "/v1/runs/{id}/approval",
                post({
                    let calls = calls.clone();
                    move |axum::Json(b): axum::Json<Value>| {
                        let calls = calls.clone();
                        async move {
                            calls
                                .lock()
                                .unwrap()
                                .push(format!("approval {}", b["choice"].as_str().unwrap_or_default()));
                            axum::Json(json!({"resolved": 1}))
                        }
                    }
                }),
            )
            .route(
                "/v1/runs/{id}/stop",
                post({
                    let calls = calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            calls.lock().unwrap().push("stop".into());
                            axum::Json(json!({}))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    fn gateway(base: &str) -> Gateway {
        let connection = Connection {
            id: "c".into(),
            name: "VPS".into(),
            base_url: base.into(),
            default_profile: "default".into(),
            ssh: None,
            verified_at: None,
        };
        let (tx, rx) = mpsc::unbounded_channel();
        Gateway {
            client: Client::new(&connection, "default", "cdc_test_key_123456".into()),
            title: "Helper".into(),
            rx,
            tx,
            run: Arc::new(Mutex::new(None)),
            stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn a_run_streams_updates_and_ends_the_turn() {
        let (base, _) = fake_gateway(vec![
            ("run.started", json!({"run_id": "run_1"})),
            ("assistant.delta", json!({"delta": "Hel"})),
            ("assistant.delta", json!({"delta": "lo"})),
            ("assistant.completed", json!({"content": "Hello"})),
            ("run.completed", json!({"completed": true})),
        ])
        .await;
        let mut g = gateway(&base);
        let answer = g.send("s1", "hi", "be brief", None).await.unwrap();
        assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn");
        let mut text = String::new();
        while let Ok(Incoming::Notification { params, .. }) = g.rx.try_recv() {
            assert_eq!(params["sessionId"], "s1", "updates name the turn's session");
            if params["update"]["sessionUpdate"] == "agent_message_chunk" {
                text.push_str(params["update"]["content"]["text"].as_str().unwrap());
            }
        }
        assert_eq!(text, "Hello");
    }

    #[tokio::test]
    async fn a_stream_cut_after_the_message_still_ends_the_turn() {
        let (base, _) = fake_gateway(vec![
            ("run.started", json!({"run_id": "run_1"})),
            ("assistant.delta", json!({"delta": "Done"})),
            ("assistant.completed", json!({"content": "Done"})),
        ])
        .await;
        let mut g = gateway(&base);
        assert_eq!(g.send("s1", "hi", "", None).await.unwrap().response().await.unwrap()["stopReason"], "end_turn");
    }

    #[tokio::test]
    async fn a_failed_run_errors_with_its_reason() {
        let (base, _) = fake_gateway(vec![
            ("run.started", json!({"run_id": "run_1"})),
            ("run.failed", json!({"turn_exit_reason": "max_iterations_reached(60/60)"})),
        ])
        .await;
        let mut g = gateway(&base);
        let error = g.send("s1", "hi", "", None).await.unwrap().response().await.unwrap_err();
        assert!(error.to_string().contains("max_iterations_reached"), "{error}");
    }

    #[tokio::test]
    async fn a_card_is_answered_on_its_run_and_stop_reaches_the_gateway() {
        let (base, calls) = fake_gateway(vec![
            ("run.started", json!({"run_id": "run_1"})),
            ("approval.request", json!({"run_id": "run_1", "request_id": "r1", "command": "rm -rf x"})),
            ("run.completed", json!({})),
        ])
        .await;
        let mut g = gateway(&base);
        g.send("s1", "hi", "", None).await.unwrap().response().await.unwrap();
        g.approve("run_1", "once").await.unwrap();
        g.stop().await.unwrap();
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls, ["approval once", "stop"], "the card's choice and the stop go to the run");
    }
}
