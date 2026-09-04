//! Native opencode driver against a scripted in-test HTTP/SSE server.
//!
//! The fake speaks just enough of the v1 surface (`/global/health`,
//! `/session`, `/session/{id}/prompt_async`, `/session/{id}/abort`,
//! `/provider`, `/command`, `/global/event`) and hands the TEST full control
//! of bus timing via `emit()` — the premature-done class is exactly about
//! what happens between events, so the fixtures must own the clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use comet_harness::{
    CancellationToken, Harness, HarnessError, OpencodeHarness, RunControls, SteerMessage,
};
use comet_proto::{
    AgentEvent, DoneStatus, PlanDecision, ReasoningLevel, RunRequest, SandboxLevel, ToolCall,
    UserInputAnswer,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

// ---------------------------------------------------------------------------
// Fake server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeOpencode {
    base: String,
    events: broadcast::Sender<(u64, String)>,
    /// Every emitted frame, sequence-stamped — replayed to late SSE
    /// subscribers so tests may emit before the driver's stream connects.
    backlog: Arc<Mutex<Vec<(u64, String)>>>,
    /// Recorded `(path, body)` of every POST.
    posts: Arc<Mutex<Vec<(String, Value)>>>,
    providers: Arc<Mutex<Value>>,
    /// Whether an SSE subscriber existed when the FIRST prompt_async landed
    /// (the no-replay bus makes prompting before the subscription a real
    /// event-loss race — observed live on fast-failing turns).
    first_prompt_had_subscriber: Arc<Mutex<Option<bool>>>,
    /// The `directory` scope of every `GET /command` — the per-request
    /// instance selector a cwd-scoped probe must set (§10.4).
    command_directories: Arc<Mutex<Vec<Option<String>>>>,
}

impl FakeOpencode {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (events, _) = broadcast::channel::<(u64, String)>(256);
        let fake = Self {
            base,
            events: events.clone(),
            backlog: Arc::new(Mutex::new(Vec::new())),
            posts: Arc::new(Mutex::new(Vec::new())),
            providers: Arc::new(Mutex::new(json!({ "all": [], "default": {} }))),
            first_prompt_had_subscriber: Arc::new(Mutex::new(None)),
            command_directories: Arc::new(Mutex::new(Vec::new())),
        };
        let accept = fake.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let fake = accept.clone();
                tokio::spawn(async move { fake.serve(stream).await });
            }
        });
        fake
    }

    /// Push one bus event (the driver accepts both the bare and the
    /// `/global/event` envelope; the fake uses the enveloped form).
    fn emit(&self, payload: Value) {
        let framed = format!(
            "data: {}\n\n",
            json!({ "directory": "/", "payload": payload })
        );
        let mut backlog = self.backlog.lock().unwrap();
        let seq = backlog.len() as u64;
        backlog.push((seq, framed.clone()));
        let _ = self.events.send((seq, framed));
    }

    fn set_providers(&self, providers: Value) {
        *self.providers.lock().unwrap() = providers;
    }

    fn posts_to(&self, path: &str) -> Vec<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .collect()
    }

    async fn serve(self, mut stream: tokio::net::TcpStream) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        // One request per connection is enough for reqwest's default pool
        // behavior in these tests; keep-alive requests re-enter here.
        loop {
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.lines();
            let start = lines.next().unwrap_or_default().to_owned();
            let content_length = lines
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .next()
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body: Value = serde_json::from_slice(&buf[header_end..header_end + content_length])
                .unwrap_or(Value::Null);
            buf.drain(..header_end + content_length);

            let mut parts = start.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();
            let path = target.split('?').next().unwrap_or_default().to_owned();
            let directory = target
                .split_once("directory=")
                .map(|(_, q)| percent_decode(q.split('&').next().unwrap_or_default()));

            if method == "GET" && path == "/global/event" {
                // Subscribe FIRST, then snapshot the backlog: frames landing
                // in between arrive on both channels and dedupe by sequence.
                let mut rx = self.events.subscribe();
                let replay = self.backlog.lock().unwrap().clone();
                let mut next_seq = replay.len() as u64;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          cache-control: no-cache\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                let _ = stream
                    .write_all(b"data: {\"payload\":{\"type\":\"server.connected\",\"properties\":{}}}\n\n")
                    .await;
                for (_, frame) in &replay {
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                }
                let _ = stream.flush().await;
                while let Ok((seq, frame)) = rx.recv().await {
                    if seq < next_seq {
                        continue;
                    }
                    next_seq = seq + 1;
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = stream.flush().await;
                }
                return;
            }

            if method == "POST" {
                if path.ends_with("/prompt_async") {
                    let mut first = self.first_prompt_had_subscriber.lock().unwrap();
                    if first.is_none() {
                        *first = Some(self.events.receiver_count() > 0);
                    }
                }
                self.posts.lock().unwrap().push((path.clone(), body));
            }
            if method == "GET" && path == "/command" {
                self.command_directories
                    .lock()
                    .unwrap()
                    .push(directory.clone());
            }
            let (status, payload) = self.route(&method, &path, directory.as_deref());
            let body = payload.to_string();
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn route(&self, method: &str, path: &str, directory: Option<&str>) -> (&'static str, Value) {
        match (method, path) {
            ("GET", "/global/health") => ("200 OK", json!({ "healthy": true })),
            ("GET", "/provider") => ("200 OK", self.providers.lock().unwrap().clone()),
            // Skills ride the same list as commands, told apart by `source`
            // (live 1.18.10); both are invocable through the same endpoint.
            // The listing is scoped to the request's `directory`: a
            // directory-scoped probe also sees that project's `.opencode`
            // skills, exactly as the server would report them.
            ("GET", "/command") => {
                let mut commands = json!([
                    { "name": "init", "description": "Create AGENTS.md", "source": "command" },
                    { "name": "cometalpha", "description": "Alpha probe skill.", "source": "skill" },
                ]);
                if let Some(dir) = directory {
                    commands.as_array_mut().unwrap().push(json!({
                        "name": "project-skill",
                        "description": dir,
                        "source": "skill",
                    }));
                }
                ("200 OK", commands)
            }
            ("POST", "/session") => ("200 OK", json!({ "id": "ses_test" })),
            ("GET", "/session/ses_resume") => ("200 OK", json!({ "id": "ses_resume" })),
            ("GET", p) if p.starts_with("/session/") => ("404 Not Found", json!({})),
            ("POST", p) if p.ends_with("/prompt_async") => ("204 No Content", json!({})),
            ("POST", p) if p.ends_with("/abort") => ("200 OK", json!(true)),
            ("POST", p) if p.contains("/permission/") || p.contains("/question/") => {
                ("200 OK", json!(true))
            }
            _ => ("404 Not Found", json!({ "missing": path })),
        }
    }
}

/// Percent-decode one query value (reqwest form-encodes the `directory`
/// scope, so a path arrives with its separators escaped).
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len() => match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        plan_mode: false,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

#[allow(clippy::type_complexity)]
fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steering) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        plan: comet_harness::PlanControls::off(),
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: q.options.first().cloned().into_iter().collect(),
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token)
}

/// Controls with live plan wiring (§11.3): the requested-mode watch a test
/// toggles mid-run, and a prepared exit decision whose call is counted — the
/// engine's half of the bridge, scripted.
#[allow(clippy::type_complexity)]
fn plan_controls(
    mode: bool,
    decision: PlanDecision,
) -> (
    RunControls,
    mpsc::Sender<SteerMessage>,
    watch::Sender<bool>,
    Arc<Mutex<usize>>,
) {
    let (controls, steer, _token) = controls();
    let (mode_tx, mode_rx) = watch::channel(mode);
    let gate_calls = Arc::new(Mutex::new(0usize));
    let calls = gate_calls.clone();
    let controls = RunControls {
        plan: comet_harness::PlanControls {
            mode: mode_rx,
            request_exit: Box::new(move || {
                *calls.lock().unwrap() += 1;
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(decision.clone());
                rx
            }),
        },
        ..controls
    };
    (controls, steer, mode_tx, gate_calls)
}

fn harness(fake: &FakeOpencode) -> OpencodeHarness {
    OpencodeHarness::new().with_base_url(fake.base.clone())
}

/// Emit the standard opening frames of an assistant turn.
fn assistant_message(fake: &FakeOpencode, session: &str, message: &str) {
    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": session, "status": { "type": "busy" } },
    }));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": message, "role": "assistant", "sessionID": session } },
    }));
}

fn idle(fake: &FakeOpencode, session: &str) {
    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": session, "status": { "type": "idle" } },
    }));
}

async fn next_event(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("event within budget")
        .expect("stream open")
        .expect("ok event")
}

/// Poll until `path` has received `n` POSTs; returns their bodies.
async fn wait_posts(fake: &FakeOpencode, path: &str, n: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let posts = fake.posts_to(path);
            if posts.len() >= n {
                return posts;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} never saw {n} posts"))
}

/// Drain until a Done arrives; returns everything seen (Done last).
async fn drain_to_done(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let ev = next_event(stream).await;
        let done = matches!(&ev, AgentEvent::Done { .. });
        events.push(ev);
        if done {
            return events;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn thinking_streams_and_the_turn_settles_only_on_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");

    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_test"
    ));
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    // Reasoning part: open snapshot → deltas → closing snapshot (full text,
    // must dedup to nothing).
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_r", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "reasoning", "text": "",
        }},
    }));
    fake.emit(json!({
        "type": "message.part.delta",
        "properties": {
            "sessionID": "ses_test", "messageID": "msg_1", "partID": "prt_r",
            "field": "text", "delta": "let me think",
        },
    }));
    let thinking = next_event(&mut stream).await;
    assert!(matches!(
        &thinking,
        AgentEvent::ReasoningDelta { text } if text == "let me think"
    ));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_r", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "reasoning", "text": "let me think",
        }},
    }));

    // Text streams; the turn must NOT settle during the quiet gap after it —
    // only idle ends the turn (the premature-done regression).
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_t", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "text", "text": "Hello",
        }},
    }));
    let text = next_event(&mut stream).await;
    assert!(matches!(&text, AgentEvent::TextDelta { text } if text == "Hello"));
    let quiet = tokio::time::timeout(Duration::from_millis(600), stream.next()).await;
    assert!(quiet.is_err(), "nothing may settle a quiet-but-live turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.first(),
        Some(AgentEvent::AssistantMessageCompleted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: Some(sid),
            ..
        }) if sid == "ses_test"
    ));
}

#[tokio::test]
async fn foreign_session_idle_never_settles_our_turn() {
    // The exact bug in opencode's own ACP layer: the first idle observed —
    // any session's — settled the turn.
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_OTHER");
    let quiet = tokio::time::timeout(Duration::from_millis(600), stream.next()).await;
    assert!(quiet.is_err(), "a foreign session's idle settled our turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn model_and_advertised_variant_ride_the_prompt() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [{
            "id": "anthropic",
            "name": "Anthropic",
            "models": { "opus": { "name": "Opus", "variants": { "high": {}, "max": {} } } },
        }],
    }));
    let (controls, _steer, _token) = controls();
    let mut req = request("hi");
    req.model = Some("anthropic/opus".into());
    req.reasoning = Some(ReasoningLevel::XHigh);
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(prompts[0]["model"]["providerID"], "anthropic");
    assert_eq!(prompts[0]["model"]["modelID"], "opus");
    // XHigh isn't advertised: the ladder clamps to "high".
    assert_eq!(prompts[0]["variant"], "high");
    assert_eq!(prompts[0]["parts"][0]["text"], "hi");

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn steer_queues_mid_turn_and_delivers_at_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_t", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "text", "text": "working",
        }},
    }));
    let _ = next_event(&mut stream).await; // TextDelta

    steer
        .send(SteerMessage {
            prompt: "also do this".into(),
            message_id: None,
        })
        .await
        .unwrap();
    // Give the steer time to land in the queue, then end turn 1.
    tokio::time::sleep(Duration::from_millis(100)).await;
    idle(&fake, "ses_test");

    let ev = next_event(&mut stream).await;
    assert!(
        matches!(&ev, AgentEvent::Steered { .. }),
        "queued steer must continue the run at the turn boundary, got {ev:?}"
    );
    // The steer went out as a second prompt on the SAME session.
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 2).await;
    assert_eq!(prompts[1]["parts"][0]["text"], "also do this");

    // Turn 2 settles normally.
    assistant_message(&fake, "ses_test", "msg_2");
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn interrupt_aborts_and_settles_interrupted() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    token.cancel();
    wait_posts(&fake, "/session/ses_test/abort", 1).await;
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn provider_retries_surface_and_cap_out() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    let retry = |attempt: u64| {
        json!({
            "type": "session.status",
            "properties": { "sessionID": "ses_test", "status": {
                "type": "retry", "attempt": attempt,
                "message": "AI_APICallError: unreachable", "next": 0,
            }},
        })
    };
    fake.emit(retry(1));
    fake.emit(retry(3));
    let ev = next_event(&mut stream).await;
    let AgentEvent::Error { message } = &ev else {
        panic!("expected a retry error chip, got {ev:?}");
    };
    assert!(
        message.contains("retrying") && message.contains("attempt 3"),
        "{message}"
    );
    assert!(message.contains("unreachable"), "{message}");

    fake.emit(retry(8));
    let ev = next_event(&mut stream).await;
    let AgentEvent::Error { message } = &ev else {
        panic!("expected the give-up chip, got {ev:?}");
    };
    assert!(message.contains("Giving up"), "{message}");
    // The driver aborted the turn; the server answers with idle.
    wait_posts(&fake, "/session/ses_test/abort", 1).await;
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(_),
            ..
        })
    ));
}

#[tokio::test]
async fn session_error_with_no_content_settles_errored() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "ProviderAuthError",
            "data": { "message": "no credentials for anthropic" },
        }},
    }));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::Error { message } if message.contains("no credentials")
    ));
    // opencode re-emits the same failure with an exception-name prefix and a
    // stack — that must NOT mint a second chip (field report: every failure
    // rendered twice).
    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "UnknownError",
            "data": { "message": "ProviderAuthError: no credentials for anthropic\n    at stack" },
        }},
    }));
    let quiet = tokio::time::timeout(Duration::from_millis(400), stream.next()).await;
    assert!(
        quiet.is_err(),
        "duplicate error must not mint a second chip"
    );
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(e),
            ..
        }) if e.contains("no credentials")
    ));
}

#[tokio::test]
async fn subagent_task_streams_tagged_and_settles_from_the_task_part() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("spawn"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    // The task tool part registers the chip and binds by metadata.
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_task", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "task",
            "state": {
                "status": "running",
                "input": { "description": "Viz probe", "prompt": "run", "subagent_type": "general" },
                "metadata": { "sessionId": "ses_child", "parentSessionId": "ses_test" },
            },
        }},
    }));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "prt_task" && name == "Agent: Viz probe"
    ));

    // Child comes up and streams: prompt in, assistant text out — tagged.
    fake.emit(json!({
        "type": "session.created",
        "properties": { "info": {
            "id": "ses_child", "parentID": "ses_test",
            "title": "Viz probe (@general subagent)",
        }},
    }));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": "msg_cu", "role": "user", "sessionID": "ses_child" } },
    }));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_cu", "messageID": "msg_cu", "sessionID": "ses_child",
            "type": "text", "text": "run",
        }},
    }));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "prt_task"
                && matches!(&**event, AgentEvent::UserMessage { text } if text == "run")
    ));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": "msg_ca", "role": "assistant", "sessionID": "ses_child" } },
    }));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_ca", "messageID": "msg_ca", "sessionID": "ses_child",
            "type": "text", "text": "finished",
        }},
    }));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::Subagent { event, .. }
            if matches!(&**event, AgentEvent::TextDelta { text } if text == "finished")
    ));

    // The task part completing settles the chip: ToolResult + tagged Done.
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_task", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "task",
            "state": {
                "status": "completed",
                "input": { "description": "Viz probe" },
                "output": "<task_result>finished</task_result>",
                "title": "Viz probe",
                "metadata": { "sessionId": "ses_child" },
                "time": { "start": 1, "end": 2 },
            },
        }},
    }));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::ToolResult { id, is_error: false, .. } if id == "prt_task"
    ));
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "prt_task"
                && matches!(&**event, AgentEvent::Done { status: DoneStatus::Completed, .. })
    ));

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn resume_reuses_the_durable_session() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut req = request("continue");
    req.resume = Some("ses_resume".into());
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_resume"
    ));
    let _ = next_event(&mut stream).await; // PlanModeChanged
    wait_posts(&fake, "/session/ses_resume/prompt_async", 1).await;

    assistant_message(&fake, "ses_resume", "msg_1");
    idle(&fake, "ses_resume");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn slash_command_routes_through_the_command_endpoint() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/init the repo"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    let commands = wait_posts(&fake, "/session/ses_test/command", 1).await;
    assert_eq!(commands[0]["command"], "init");
    assert_eq!(commands[0]["arguments"], "the repo");
    assert!(fake.posts_to("/session/ses_test/prompt_async").is_empty());

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// A skill in the catalog is invoked exactly like a command — `GET /command`
/// lists both and the endpoint resolves either by name — so `/name args`
/// reaches opencode as its own native user action.
#[tokio::test]
async fn skill_invocation_routes_through_the_command_endpoint() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/cometalpha  do it "), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    let commands = wait_posts(&fake, "/session/ses_test/command", 1).await;
    assert_eq!(commands[0]["command"], "cometalpha");
    assert_eq!(commands[0]["arguments"], "do it", "args are trimmed");
    assert!(fake.posts_to("/session/ses_test/prompt_async").is_empty());

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// §10.5: a `/name` this harness's catalog does not advertise is never
/// translated — it stays prompt text so the CLI reacts as it would natively.
#[tokio::test]
async fn unknown_invocation_stays_prompt_text() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/imagegen a cat"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    let text = prompts[0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("prompt part: {}", prompts[0]));
    assert_eq!(text, "/imagegen a cat");
    assert!(fake.posts_to("/session/ses_test/command").is_empty());

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn first_prompt_waits_for_the_live_event_subscription() {
    // The v1 bus has no replay: a fast-failing turn (bad model id) emits
    // busy → session.error → idle within ~200ms of the prompt. Prompting
    // before the SSE stream exists loses the whole turn (observed live,
    // 1.18.21) — the driver must gate the first prompt on the connection.
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged
    wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(
        *fake.first_prompt_had_subscriber.lock().unwrap(),
        Some(true),
        "prompt must not be posted before the /global/event subscription exists"
    );

    // And the fast-failure lifecycle settles promptly (all three frames in
    // one burst), not via the stall watchdog.
    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": "ses_test", "status": { "type": "busy" } },
    }));
    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "UnknownError",
            "data": { "message": "Model not found: opencode/gone-model" },
        }},
    }));
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(e),
            ..
        }) if e.contains("Model not found")
    ));
}

// ---------------------------------------------------------------------------
// Native plan mode (ARCHITECTURE.md §11.2, OpenCode row)
// ---------------------------------------------------------------------------

fn keep_planning(feedback: Option<&str>) -> PlanDecision {
    PlanDecision::keep_planning(feedback.map(str::to_owned))
}

/// The requested mode IS the `agent` field, on every prompt — and opencode
/// has no other switch, so a toggle mid-run lands on the next one.
#[tokio::test]
async fn plan_mode_rides_the_agent_and_a_toggle_lands_on_the_next_prompt() {
    let fake = FakeOpencode::start().await;
    assert!(
        harness(&fake).plan_mode(),
        "the composer toggle is gated on this"
    );
    let (controls, steer, mode, _gate) = plan_controls(true, keep_planning(None));
    let mut req = request("plan it");
    req.plan_mode = true;
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let ev = next_event(&mut stream).await;
    assert!(
        matches!(&ev, AgentEvent::PlanModeChanged { active: true }),
        "the run reports the agent it sent, got {ev:?}"
    );

    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(prompts[0]["agent"], "plan");

    // Toggled off mid-turn: the queued steer carries the new agent.
    mode.send(false).expect("toggle");
    assistant_message(&fake, "ses_test", "msg_1");
    steer
        .send(SteerMessage {
            prompt: "now build it".into(),
            message_id: None,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    idle(&fake, "ses_test");
    let ev = next_event(&mut stream).await;
    assert!(matches!(&ev, AgentEvent::Steered { .. }), "got {ev:?}");
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 2).await;
    assert_eq!(prompts[1]["parts"][0]["text"], "now build it");
    assert_eq!(prompts[1]["agent"], "build");

    assistant_message(&fake, "ses_test", "msg_2");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn a_run_outside_plan_mode_prompts_as_the_build_agent() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let ev = next_event(&mut stream).await;
    assert!(matches!(&ev, AgentEvent::PlanModeChanged { active: false }));
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(prompts[0]["agent"], "build");

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// The plan is the file the plan agent writes (it may write nowhere else):
/// each completed edit/write on it is re-read from disk, once.
#[tokio::test]
async fn a_completed_plan_file_edit_streams_the_plan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plans = dir.path().join(".opencode").join("plans");
    std::fs::create_dir_all(&plans).expect("plans dir");
    let plan = plans.join("1-veil-port.md");
    std::fs::write(&plan, "# Veil port\n\n1. Port the veil.\n").expect("plan file");

    let fake = FakeOpencode::start().await;
    let (controls, _steer, _mode, _gate) = plan_controls(true, keep_planning(None));
    let mut req = request("plan it");
    req.plan_mode = true;
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    let part = json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_w", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "edit", "callID": "call_edit",
            "state": {
                "status": "completed",
                "input": { "filePath": plan.to_str().unwrap(), "oldString": "", "newString": "x" },
                "output": "written",
            },
        }},
    });
    fake.emit(part.clone());
    let _ = next_event(&mut stream).await; // ToolCall
    let _ = next_event(&mut stream).await; // ToolResult
    let ev = next_event(&mut stream).await;
    assert!(
        matches!(
            &ev,
            AgentEvent::PlanUpdated { text, path }
                if text == "# Veil port\n\n1. Port the veil.\n"
                    && path.as_deref() == plan.to_str()
        ),
        "got {ev:?}"
    );

    // The same snapshot re-delivered must not repeat the plan.
    fake.emit(part);
    let quiet = tokio::time::timeout(Duration::from_millis(400), stream.next()).await;
    assert!(quiet.is_err(), "a re-delivered snapshot repeated the plan");

    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// The reported mode is what the opencode TUI itself listens for: a
/// completed `plan_enter` / `plan_exit` tool part. The plan card already
/// represents the gate, so those parts carry the signal and NO tool chip —
/// a rejected `plan_exit` must not render as a failed tool.
#[tokio::test]
async fn plan_gate_parts_signal_the_mode_without_a_chip() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _mode, _gate) = plan_controls(false, keep_planning(None));
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged { false }

    assistant_message(&fake, "ses_test", "msg_1");
    let tool_part = |part_id: &str, tool: &str, call: &str, state: serde_json::Value| {
        json!({
            "type": "message.part.updated",
            "properties": { "part": {
                "id": part_id, "messageID": "msg_1", "sessionID": "ses_test",
                "type": "tool", "tool": tool, "callID": call,
                "state": state,
            }},
        })
    };
    let completed = || json!({ "status": "completed", "input": {}, "output": "ok" });

    fake.emit(tool_part("prt_e", "plan_enter", "call_enter", completed()));
    let ev = next_event(&mut stream).await;
    assert!(
        matches!(&ev, AgentEvent::PlanModeChanged { active: true }),
        "the gate part must yield the signal and no chip, got {ev:?}"
    );

    fake.emit(tool_part("prt_x", "plan_exit", "call_exit", completed()));
    let ev = next_event(&mut stream).await;
    assert!(
        matches!(&ev, AgentEvent::PlanModeChanged { active: false }),
        "got {ev:?}"
    );

    // A rejected ("No") plan_exit: neither a chip nor a mode change.
    fake.emit(tool_part(
        "prt_r",
        "plan_exit",
        "call_reject",
        json!({ "status": "error", "input": {}, "error": "rejected" }),
    ));
    let quiet = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
    assert!(
        quiet.is_err(),
        "a rejected plan_exit emitted {quiet:?} instead of nothing"
    );

    // An ordinary tool still chips as before.
    fake.emit(tool_part("prt_b", "bash", "call_bash", completed()));
    let ev = next_event(&mut stream).await;
    assert!(matches!(&ev, AgentEvent::ToolCall { .. }), "got {ev:?}");
    let ev = next_event(&mut stream).await;
    assert!(matches!(&ev, AgentEvent::ToolResult { .. }), "got {ev:?}");

    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// The gate: `plan_exit` asks its "Build Agent" question bound to its own
/// tool call. That question is the plan decision, not an ordinary ask.
/// Answering it is the WHOLE of the adapter's job: "keep planning" feedback
/// is delivered by the engine as the user's next message on the ordinary
/// steer path, so the adapter posts no prompt of its own.
#[tokio::test]
async fn the_plan_exit_question_is_the_exit_gate() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _mode, gate) = plan_controls(true, keep_planning(Some("shorter")));
    let mut req = request("plan it");
    req.plan_mode = true;
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    // The running plan_exit part binds the callID the question names.
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_x", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "plan_exit", "callID": "call_exit",
            "state": { "status": "running", "input": {} },
        }},
    }));
    fake.emit(json!({
        "type": "question.asked",
        "properties": {
            "id": "que_gate",
            "sessionID": "ses_test",
            "questions": [{
                "question": "Plan at .opencode/plans/1-veil-port.md is complete. \
                             Would you like to switch to the build agent and start implementing?",
                "header": "Build Agent",
                "options": [
                    {"label": "Yes", "description": "Switch to build agent and start implementing the plan"},
                    {"label": "No", "description": "Stay with plan agent to continue refining the plan"},
                ],
            }],
            "tool": { "messageID": "msg_1", "callID": "call_exit" },
        },
    }));

    let replies = wait_posts(&fake, "/question/que_gate/reply", 1).await;
    assert_eq!(replies[0]["answers"][0][0], "No");
    assert_eq!(
        *gate.lock().unwrap(),
        1,
        "the plan bridge answered the gate"
    );
    // The feedback is the ENGINE's to deliver: no follow-up prompt here.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fake.posts_to("/session/ses_test/prompt_async").len(),
        1,
        "the adapter posted the feedback itself"
    );

    // An unrelated question is still an ordinary ask.
    fake.emit(json!({
        "type": "question.asked",
        "properties": {
            "id": "que_plain",
            "sessionID": "ses_test",
            "questions": [{
                "question": "Which color?",
                "header": "Color",
                "options": [{"label": "Red", "description": "warm"}],
            }],
        },
    }));
    let ev = next_event(&mut stream).await;
    assert!(
        matches!(
            &ev,
            AgentEvent::InputRequested { request_id, .. } if request_id == "que_plain"
        ),
        "the gate must not swallow ordinary questions, got {ev:?}"
    );
    let replies = wait_posts(&fake, "/question/que_plain/reply", 1).await;
    assert_eq!(replies[0]["answers"][0][0], "Red");

    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

/// Approve: the answer is the whole message. opencode itself injects the
/// build message and switches the agent — the adapter sends nothing else.
#[tokio::test]
async fn an_approved_plan_gate_answers_yes_and_prompts_nothing() {
    let fake = FakeOpencode::start().await;
    let decision = PlanDecision::approve();
    let (controls, _steer, _mode, gate) = plan_controls(true, decision);
    let mut req = request("plan it");
    req.plan_mode = true;
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // PlanModeChanged

    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_x", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "plan_exit", "callID": "call_exit",
            "state": { "status": "running", "input": {} },
        }},
    }));
    fake.emit(json!({
        "type": "question.asked",
        "properties": {
            "id": "que_gate",
            "sessionID": "ses_test",
            "questions": [{
                "question": "Plan is complete. Switch to the build agent?",
                "header": "Build Agent",
                "options": [{"label": "Yes"}, {"label": "No"}],
            }],
            "tool": { "messageID": "msg_1", "callID": "call_exit" },
        },
    }));

    let replies = wait_posts(&fake, "/question/que_gate/reply", 1).await;
    assert_eq!(replies[0]["answers"][0][0], "Yes");
    assert_eq!(*gate.lock().unwrap(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fake.posts_to("/session/ses_test/prompt_async").len(),
        1,
        "approval must not add a prompt of comet's own"
    );

    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn models_discover_from_the_provider_catalog() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [
            {
                "id": "opencode",
                "name": "OpenCode Zen",
                "models": { "big-pickle": { "name": "Big Pickle" } },
            },
            {
                "id": "catalog-only",
                "name": "Needs A Key",
                "models": { "locked": { "name": "Locked" } },
            },
        ],
        "connected": ["opencode"],
    }));
    let harness = harness(&fake);
    let models = harness.models().await.expect("models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "opencode/big-pickle");
    // Commands were primed off the same probe.
    let commands = harness.commands(None).await.expect("commands");
    assert_eq!(commands[0].name, "init");
}

/// §10.4 "One discovery path": the run fetches `/command` for its own slash
/// routing, but the catalog never rides the run stream — `commands()` is the
/// only source the composer has.
#[tokio::test]
async fn run_stream_carries_no_catalog_event() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::AvailableCommands { .. })),
        "the retired run-time catalog event was emitted: {events:?}"
    );
    // The catalog itself is still reachable — through the probe alone.
    assert!(
        !harness(&fake)
            .commands(None)
            .await
            .expect("commands")
            .is_empty()
    );
}

/// §10.4: the catalog is cwd-scoped, so the probe asks `/command` about the
/// directory the run would use — the server's own per-request instance
/// selector — and caches per directory.
#[tokio::test]
async fn commands_probe_scopes_the_listing_to_the_requested_directory() {
    let fake = FakeOpencode::start().await;
    let harness = harness(&fake);
    let names = |commands: &[comet_proto::SlashCommand]| {
        commands.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    };

    let project = harness
        .commands(Some(std::path::Path::new("/w/project")))
        .await
        .expect("commands");
    assert_eq!(names(&project), ["init", "cometalpha", "project-skill"]);
    assert_eq!(
        project[2].description, "/w/project",
        "the probe scoped the request to the requested cwd: {project:?}"
    );

    // Another directory on the same instance is its own catalog, not the
    // cached one; no cwd asks the server for its unscoped listing.
    let other = harness
        .commands(Some(std::path::Path::new("/w/other")))
        .await
        .expect("commands");
    assert_eq!(other[2].description, "/w/other");
    let none = harness.commands(None).await.expect("commands");
    assert_eq!(names(&none), ["init", "cometalpha"]);

    assert_eq!(
        *fake.command_directories.lock().unwrap(),
        [
            Some("/w/project".to_owned()),
            Some("/w/other".to_owned()),
            None
        ],
        "one `/command` per directory, each carrying its own scope"
    );
}

/// Live discovery against a real `opencode serve`: `cargo test -p
/// comet-harness --test opencode -- --ignored live_commands`. Boots the
/// server, reads `GET /command`, tears it down — no model turn, no cost.
///
/// §10.4 evidence that the catalog is the server's, not comet's: the list is
/// whatever `/command` returns, skills (`source: "skill"`) included.
#[tokio::test]
#[ignore]
async fn live_commands_discovery() {
    let h = OpencodeHarness::new();
    let commands = h.commands(None).await.expect("live discovery");
    assert!(
        !commands.is_empty(),
        "opencode ships built-in commands (/init, /review)"
    );
    assert!(commands.iter().any(|c| c.name == "init"));
    eprintln!(
        "{} commands: {:?}",
        commands.len(),
        commands.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    // §10.4 evidence that the catalog is cwd-scoped and the SERVER resolves
    // it: a skill under the probe directory's `.opencode/skill` is offered
    // only for the probe that scopes its request to that directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = dir
        .path()
        .join(".opencode")
        .join("skill")
        .join("comet-live-probe");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: comet-live-probe\ndescription: comet live opencode discovery probe.\n---\n\nProbe body.\n",
    )
    .expect("SKILL.md");
    let scoped = h
        .commands(Some(dir.path()))
        .await
        .expect("cwd-scoped live discovery");
    eprintln!(
        "{} commands in {}: {:?}",
        scoped.len(),
        dir.path().display(),
        scoped.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        scoped.iter().any(|c| c.name == "comet-live-probe"),
        "the project skill under the probe cwd is missing"
    );
    assert!(
        commands.iter().all(|c| c.name != "comet-live-probe"),
        "the cwd-less catalog must not carry a project skill"
    );
}
