//! A Hermes gateway's run events as the ACP updates the bot actor already understands.
//!
//! The actor maps `session/update`s onto transcript entries and answers `session/request_permission`s with
//! cards (`agent/bot/updates.rs`). Translating here rather than writing a second mapping keeps one set of
//! rules for what a chat shows, whichever computer the agent runs on. A run's own frames (`message.delta`,
//! `tool.started`, …) are the run stream's; the older session-chat names (`assistant.delta`, `tool_name`) are
//! read too, so a gateway that words them differently still lands in the same trace.

use crate::agent::acp::{self, Incoming};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// The tool whose result is the turn's plan (`tools/todo_tool.py`): `{todos:[{content, status}], …}`.
const PLAN_TOOL: &str = "todo_list";
/// What the trace calls a delegated task's call.
const SUBAGENT: &str = "subagent";

/// One call still open, as the trace knows it: the entry id handed out, and the gateway's own id when it named
/// one (a run does, and then a completion pairs exactly even with calls running side by side).
struct Open {
    id: String,
    call: Option<String>,
}

/// One run's frames, as the actor hears them.
pub(crate) struct Events {
    tx: mpsc::UnboundedSender<Incoming>,
    /// The gateway session the run belongs to: the actor applies an update only to its turn's session.
    session: String,
    /// Calls still open, by tool name: a gateway that names no call is matched in the order it started them.
    open: HashMap<String, Vec<Open>>,
    calls: u64,
    /// Whether the reply's first text has arrived: Hermes sometimes opens with blank lines.
    reply_started: bool,
    /// Whether reply text reached the chat, so a run's final output needn't be delivered twice.
    replied: bool,
    /// The reply text already delivered, so a terminal `output` can add only what the stream missed.
    streamed: String,
}

impl Events {
    pub(crate) fn new(tx: mpsc::UnboundedSender<Incoming>, session: &str) -> Self {
        Self {
            tx,
            session: session.to_owned(),
            open: HashMap::new(),
            calls: 0,
            reply_started: false,
            replied: false,
            streamed: String::new(),
        }
    }

    /// What the actor gets for a frame. A run's terminal frames are the stream's own (see `agent/bot/gateway.rs`).
    pub(crate) fn frame(&mut self, event: &str, d: &Value) {
        match event {
            "message.delta" | "assistant.delta" => self.delta(d),
            "message.interim" | "assistant.commentary" => {
                self.chunk("agent_thought_chunk", d["text"].as_str().unwrap_or_default());
            }
            "tool.started" => self.start_tool(d),
            "tool.completed" | "tool.failed" => {
                let failed = event == "tool.failed" || d["error"].as_bool().unwrap_or(false);
                self.end_tool(d, failed);
            }
            "subagent.start" => self.subagent_start(d),
            "subagent.complete" => self.subagent_end(d),
            "approval.request" => self.approval(d),
            // `reasoning.available` repeats what the reply already says, so the trace shows it once.
            _ => {}
        }
    }

    /// Delivers the end of a run's reply: `output` whole when nothing streamed, and when part of it did, only
    /// the tail the stream missed before it dropped.
    pub(crate) fn finish(&mut self, output: Option<&str>) {
        let Some(output) = output.filter(|text| !text.is_empty()) else { return };
        if !self.replied {
            self.delta(&json!({"delta": output}));
            return;
        }
        let tail = output
            .strip_prefix(self.streamed.as_str())
            .or_else(|| output.trim_start().strip_prefix(self.streamed.as_str()));
        if let Some(tail) = tail.filter(|tail| !tail.is_empty()) {
            self.chunk("agent_message_chunk", tail);
        }
    }

    fn delta(&mut self, d: &Value) {
        let delta = d["delta"].as_str().unwrap_or_default();
        // Leading blank lines before the first word aren't part of the reply.
        let text = if self.reply_started { delta } else { delta.trim_start() };
        if !text.is_empty() {
            self.reply_started = true;
            self.replied = true;
            self.streamed.push_str(text);
        }
        self.chunk("agent_message_chunk", text);
    }

    fn chunk(&self, update: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.notify(&json!({"sessionUpdate": update, "content": {"type": "text", "text": text}}));
    }

    fn notify(&self, update: &Value) {
        let params = json!({"sessionId": self.session, "update": update});
        let _ = self.tx.send(Incoming::Notification { method: "session/update".into(), params });
    }

    fn start_tool(&mut self, d: &Value) {
        self.calls += 1;
        let name = tool_name(d);
        let call = call_id(d).map(str::to_owned);
        let id = call.clone().unwrap_or_else(|| format!("{name}#{}", self.calls));
        self.open_call(name, call, id.clone());
        let command = match &d["args"] {
            Value::String(s) => s.clone(),
            args => args["command"].as_str().unwrap_or_default().to_owned(),
        };
        let preview = d["preview"].as_str().filter(|p| !p.is_empty()).unwrap_or_default();
        // A run's `tool.started` names the tool and what it does, not its arguments; the preview stands in.
        let detail = if command.is_empty() { preview } else { &command };
        let mut content = Vec::new();
        if !preview.is_empty() {
            content.push(json!({"type": "text", "text": preview}));
        }
        self.notify(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title(name, detail),
            "kind": kind(name),
            "status": "in_progress",
            "rawInput": {"command": command},
            "content": content,
            "locations": [],
        }));
    }

    fn end_tool(&mut self, d: &Value, failed: bool) {
        let name = tool_name(d);
        let Some(open) = self.close_call(name, call_id(d)) else { return };
        let mut content = Vec::new();
        if let Some(preview) = d["preview"].as_str().filter(|p| !p.is_empty()) {
            content.push(json!({"type": "text", "text": result_text(preview)}));
        }
        self.notify(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": open.id,
            "status": if failed { "failed" } else { "completed" },
            "content": content,
        }));
    }

    /// Hermes' `todo_list` returns the whole list on every call; the actor's `plan` update is what the four
    /// clients float above the composer. ACP plans hold no cancelled state: a cancelled step stays listed,
    /// struck through like a done one (the ACP adapter's own rule).
    ///
    /// `result` is the tool's whole result from the session's transcript; `frame` stands in for it with the
    /// run stream's preview when the transcript couldn't be read.
    pub(crate) fn plan(&self, result: Option<Value>, frame: &Value) {
        let Some(data) = result.or_else(|| {
            let preview = frame["preview"].as_str()?;
            serde_json::from_str::<Value>(preview).ok()
        }) else {
            tracing::debug!("a todo_list result couldn't be read; no plan is shown");
            return;
        };
        let Some(todos) = data["todos"].as_array() else { return };
        let entries: Vec<Value> = todos
            .iter()
            .filter_map(|item| {
                let content = item["content"].as_str().or_else(|| item["id"].as_str())?.trim();
                if content.is_empty() {
                    return None;
                }
                let (content, status) = match item["status"].as_str().unwrap_or("pending") {
                    "in_progress" => (content.to_owned(), "in_progress"),
                    "completed" => (content.to_owned(), "completed"),
                    "cancelled" => (format!("[cancelled] {content}"), "completed"),
                    _ => (content.to_owned(), "pending"),
                };
                Some(json!({"content": content, "priority": "medium", "status": status}))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.notify(&json!({"sessionUpdate": "plan", "entries": entries}));
    }

    /// A delegated task, shown as the call it is: what it was asked to do, and how it ended.
    fn subagent_start(&mut self, d: &Value) {
        self.calls += 1;
        let id = format!("{SUBAGENT}#{}", self.calls);
        self.open_call(SUBAGENT, subagent_id(d), id.clone());
        let goal = d["goal"].as_str().or_else(|| d["preview"].as_str()).unwrap_or_default().trim();
        let of = match (d["task_index"].as_u64(), d["task_count"].as_u64()) {
            (Some(i), Some(n)) if n > 1 => format!(" {i}/{n}"),
            _ => String::new(),
        };
        let title = match goal {
            "" => format!("Subagent{of}"),
            goal => format!("Subagent{of}: {}", acp::truncate(goal, 120)),
        };
        self.notify(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title,
            "kind": "other",
            "status": "in_progress",
            "rawInput": {},
            "content": [],
            "locations": [],
        }));
    }

    fn subagent_end(&mut self, d: &Value) {
        let call = subagent_id(d);
        let Some(open) = self.close_call(SUBAGENT, call.as_deref()) else { return };
        let failed = !matches!(d["status"].as_str().unwrap_or("completed"), "completed" | "ok");
        let mut content = Vec::new();
        if let Some(note) = d["summary"].as_str().or_else(|| d["preview"].as_str()).filter(|s| !s.is_empty()) {
            content.push(json!({"type": "text", "text": note}));
        }
        self.notify(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": open.id,
            "status": if failed { "failed" } else { "completed" },
            "content": content,
        }));
    }

    /// Keeps a call open until its completion: by the gateway's own id when it named one, else in order.
    fn open_call(&mut self, kind: &str, call: Option<String>, id: String) {
        self.open.entry(kind.to_owned()).or_default().push(Open { id, call });
    }

    /// Pairs a completion with the call it closes, which is the first one when the gateway names no id.
    fn close_call(&mut self, kind: &str, call: Option<&str>) -> Option<Open> {
        let open = self.open.get_mut(kind)?;
        let index = match call {
            Some(call) => open.iter().position(|o| o.call.as_deref() == Some(call)),
            None => Some(0),
        };
        let index = index.filter(|index| *index < open.len())?;
        Some(open.remove(index))
    }

    /// Hermes asks about a command and waits; the answer goes back by run and request (`agent/bot/gateway.rs`).
    /// A gateway holding the run's authority also names the execution generation the answer belongs to.
    fn approval(&self, d: &Value) {
        let command = d["command"].as_str().unwrap_or_default();
        let title = match d["description"].as_str().filter(|t| !t.is_empty()) {
            Some(t) => t.to_owned(),
            None if command.is_empty() => "Use a tool".to_owned(),
            None => command.to_owned(),
        };
        let options: Vec<Value> =
            d["choices"].as_array().into_iter().flatten().filter_map(Value::as_str).map(option).collect();
        let _ = self.tx.send(Incoming::Request {
            // Where to answer: the run the gateway holds, the request inside it, and its generation.
            id: json!({
                "run": d["run_id"],
                "request": d["request_id"].as_str().or_else(|| d["prompt_id"].as_str()),
                "generation": d["execution_generation"],
            }),
            method: "session/request_permission".into(),
            params: json!({
                "toolCall": {
                    "toolCallId": format!("approval#{}", d["run_id"].as_str().unwrap_or_default()),
                    "title": title,
                    "kind": "execute",
                    "rawInput": {"command": command},
                    "content": [],
                },
                "options": options,
            }),
        });
    }
}

/// The tool a frame is about: a run names it `tool`, the session stream `tool_name`.
fn tool_name(d: &Value) -> &str {
    d["tool_name"].as_str().or_else(|| d["tool"].as_str()).filter(|n| !n.is_empty()).unwrap_or("tool")
}

/// Whether a frame carries the todo tool's completed result, whose list the run stream only previews.
pub(crate) fn is_plan(event: &str, d: &Value) -> bool {
    event == "tool.completed" && !d["error"].as_bool().unwrap_or(false) && tool_name(d) == PLAN_TOOL
}

/// The id the gateway gave one call, which pairs its start with its completion exactly.
fn call_id(d: &Value) -> Option<&str> {
    d["tool_call_id"].as_str().filter(|id| !id.is_empty())
}

/// A delegated task's own id, which pairs its start with its completion even with several running.
fn subagent_id(d: &Value) -> Option<String> {
    ["subagent_id", "delegation_id", "child_session_id"]
        .iter()
        .find_map(|key| d[*key].as_str().filter(|id| !id.is_empty()))
        .map(str::to_owned)
}

/// A completed call's result as the trace shows it: a gateway wraps one under `output`, plain previews stay as
/// they are.
fn result_text(preview: &str) -> String {
    match serde_json::from_str::<Value>(preview) {
        Ok(v) => match &v["output"] {
            Value::String(output) => output.clone(),
            _ => preview.to_owned(),
        },
        Err(_) => preview.to_owned(),
    }
}

/// A choice Hermes accepts, as an ACP permission option (its `optionId` is the choice itself).
fn option(choice: &str) -> Value {
    let (name, kind) = match choice {
        "once" => ("Allow once", "allow_once"),
        "session" => ("Allow for this session", "allow_always"),
        "always" => ("Always allow", "allow_always"),
        _ => ("Deny", "reject_once"),
    };
    json!({"optionId": choice, "name": name, "kind": kind})
}

/// Hermes names its tools; the title a chat shows is the command when there is one.
fn title(name: &str, command: &str) -> String {
    match command.trim() {
        "" => name.to_owned(),
        c => format!("{name}: {}", acp::truncate(c, 120)),
    }
}

fn kind(name: &str) -> &'static str {
    match name {
        "terminal" | "shell" | "bash" | "execute" | "exec" => "execute",
        "browser" | "browse" => "fetch",
        _ => "other",
    }
}

#[cfg(test)]
mod tests;
