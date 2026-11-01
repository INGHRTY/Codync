//! Talking to a Hermes gateway's HTTP API: sessions, a turn's run and its event stream, approvals, models
//! and capability checks.
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
/// A run's event stream is reattached after this long, which loses nothing: the stream replays. It is long
/// enough to ride out a normal turn without the reattach, and short enough that a dead connection is noticed.
const RUN_STREAM_TIMEOUT: Duration = Duration::from_secs(600);

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
        let features = &caps["features"];
        for (name, what) in [
            ("run_submission", "start a turn as a run"),
            ("run_events_sse", "stream a run's events"),
            ("run_status", "report a run's status"),
        ] {
            if !features[name].as_bool().unwrap_or(false) {
                bail!("this Hermes gateway can't {what}; update Hermes on it");
            }
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

    /// Starts one turn as a run and returns its id, without waiting for it: the run belongs to the gateway,
    /// not to a connection, so a stream that drops is reattached rather than ending the turn.
    ///
    /// `message` is Hermes' `input` ([`super::events`]' `chat_message` shape): the user's text, or messages
    /// whose last one carries text and `image_url` parts. `system` is the ephemeral system prompt a gateway
    /// session has no place for of its own.
    pub async fn start_run(&self, session: &str, message: &Value, system: &str, model: Option<&str>) -> Result<String> {
        let mut body = json!({
            "input": run_input(message),
            "session_id": session,
            "instructions": system,
        });
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            body["model"] = model.into();
        }
        let res = self.answer(self.request(reqwest::Method::POST, "/v1/runs").json(&body), SESSION_TIMEOUT).await?;
        res["run_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the gateway returned no run id"))
    }

    /// A run's events, from `after` (the last sequence number already applied): the gateway replays what was
    /// missed first, then streams live, so a reattach continues the turn where the old stream stopped.
    pub async fn run_events(
        &self,
        run: &str,
        after: Option<u64>,
    ) -> Result<futures::stream::BoxStream<'static, Result<Frame>>> {
        let mut req = self.request(reqwest::Method::GET, &format!("/v1/runs/{run}/events"));
        if let Some(after) = after {
            req = req.header("last-event-id", after.to_string());
        }
        let res = req.timeout(RUN_STREAM_TIMEOUT).send().await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            bail!("the Hermes gateway has no events for run {run} ({}): {}", status.as_u16(), error_message(&text));
        }
        Ok(Box::pin(frames(res)))
    }

    /// A run's pollable status: `running`, `queued`, `waiting_for_approval`, or one of the terminal states,
    /// with the reply in `output` once it completed.
    pub async fn run_status(&self, run: &str) -> Result<Value> {
        self.answer(self.request(reqwest::Method::GET, &format!("/v1/runs/{run}")), TIMEOUT).await
    }

    /// The last todo tool result in `session`, whole: the transcript keeps it as the tool wrote it, while
    /// a run's `tool.completed.preview` is cut at 500 characters. `call` names the call whose result it is.
    pub async fn todo_result(&self, session: &str, call: Option<&str>) -> Result<Option<Value>> {
        let res = self
            .answer(
                self.request(reqwest::Method::GET, &format!("/api/sessions/{session}/messages?order=latest&limit=500")),
                TIMEOUT,
            )
            .await?;
        let call = call.filter(|call| !call.is_empty());
        let rows = res["data"].as_array().into_iter().flatten().rev();
        for row in rows {
            if row["role"].as_str() != Some("tool") {
                continue;
            }
            if call.is_some_and(|call| row["tool_call_id"].as_str() != Some(call)) {
                continue;
            }
            let Ok(data) = serde_json::from_str::<Value>(row["content"].as_str().unwrap_or_default()) else {
                continue;
            };
            if data["todos"].is_array() {
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// Answers a permission card the run raised (`once`, `session`, `always` or `deny`). A gateway that holds
    /// the run's authority wants the exact prompt and generation as well; the three fields travel together.
    pub async fn approve(&self, run: &str, choice: &str, request: Option<&str>, generation: Option<u64>) -> Result<()> {
        let mut body = json!({"choice": choice});
        if let (Some(request), Some(generation)) = (request, generation) {
            body["request_id"] = request.into();
            body["execution_generation"] = generation.into();
        }
        self.answer(self.request(reqwest::Method::POST, &format!("/v1/runs/{run}/approval")).json(&body), TIMEOUT)
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

/// A turn's message as Hermes' run `input`: the text itself, or one user message whose content holds its
/// parts (text and `image_url`), which is the shape a run's `input` takes for anything but plain text.
fn run_input(message: &Value) -> Value {
    match message {
        Value::Array(parts) => json!([{"role": "user", "content": parts}]),
        text => text.clone(),
    }
}

/// One SSE frame: the gateway names every frame (`message.delta`, `tool.started`, …), which is what tells the
/// events apart; `data` is the payload, carrying the run's `seq` for a reattach. A run's stream names it in the
/// payload (`{"event": …}`) rather than on an `event:` line, so both are read.
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
    let state = SseState { body, buffer: String::new(), event: String::new(), id: None };
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
    /// The `event:` name of the frame being read; a run's stream names it inside the payload instead.
    event: String,
    /// The last `id:` line read, which is the frame's sequence number on a run's stream.
    id: Option<u64>,
}

impl SseState {
    /// The next complete frame in the buffer: its name and `data:` payload.
    fn take_frame(&mut self) -> Option<Result<Frame>> {
        loop {
            let end = self.buffer.find('\n')?;
            let line = self.buffer[..end].trim_end_matches('\r').to_owned();
            self.buffer.drain(..=end);
            if line.is_empty() {
                // A blank line ends a frame; one without a payload (a keepalive comment) is skipped.
                if !self.event.is_empty() {
                    let event = std::mem::take(&mut self.event);
                    return Some(Err(anyhow!("frame `{event}` carried no data")));
                }
                self.id = None;
                continue;
            }
            if let Some(name) = line.strip_prefix("event:") {
                name.trim().clone_into(&mut self.event);
            } else if let Some(id) = line.strip_prefix("id:") {
                self.id = id.trim().parse().ok();
            } else if let Some(data) = line.strip_prefix("data:") {
                let named = std::mem::take(&mut self.event);
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                let mut data: Value = match serde_json::from_str(data) {
                    Ok(value) => value,
                    Err(e) => return Some(Err(anyhow!("unreadable frame: {e}"))),
                };
                // Only a JSON object can carry the sequence; other payloads stay untouched rather than panic.
                if let Value::Object(fields) = &mut data
                    && fields.get("seq").is_none_or(Value::is_null)
                    && let Some(id) = self.id
                {
                    fields.insert("seq".into(), id.into());
                }
                // A run's stream names the frame in its payload; a plain SSE stream on the `event:` line.
                let event =
                    if named.is_empty() { data["event"].as_str().unwrap_or_default().to_owned() } else { named };
                if event.is_empty() {
                    continue;
                }
                return Some(Ok(Frame { event, data }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(body: &str) -> SseState {
        SseState { body: Box::pin(futures::stream::empty()), buffer: body.to_owned(), event: String::new(), id: None }
    }

    /// A run's stream as the gateway writes it: the frame's name is in the payload and the `id:` line carries
    /// its sequence, with a keepalive comment first and a closing sentinel last.
    #[test]
    fn reads_frames_named_in_the_payload() {
        let mut state = reader(concat!(
            ": open\n\n",
            "id: 0\n",
            "data: {\"event\": \"message.delta\", \"run_id\": \"run_1\", \"delta\": \"ok\", \"seq\": 0}\n\n",
            "id: 1\n",
            "data: {\"event\": \"tool.started\", \"tool\": \"terminal\", \"seq\": 1}\n\n",
            ": stream closed\n\n",
        ));
        let delta = state.take_frame().expect("a frame").expect("readable");
        assert_eq!((delta.event.as_str(), delta.data["delta"].as_str()), ("message.delta", Some("ok")));
        let tool = state.take_frame().expect("a second frame").expect("readable");
        assert_eq!((tool.event.as_str(), tool.data["seq"].as_u64()), ("tool.started", Some(1)));
        assert!(state.take_frame().is_none(), "the sentinel ends the stream");
    }

    /// The `id:` line stands in for a payload without `seq`, so a reattach resumes where the stream stopped.
    #[test]
    fn takes_the_sequence_from_the_id_line() {
        let mut state = reader("id: 7\ndata: {\"event\": \"run.completed\", \"output\": \"done\"}\n\n");
        let frame = state.take_frame().expect("a frame").expect("readable");
        assert_eq!(frame.data["seq"].as_u64(), Some(7));
    }

    /// A payload that isn't an object can't carry a sequence, and the reader leaves it as it came.
    #[test]
    fn a_payload_that_isnt_an_object_keeps_its_sequence_out_of_the_way() {
        let mut state = reader("id: 3\nevent: message.delta\ndata: [1, 2]\n\n");
        let frame = state.take_frame().expect("a frame").expect("readable");
        assert_eq!(frame.event, "message.delta");
        assert_eq!(frame.data, json!([1, 2]));
    }

    /// The state's chat stream names frames on the `event:` line, which still reads.
    #[test]
    fn reads_frames_named_on_the_event_line() {
        let mut state = reader("event: message.delta\ndata: {\"delta\": \"hi\"}\n\n");
        let frame = state.take_frame().expect("a frame").expect("readable");
        assert_eq!((frame.event.as_str(), frame.data["delta"].as_str()), ("message.delta", Some("hi")));
    }
}
