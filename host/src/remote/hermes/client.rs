//! Talking to a Hermes gateway's HTTP API: sessions, the SSE stream of a run, approvals, models and
//! capability checks.
//!
//! The gateway is the user's own VPS. Every route exists both un-prefixed (the gateway's default profile)
//! and under `/p/<profile>/`, and each profile answers only its own `API_SERVER_KEY` — so the caller passes
//! the profile's key, never a gateway-wide one.

use super::connections::Connection;
use crate::agent::acp::FailureKind;
use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

/// The gateway didn't answer in time; a bot that can't be reached must not hold a turn forever.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Creating or resuming a session may take as long as an agent starting locally.
const SESSION_TIMEOUT: Duration = Duration::from_secs(180);

/// One gateway, its profile and that profile's key.
#[derive(Clone)]
pub struct Client {
    base: String,
    key: String,
}

impl Client {
    pub fn new(connection: &Connection, profile: &str, key: String) -> Self {
        Self { base: connection.profile_url(profile), key }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        crate::http()
            .request(method, format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", self.key))
            .header("content-type", "application/json")
    }

    /// Answers the body, turning the gateway's error into one the apps can show.
    async fn answer(&self, req: reqwest::RequestBuilder, timeout: Duration) -> Result<Value> {
        let res = req.timeout(timeout).send().await.map_err(|e| {
            let kind = if e.is_timeout() { FailureKind::Timeout } else { FailureKind::NotSetUp };
            crate::agent::acp::Failure::error(kind, format!("Hermes gateway: {e}"))
        })?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            bail!("the Hermes gateway refused this profile's key ({})", status.as_u16());
        }
        if !status.is_success() {
            bail!("the Hermes gateway failed ({}): {}", status.as_u16(), error_message(&text));
        }
        serde_json::from_str(&text).with_context(|| format!("unreadable Hermes answer: {}", clip(&text)))
    }

    /// What the gateway can do; refused outright when it isn't a Hermes `api_server` at all.
    pub async fn capabilities(&self) -> Result<Value> {
        let caps = self.answer(self.request(reqwest::Method::GET, "/v1/capabilities"), TIMEOUT).await?;
        if !caps["features"]["session_chat_streaming"].as_bool().unwrap_or(false) {
            bail!("this Hermes gateway can't stream session chats; update Hermes on it");
        }
        Ok(caps)
    }

    /// The models it offers (`hermes-agent` on a stock gateway).
    pub async fn models(&self) -> Result<Vec<String>> {
        let res = self.answer(self.request(reqwest::Method::GET, "/v1/models"), TIMEOUT).await?;
        Ok(res["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect())
    }

    /// Makes an empty session; `title` must be unique on the gateway.
    pub async fn create_session(&self, title: &str) -> Result<String> {
        let res = self
            .answer(
                self.request(reqwest::Method::POST, "/api/sessions").json(&json!({"title": title})),
                SESSION_TIMEOUT,
            )
            .await?;
        res["session"]["id"]
            .as_str()
            .or_else(|| res["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the gateway returned no session id"))
    }

    /// Streams one turn: `(event name, payload)` frames, in order, until the run is done.
    pub async fn chat_stream(
        &self,
        session: &str,
        body: Value,
    ) -> Result<futures::stream::BoxStream<'static, Result<Frame>>> {
        let res = self
            .request(reqwest::Method::POST, &format!("/api/sessions/{session}/chat/stream"))
            .json(&body)
            .timeout(SESSION_TIMEOUT)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            bail!("the Hermes gateway couldn't start the run ({}): {}", status.as_u16(), error_message(&text));
        }
        Ok(Box::pin(frames(res)))
    }

    /// Answers a permission card the run raised (`once`, `session`, `always` or `deny`).
    pub async fn approve(&self, run: &str, choice: &str) -> Result<()> {
        self.answer(
            self.request(reqwest::Method::POST, &format!("/v1/runs/{run}/approval")).json(&json!({"choice": choice})),
            TIMEOUT,
        )
        .await
        .map(drop)
    }

    /// Stops a running turn.
    pub async fn stop(&self, run: &str) -> Result<()> {
        self.answer(self.request(reqwest::Method::POST, &format!("/v1/runs/{run}/stop")).json(&json!({})), TIMEOUT)
            .await
            .map(drop)
    }
}

/// One SSE frame: the gateway names every frame (`run.started`, `assistant.delta`, …), which is what tells the
/// events apart; `data` is the payload.
#[derive(Clone, Debug)]
pub struct Frame {
    pub event: String,
    pub data: Value,
}

/// The gateway's own error text, from `{"error":{"message":…}}` or the raw body.
fn error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| clip(text))
}

fn clip(text: &str) -> String {
    text.chars().take(300).collect()
}

/// Reads a `text/event-stream` body frame by frame.
fn frames(res: reqwest::Response) -> impl futures::Stream<Item = Result<Frame>> + Send {
    let body = Box::pin(res.bytes_stream().map(|chunk| chunk.map(|bytes| bytes.to_vec())));
    let state = SseState { body, buffer: String::new(), event: String::new() };
    futures::stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        loop {
            // A complete frame already buffered is handed out before more bytes are read.
            if let Some(frame) = state.take_frame() {
                return Some((frame, Some(state)));
            }
            match state.body.next().await {
                Some(Ok(bytes)) => state.buffer.push_str(&String::from_utf8_lossy(&bytes)),
                Some(Err(e)) => {
                    let error = Err(anyhow!("the Hermes gateway's stream broke: {e}"));
                    return Some((error, None));
                }
                None => return None,
            }
        }
    })
}

struct SseState {
    body: futures::stream::BoxStream<'static, reqwest::Result<Vec<u8>>>,
    buffer: String,
    /// The `event:` name of the frame being read.
    event: String,
}

impl SseState {
    /// The next complete frame in the buffer: its `event:` name and `data:` payload.
    fn take_frame(&mut self) -> Option<Result<Frame>> {
        loop {
            let end = self.buffer.find('\n')?;
            let line = self.buffer[..end].trim_end_matches('\r').to_owned();
            self.buffer.drain(..=end);
            if line.is_empty() {
                // A blank line ends a frame; one without a payload (a keepalive comment) is skipped.
                if self.event.is_empty() {
                    continue;
                }
                let event = std::mem::take(&mut self.event);
                return Some(Err(anyhow!("frame `{event}` carried no data")));
            }
            if let Some(name) = line.strip_prefix("event:") {
                name.trim().clone_into(&mut self.event);
            } else if let Some(data) = line.strip_prefix("data:") {
                let event = std::mem::take(&mut self.event);
                let data = data.trim();
                if event.is_empty() || data.is_empty() {
                    continue;
                }
                return Some(match serde_json::from_str::<Value>(data) {
                    Ok(value) => Ok(Frame { event, data: value }),
                    Err(e) => Err(anyhow!("unreadable `{event}` frame: {e}")),
                });
            }
        }
    }
}
