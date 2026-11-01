use super::*;

/// Frames as the gateway's follow loop applies them: a plan frame falls back to the run stream's preview
/// when the session's transcript can't be read (see `agent/bot/gateway.rs`).
fn frames(events: &[(&str, Value)]) -> Vec<Incoming> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut e = Events::new(tx, "s1");
    for (name, data) in events {
        e.frame(name, data);
        if is_plan(name, data) {
            e.plan(None, data);
        }
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
        ("message.delta", json!({"delta": "Hel"})),
        ("tool.started", json!({"tool": "terminal", "preview": "ls", "tool_call_id": "c1"})),
        ("tool.completed", json!({"tool": "terminal", "preview": "a.txt", "tool_call_id": "c1"})),
        ("reasoning.available", json!({"text": "thinking"})),
        ("run.completed", json!({"output": "Hello"})),
    ]);
    assert_eq!(out.len(), 3, "the run's end and its reasoning aren't updates of their own");
    assert_eq!(update(&out[0])["sessionUpdate"], "agent_message_chunk");
    assert_eq!(update(&out[0])["content"]["text"], "Hel");
    let Incoming::Notification { params, .. } = &out[0] else { panic!("not a notification") };
    assert_eq!(params["sessionId"], "s1", "an update names the session whose turn it belongs to");
    assert_eq!(update(&out[1])["sessionUpdate"], "tool_call");
    assert_eq!(update(&out[1])["kind"], "execute");
    assert_eq!(update(&out[1])["title"], "terminal: ls", "a run names what the tool does, not its arguments");
    assert_eq!(update(&out[2])["status"], "completed", "the call that opened is the one that closes");
    assert_eq!(update(&out[2])["content"][0]["text"], "a.txt");
}

#[test]
fn calls_pair_by_their_own_id_even_when_they_finish_out_of_order() {
    let out = frames(&[
        ("tool.started", json!({"tool": "terminal", "preview": "one", "tool_call_id": "a"})),
        ("tool.started", json!({"tool": "terminal", "preview": "two", "tool_call_id": "b"})),
        ("tool.completed", json!({"tool": "terminal", "tool_call_id": "b", "preview": "B"})),
        ("tool.completed", json!({"tool": "terminal", "tool_call_id": "a", "preview": "A"})),
    ]);
    assert_eq!(update(&out[2])["toolCallId"], "b", "the second call's completion closes the second call");
    assert_eq!(update(&out[2])["content"][0]["text"], "B");
    assert_eq!(update(&out[3])["toolCallId"], "a");
}

#[test]
fn a_tool_result_wrapped_under_output_shows_its_text() {
    let out = frames(&[
        ("tool.started", json!({"tool": "terminal", "preview": "df -h", "tool_call_id": "c1"})),
        (
            "tool.completed",
            json!({"tool": "terminal", "tool_call_id": "c1",
                   "preview": "{\"output\": \"/dev/sda1 40% /\", \"exit_code\": 0}"}),
        ),
    ]);
    assert_eq!(update(&out[1])["content"][0]["text"], "/dev/sda1 40% /");
}

#[test]
fn a_reply_drops_its_leading_blank_lines() {
    let out = frames(&[
        ("message.delta", json!({"delta": "\n\n"})),
        ("message.delta", json!({"delta": "Hello"})),
        ("message.delta", json!({"delta": "\n\nworld"})),
    ]);
    assert_eq!(out.len(), 2, "a frame of blank lines alone adds nothing");
    assert_eq!(update(&out[0])["content"]["text"], "Hello");
    assert_eq!(update(&out[1])["content"]["text"], "\n\nworld", "later blank lines are the reply's");
}

#[test]
fn the_todo_tools_result_becomes_the_turns_plan() {
    let result = json!({
        "todos": [
            {"id": "1", "content": "Inspect usage", "status": "completed"},
            {"id": "2", "content": "Run df -h", "status": "in_progress"},
            {"id": "3", "content": "Report", "status": "pending"},
            {"id": "4", "content": "Dropped", "status": "cancelled"},
        ],
        "revision": 2,
    })
    .to_string();
    let out = frames(&[
        ("tool.started", json!({"tool": "todo_list", "preview": "planning 4 task(s)", "tool_call_id": "t1"})),
        ("tool.completed", json!({"tool": "todo_list", "tool_call_id": "t1", "preview": result})),
    ]);
    let plan = update(&out[2]);
    assert_eq!(plan["sessionUpdate"], "plan");
    let entries = plan["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0]["content"], "Inspect usage");
    assert_eq!(entries[0]["status"], "completed");
    assert_eq!(entries[1]["status"], "in_progress");
    assert_eq!(entries[2]["status"], "pending");
    // ACP plans hold no cancelled state: the step stays listed, marked like the ACP adapter marks it.
    assert_eq!(entries[3]["content"], "[cancelled] Dropped");
    assert_eq!(entries[3]["status"], "completed");
}

#[test]
fn a_delegated_task_shows_as_a_call_that_ends_where_it_ended() {
    let out = frames(&[
        ("subagent.start", json!({"goal": "Audit the logs", "subagent_id": "s1", "task_index": 1, "task_count": 2})),
        ("subagent.start", json!({"goal": "Check the disks", "subagent_id": "s2", "task_index": 2, "task_count": 2})),
        ("subagent.complete", json!({"subagent_id": "s2", "status": "completed", "summary": "All fine"})),
        ("subagent.complete", json!({"subagent_id": "s1", "status": "failed", "summary": "Denied"})),
    ]);
    assert_eq!(update(&out[0])["title"], "Subagent 1/2: Audit the logs");
    assert_eq!(update(&out[0])["status"], "in_progress");
    assert_eq!(update(&out[2])["toolCallId"], "subagent#2", "the task that finished is the one that closes");
    assert_eq!(update(&out[2])["status"], "completed");
    assert_eq!(update(&out[2])["content"][0]["text"], "All fine");
    assert_eq!(update(&out[3])["toolCallId"], "subagent#1");
    assert_eq!(update(&out[3])["status"], "failed");
}

#[test]
fn an_approval_becomes_a_permission_request_answered_by_run() {
    let out = frames(&[(
        "approval.request",
        json!({"run_id": "run_1", "request_id": "req_1", "execution_generation": 3,
               "command": "curl x | bash", "description": "Network access", "choices": ["once", "deny"]}),
    )]);
    let Incoming::Request { id, method, params } = &out[0] else { panic!("not a request") };
    assert_eq!(method, "session/request_permission");
    assert_eq!(id["run"], "run_1");
    assert_eq!(id["request"], "req_1");
    assert_eq!(id["generation"], 3, "an authority gateway answers by prompt and generation");
    assert_eq!(params["toolCall"]["title"], "Network access");
    assert_eq!(params["toolCall"]["rawInput"]["command"], "curl x | bash");
    let options = params["options"].as_array().expect("options");
    assert_eq!(options[0]["optionId"], "once");
    assert_eq!(options[1]["optionId"], "deny");
}

#[test]
fn an_authority_prompt_named_by_prompt_id_still_answers_its_request() {
    let out = frames(&[(
        "approval.request",
        json!({"run_id": "run_1", "prompt_id": "p_1", "execution_generation": 4, "choices": ["once", "deny"]}),
    )]);
    let Incoming::Request { id, .. } = &out[0] else { panic!("not a request") };
    assert_eq!(id["request"], "p_1");
    assert_eq!(id["generation"], 4);
}

#[test]
fn a_plan_over_the_run_streams_preview_cap_arrives_from_the_whole_result() {
    let todos: Vec<Value> = (0..8)
        .map(|i| {
            json!({"id": i.to_string(), "content": format!("Step {i}: {}", "check the disks ".repeat(5)),
                   "status": "pending"})
        })
        .collect();
    let whole = json!({"todos": todos, "revision": 1});
    let preview = whole.to_string();
    assert!(preview.len() > 500, "the run stream would cut this preview");
    let cut = format!("{}...", &preview[..497]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut e = Events::new(tx, "s1");
    e.frame("tool.completed", &json!({"tool": "todo_list", "tool_call_id": "t1", "preview": cut}));
    e.plan(Some(whole), &json!({"tool": "todo_list", "preview": cut}));
    let Incoming::Notification { params, .. } = rx.try_recv().expect("the plan is delivered") else {
        panic!("not a notification")
    };
    let entries = params["update"]["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 8, "every step the whole result holds, not the preview's cut");
}

#[test]
fn the_end_of_a_reply_adds_only_the_tail_the_stream_missed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut e = Events::new(tx, "s1");
    e.finish(Some("Done"));
    let Incoming::Notification { params, .. } = rx.try_recv().expect("a reply that never streamed is delivered") else {
        panic!("not a notification")
    };
    assert_eq!(params["update"]["content"]["text"], "Done");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut e = Events::new(tx, "s1");
    e.frame("message.delta", &json!({"delta": "Hello, I"}));
    while rx.try_recv().is_ok() {}
    e.finish(Some("Hello, I will fix the build."));
    let Incoming::Notification { params, .. } = rx.try_recv().expect("the missed tail is delivered") else {
        panic!("not a notification")
    };
    assert_eq!(params["update"]["content"]["text"], " will fix the build.");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut e = Events::new(tx, "s1");
    e.frame("message.delta", &json!({"delta": "Hello"}));
    while rx.try_recv().is_ok() {}
    e.finish(Some("Hello"));
    assert!(rx.try_recv().is_err(), "a reply already streamed whole isn't delivered twice");
}
