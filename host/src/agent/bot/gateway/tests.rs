use super::*;
use crate::remote::hermes::Connection;
use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A gateway's answers for one test: the frames its run's stream carries, how much of them each attach
/// serves before the stream closes, the run's pollable status (a sequence, for a run that moves on; `null`
/// is a failed read), the session transcript a plan is read from, and which attach attempts fail.
struct Fake {
    frames: Vec<(String, Value)>,
    cut: Vec<usize>,
    status: Value,
    statuses: Vec<Value>,
    messages: Value,
    events_errors: Vec<u16>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            cut: Vec::new(),
            status: json!({"status": "running"}),
            statuses: Vec::new(),
            messages: json!({"data": []}),
            events_errors: Vec::new(),
        }
    }
}

/// Frames as the gateway stamps them, each with its run `seq`.
fn run_frames(events: &[(&str, Value)]) -> Vec<(String, Value)> {
    events
        .iter()
        .enumerate()
        .map(|(i, (event, data))| {
            let mut data = data.clone();
            data["seq"] = json!(i);
            ((*event).to_owned(), data)
        })
        .collect()
}

/// The gateway `fake` describes: its base URL, and the calls it received.
async fn fake_gateway(fake: Fake) -> (String, Arc<StdMutex<Vec<String>>>) {
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let frames = Arc::new(fake.frames);
    let cut = Arc::new(fake.cut);
    let attaches = Arc::new(AtomicUsize::new(0));
    let status = fake.status.clone();
    let statuses = Arc::new(fake.statuses);
    let status_reads = Arc::new(AtomicUsize::new(0));
    let messages = fake.messages.clone();
    let events_errors = fake.events_errors;
    let app = Router::new()
        .route(
            "/v1/capabilities",
            get(|| async {
                axum::Json(json!({"features": {"run_submission": true, "run_events_sse": true, "run_status": true}}))
            }),
        )
        .route("/api/sessions", post(|| async { axum::Json(json!({"session": {"id": "s1"}})) }))
        .route(
            "/api/sessions/{id}/messages",
            get(move || {
                let messages = messages.clone();
                async move { axum::Json(messages) }
            }),
        )
        .route(
            "/v1/runs",
            post({
                let calls = calls.clone();
                move |axum::Json(b): axum::Json<Value>| {
                    let calls = calls.clone();
                    async move {
                        calls.lock().unwrap().push(format!("run {b}"));
                        axum::Json(json!({"run_id": "run_1", "status": "started"}))
                    }
                }
            }),
        )
        .route(
            "/v1/runs/{id}",
            get(move || {
                let status = status.clone();
                let statuses = statuses.clone();
                let read = status_reads.fetch_add(1, Ordering::SeqCst);
                async move {
                    match statuses.get(read).cloned().unwrap_or(status) {
                        Value::Null => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                        status => axum::Json(status).into_response(),
                    }
                }
            }),
        )
        .route(
            "/v1/runs/{id}/events",
            get({
                let frames = frames.clone();
                let cut = cut.clone();
                let attaches = attaches.clone();
                let calls = calls.clone();
                move |headers: HeaderMap| {
                    let after =
                        headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
                    calls.lock().unwrap().push(format!("events after {after:?}"));
                    let attach = attaches.fetch_add(1, Ordering::SeqCst);
                    let frames = frames.clone();
                    let cut = cut.clone();
                    let events_errors = events_errors.clone();
                    async move {
                        if let Some(code) = events_errors.get(attach).copied() {
                            return axum::response::Response::builder()
                                .status(StatusCode::from_u16(code).unwrap())
                                .body(axum::body::Body::from("{}"))
                                .unwrap();
                        }
                        let served = cut.get(attach).copied().unwrap_or(usize::MAX);
                        let streamed: Vec<Result<Event, std::convert::Infallible>> = frames
                            .iter()
                            .filter(|(_, d)| match after {
                                Some(after) => d["seq"].as_u64().unwrap_or(0) > after,
                                None => true,
                            })
                            .take(served)
                            .map(|(event, data)| Ok(Event::default().event(event.clone()).data(data.to_string())))
                            .collect();
                        Sse::new(futures::stream::iter(streamed)).into_response()
                    }
                }
            }),
        )
        .route(
            "/v1/runs/{id}/approval",
            post({
                let calls = calls.clone();
                move |axum::Json(b): axum::Json<Value>| {
                    let calls = calls.clone();
                    async move {
                        calls.lock().unwrap().push(format!(
                            "approval {} {} {}",
                            b["choice"].as_str().unwrap_or_default(),
                            b["request_id"].as_str().unwrap_or_default(),
                            b["execution_generation"].as_u64().unwrap_or_default(),
                        ));
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
        follow: Mutex::new(None),
    }
}

/// The updates the actor heard, in order.
fn heard_updates(g: &mut Gateway) -> Vec<Value> {
    let mut updates = Vec::new();
    while let Ok(Incoming::Notification { params, .. }) = g.rx.try_recv() {
        assert_eq!(params["sessionId"], "s1", "updates name the turn's session");
        updates.push(params["update"].clone());
    }
    updates
}

/// The reply text the actor heard.
fn heard(g: &mut Gateway) -> String {
    heard_updates(g)
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|update| update["content"]["text"].as_str())
        .collect()
}

/// The entries of the last plan the actor heard.
fn heard_plan(g: &mut Gateway) -> Vec<String> {
    let updates = heard_updates(g);
    let plan = updates.iter().rev().find(|update| update["sessionUpdate"] == "plan").expect("a plan");
    plan["entries"].as_array().expect("entries").iter().map(|e| e["content"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn a_finished_run_ends_the_turn_and_a_failed_one_errors() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut events = Events::new(tx, "s1");
    assert_eq!(terminal("run.completed", &json!({}), &mut events).unwrap()["stopReason"], "end_turn");
    assert_eq!(terminal("run.cancelled", &json!({}), &mut events).unwrap()["stopReason"], "cancelled");
    assert!(
        terminal("run.failed", &json!({"turn_exit_reason": "max_iterations_reached(60/60)"}), &mut events).is_err()
    );
    assert!(
        terminal("run.interrupted", &json!({"error": "Gateway shutdown interrupted the run."}), &mut events).is_err()
    );
}

#[test]
fn a_run_read_back_from_status_delivers_only_a_reply_that_never_streamed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = Events::new(tx, "s1");
    recovered(&json!({"status": "completed", "output": "Done"}), &mut events).unwrap();
    let Incoming::Notification { params, .. } = rx.try_recv().expect("the reply is delivered") else {
        panic!("not a notification")
    };
    assert_eq!(params["update"]["content"]["text"], "Done");
    // A reply that already streamed is delivered only up to what the stream missed, and a terminal state ends it.
    events.frame("message.delta", &json!({"delta": "Done"}));
    while rx.try_recv().is_ok() {}
    recovered(&json!({"status": "completed", "output": "Done"}), &mut events).unwrap();
    assert!(rx.try_recv().is_err(), "the reply reached the chat once");
    assert!(recovered(&json!({"status": "failed", "error": "boom"}), &mut events).is_err());
    assert!(is_terminal(&json!({"status": "completed"})));
    assert!(!is_terminal(&json!({"status": "queued"})), "a queued run is still the gateway's to finish");
}

#[test]
fn an_image_travels_in_the_message_and_text_alone_stays_plain() {
    let words = json!([{"type": "text", "text": "what is this?"}]);
    assert_eq!(chat_message(&words), json!("what is this?"));
    let with_image = json!([
        {"type": "text", "text": "what is this?"},
        {"type": "image", "mimeType": "image/png", "data": "iVBORw=="},
    ]);
    assert_eq!(
        chat_message(&with_image),
        json!([
            {"type": "text", "text": "what is this?"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw=="}},
        ])
    );
    // An image with no data is left out rather than sent as an empty URL.
    assert_eq!(chat_message(&json!([{"type": "image", "mimeType": "image/png", "data": ""}])), json!(""));
}

#[test]
fn session_titles_are_unique() {
    assert!(title("VPS Bot").starts_with("VPS Bot · "));
    assert_ne!(title("VPS Bot"), title("VPS Bot"));
}

#[tokio::test]
async fn a_run_streams_updates_and_ends_the_turn() {
    let (base, calls) = fake_gateway(Fake {
        frames: run_frames(&[
            ("message.delta", json!({"delta": "Hel"})),
            ("message.delta", json!({"delta": "lo"})),
            ("run.completed", json!({"completed": true, "output": "Hello"})),
        ]),
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!("hi"), "be brief", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn");
    assert_eq!(heard(&mut g), "Hello");
    let calls = calls.lock().unwrap().clone();
    assert!(calls[0].contains("hi"), "the turn's message starts the run: {calls:?}");
    assert!(calls[0].contains("input"), "as Hermes' run input: {calls:?}");
}

#[tokio::test]
async fn an_image_turn_wraps_its_parts_in_one_user_message() {
    let (base, calls) =
        fake_gateway(Fake { frames: run_frames(&[("run.completed", json!({}))]), ..Fake::default() }).await;
    let mut g = gateway(&base);
    let prompt = json!([
        {"type": "text", "text": "what is this?"},
        {"type": "image", "mimeType": "image/png", "data": "iVBORw=="},
    ]);
    g.send("s1", &chat_message(&prompt), "", None).await.unwrap().response().await.unwrap();
    let calls = calls.lock().unwrap().clone();
    let body: Value = serde_json::from_str(calls[0].strip_prefix("run ").unwrap()).unwrap();
    assert_eq!(
        body["input"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "what is this?"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw=="}},
        ]}]),
        "Hermes' run input takes a user message whose content holds the parts"
    );
}

#[tokio::test]
async fn a_cut_stream_is_reattached_where_it_stopped() {
    let (base, calls) = fake_gateway(Fake {
        frames: run_frames(&[
            ("message.delta", json!({"delta": "Hel"})),
            ("message.delta", json!({"delta": "lo"})),
            ("run.completed", json!({"completed": true, "output": "Hello"})),
        ]),
        // The first attach hears one frame and its connection drops; the reattach replays from there.
        cut: vec![1],
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!("hi"), "", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn");
    assert_eq!(heard(&mut g), "Hello", "the reply is heard once, not replayed");
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls[2], "events after Some(0)", "the reattach resumes after the last frame applied");
}

#[tokio::test]
async fn a_run_finished_while_away_is_read_back_with_its_reply() {
    let (base, _) = fake_gateway(Fake {
        status: json!({"status": "completed", "output": "Done"}),
        // The gateway holds no stream for the run any more: only its status can answer.
        events_errors: vec![404],
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!([{"type": "text", "text": "hi"}]), "", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn");
    assert_eq!(heard(&mut g), "Done");
}

#[tokio::test]
async fn a_queued_run_is_waited_for_until_it_finishes() {
    let (base, _) = fake_gateway(Fake {
        statuses: vec![
            json!({"status": "queued"}),
            json!({"status": "running"}),
            json!({"status": "completed", "output": "Done"}),
        ],
        // A queued run's stream is not served yet; its status is all there is until the gateway runs it.
        events_errors: vec![404],
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!("hi"), "", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn", "a queued run isn't a failed turn");
    assert_eq!(heard(&mut g), "Done");
}

#[tokio::test]
async fn a_dropped_link_reattaches_instead_of_failing_the_turn() {
    // Two attach attempts and two status reads hit a link that's down; the run finishes behind it.
    let (base, _) = fake_gateway(Fake {
        frames: run_frames(&[("run.completed", json!({"output": "Done"}))]),
        events_errors: vec![500, 500],
        statuses: vec![Value::Null, Value::Null, json!({"status": "completed", "output": "Done"})],
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!("hi"), "", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn", "a transient drop isn't a failed turn");
    assert_eq!(heard(&mut g), "Done", "the answer the run delivered while the link was down still arrives");
}

#[tokio::test]
async fn a_plan_over_the_run_streams_preview_cap_comes_from_the_sessions_transcript() {
    let todos: Vec<Value> = (0..8)
        .map(|i| {
            json!({"id": i.to_string(), "content": format!("Step {i}: {}", "check the disks ".repeat(5)),
                         "status": "pending"})
        })
        .collect();
    let whole = json!({"todos": todos, "revision": 1}).to_string();
    assert!(whole.len() > 500, "the run stream would cut this result's preview");
    let cut = format!("{}...", &whole[..497]);
    let (base, _) = fake_gateway(Fake {
        frames: run_frames(&[
            ("tool.started", json!({"tool": "todo_list", "preview": "planning 8 task(s)", "tool_call_id": "t1"})),
            ("tool.completed", json!({"tool": "todo_list", "tool_call_id": "t1", "preview": cut})),
            ("run.completed", json!({"output": "done"})),
        ]),
        messages: json!({"data": [{
            "role": "tool", "tool_call_id": "t1", "tool_name": "todo_list", "content": whole,
        }]}),
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let answer = g.send("s1", &json!("plan it"), "", None).await.unwrap();
    assert_eq!(answer.response().await.unwrap()["stopReason"], "end_turn");
    assert_eq!(heard_plan(&mut g).len(), 8, "every step of the whole result, not the preview's cut");
}

#[tokio::test]
async fn a_failed_run_errors_with_its_reason() {
    let (base, _) = fake_gateway(Fake {
        frames: run_frames(&[("run.failed", json!({"turn_exit_reason": "max_iterations_reached(60/60)"}))]),
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    let error = g.send("s1", &json!("hi"), "", None).await.unwrap().response().await.unwrap_err();
    assert!(error.to_string().contains("max_iterations_reached"), "{error}");
}

#[tokio::test]
async fn a_card_is_answered_on_its_run_with_its_prompt_and_generation() {
    let (base, calls) = fake_gateway(Fake {
        frames: run_frames(&[
            (
                "approval.request",
                json!({"run_id": "run_1", "request_id": "req_1", "execution_generation": 7,
                       "command": "rm -rf x"}),
            ),
            ("run.completed", json!({})),
        ]),
        ..Fake::default()
    })
    .await;
    let mut g = gateway(&base);
    g.send("s1", &json!("hi"), "", None).await.unwrap().response().await.unwrap();
    g.approve("run_1", "once", Some("req_1"), Some(7)).await.unwrap();
    g.stop().await.unwrap();
    let calls = calls.lock().unwrap().clone();
    assert_eq!(
        &calls[2..],
        ["approval once req_1 7", "stop"],
        "the exact prompt and generation travel with the choice"
    );
}

#[tokio::test]
async fn a_lost_run_is_settled_before_a_resume() {
    let (base, calls) = fake_gateway(Fake { status: json!({"status": "running"}), ..Fake::default() }).await;
    let g = gateway(&base);
    g.settle("run_1").await.unwrap();
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls, ["stop"], "a run still going at the gateway is stopped, not left to answer into a resume");
}
