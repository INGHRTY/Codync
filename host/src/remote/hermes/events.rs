//! A Hermes gateway's SSE frames as the ACP updates the bot actor already understands.
//!
//! The actor maps `session/update`s onto transcript entries and answers `session/request_permission`s with
//! cards (`agent/bot/updates.rs`). Translating here rather than writing a second mapping keeps one set of
//! rules for what a chat shows, whichever computer the agent runs on.

use crate::agent::acp::Incoming;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// One run's frames, as the actor hears them.
pub(crate) struct Events {
    tx: mpsc::UnboundedSender<Incoming>,
    /// The gateway session the run belongs to: the actor applies an update only to its turn's session.
    session: String,
    /// Calls still open, by tool name: Hermes numbers none of them, so pairs are matched in order.
    open: HashMap<String, Vec<String>>,
    calls: u64,
    /// Whether the reply's first text has arrived: Hermes sometimes opens with blank lines.
    reply_started: bool,
}

impl Events {
    pub(crate) fn new(tx: mpsc::UnboundedSender<Incoming>, session: &str) -> Self {
        Self { tx, session: session.to_owned(), open: HashMap::new(), calls: 0, reply_started: false }
    }

    /// What the actor gets for a frame. A run's terminal frames are the stream's own (see `agent/bot/gateway.rs`).
    pub(crate) fn frame(&mut self, event: &str, d: &Value) {
        match event {
            "assistant.delta" => {
                let delta = d["delta"].as_str().unwrap_or_default();
                // Leading blank lines before the first word aren't part of the reply.
                let text = if self.reply_started { delta } else { delta.trim_start() };
                if !text.is_empty() {
                    self.reply_started = true;
                }
                self.chunk("agent_message_chunk", text);
            }
            "assistant.commentary" => self.chunk("agent_thought_chunk", d["text"].as_str().unwrap_or_default()),
            // `tool.progress` on the pseudo-tool `_thinking` repeats the reply's text, which the deltas already
            // carried; it isn't reasoning, so the trace shows it once.
            "tool.started" => self.start_tool(d),
            "tool.completed" | "tool.failed" => self.end_tool(d, event == "tool.failed"),
            "approval.request" => self.approval(d),
            _ => {}
        }
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
        let name = d["tool_name"].as_str().filter(|n| !n.is_empty()).unwrap_or("tool");
        let id = format!("{name}#{}", self.calls);
        self.open.entry(name.to_owned()).or_default().push(id.clone());
        let command = match &d["args"] {
            Value::String(s) => s.clone(),
            args => args["command"].as_str().unwrap_or_default().to_owned(),
        };
        let mut content = Vec::new();
        if let Some(preview) = d["preview"].as_str().filter(|p| !p.is_empty()) {
            content.push(json!({"type": "text", "text": preview}));
        }
        self.notify(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title(name, &command),
            "kind": kind(name),
            "status": "in_progress",
            "rawInput": {"command": command},
            "content": content,
            "locations": [],
        }));
    }

    fn end_tool(&mut self, d: &Value, failed: bool) {
        let name = d["tool_name"].as_str().filter(|n| !n.is_empty()).unwrap_or("tool");
        let Some(id) = self.open.get_mut(name).and_then(|ids| if ids.is_empty() { None } else { Some(ids.remove(0)) })
        else {
            return;
        };
        let mut content = Vec::new();
        if let Some(preview) = d["preview"].as_str().filter(|p| !p.is_empty()) {
            content.push(json!({"type": "text", "text": preview}));
        }
        self.notify(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if failed { "failed" } else { "completed" },
            "content": content,
        }));
    }

    /// Hermes asks about a command and waits; the answer goes back by run and request (`agent/bot/gateway.rs`).
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
            // Where to answer: the run the gateway holds, and the request inside it.
            id: json!({"run": d["run_id"], "request": d["request_id"]}),
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
        c => format!("{name}: {}", crate::agent::acp::truncate(c, 120)),
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
mod tests {
    use super::*;

    fn frames(events: &[(&str, Value)]) -> Vec<Incoming> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut e = Events::new(tx, "s1");
        for (name, data) in events {
            e.frame(name, data);
        }
        drop(e);
        let mut out = Vec::new();
        while let Ok(i) = rx.try_recv() {
            out.push(i);
        }
        out
    }

    fn update(inc: &Incoming) -> &Value {
        let Incoming::Notification { params, .. } = inc else { panic!("not a notification") };
        &params["update"]
    }

    #[test]
    fn a_turn_becomes_chunks_and_paired_tool_calls() {
        let out = frames(&[
            ("assistant.delta", json!({"delta": "Hel"})),
            ("tool.started", json!({"tool_name": "terminal", "preview": "ls", "args": {"command": "ls -la"}})),
            ("tool.completed", json!({"tool_name": "terminal", "preview": "a.txt"})),
            ("tool.progress", json!({"tool_name": "_thinking", "delta": "thinking"})),
            ("run.completed", json!({"message_id": "m"})),
        ]);
        assert_eq!(out.len(), 3, "the terminal frame and the reply's echo aren't updates");
        assert_eq!(update(&out[0])["sessionUpdate"], "agent_message_chunk");
        assert_eq!(update(&out[0])["content"]["text"], "Hel");
        let Incoming::Notification { params, .. } = &out[0] else { panic!("not a notification") };
        assert_eq!(params["sessionId"], "s1", "an update names the session whose turn it belongs to");
        assert_eq!(update(&out[1])["sessionUpdate"], "tool_call");
        assert_eq!(update(&out[1])["kind"], "execute");
        assert_eq!(update(&out[1])["title"], "terminal: ls -la");
        assert_eq!(update(&out[2])["status"], "completed", "the call that opened is the one that closes");
    }

    #[test]
    fn a_reply_drops_its_leading_blank_lines() {
        let out = frames(&[
            ("assistant.delta", json!({"delta": "\n\n"})),
            ("assistant.delta", json!({"delta": "Hello"})),
            ("assistant.delta", json!({"delta": "\n\nworld"})),
        ]);
        assert_eq!(out.len(), 2, "a frame of blank lines alone adds nothing");
        assert_eq!(update(&out[0])["content"]["text"], "Hello");
        assert_eq!(update(&out[1])["content"]["text"], "\n\nworld", "later blank lines are the reply's");
    }

    #[test]
    fn an_approval_becomes_a_permission_request_answered_by_run() {
        let out = frames(&[(
            "approval.request",
            json!({"run_id": "run_1", "request_id": "req_1", "command": "curl x | bash",
                   "description": "Network access", "choices": ["once", "deny"]}),
        )]);
        let Incoming::Request { id, method, params } = &out[0] else { panic!("not a request") };
        assert_eq!(method, "session/request_permission");
        assert_eq!(id["run"], "run_1");
        assert_eq!(id["request"], "req_1");
        assert_eq!(params["toolCall"]["title"], "Network access");
        assert_eq!(params["toolCall"]["rawInput"]["command"], "curl x | bash");
        let options = params["options"].as_array().expect("options");
        assert_eq!(options[0]["optionId"], "once");
        assert_eq!(options[1]["optionId"], "deny");
    }
}
