//! CodexHarness integration tests against the fake app server in
//! `tests/fixtures/fake-codex.sh` (no real `codex` binary involved).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use comet_harness::{
    CancellationToken, CodexHarness, Harness, HarnessError, RunControls, SteerMessage,
};
use comet_proto::{
    AgentEvent, DoneStatus, HarnessId, ReasoningLevel, RunRequest, SandboxLevel, TodoItem,
    ToolCall, UserInputAnswer, UserInputQuestion,
};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-codex.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> CodexHarness {
    CodexHarness::new().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: Some("gpt-5.6-sol".into()),
        reasoning: Some(ReasoningLevel::Ultra),
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        plan_mode: false,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

/// Controls whose `request_input` answers every question with `answer_label`.
fn controls(
    answer_label: &'static str,
) -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        plan: comet_harness::PlanControls::off(),
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![answer_label.into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &CodexHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

#[tokio::test]
async fn happy_path_maps_deltas_items_usage_and_done() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:happy");
    req.cwd = "/tmp".into();
    req.model_options.insert(
        "serviceTier".into(),
        serde_json::Value::String("fast".into()),
    );
    let events = run_to_end(&harness(), req, controls).await;

    // SessionStarted from thread/start's thread id.
    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStarted {
                harness,
                model,
                cwd,
                session_id,
                ..
            } => Some((harness, model, cwd, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "{events:?}");
    let (h, model, cwd, session_id) = starts[0];
    assert_eq!(*h, HarnessId::Codex);
    assert_eq!(model, "gpt-5.6-sol");
    assert_eq!(cwd, "/tmp");
    assert_eq!(session_id, "th-1");

    // Deltas — both wire spellings accepted.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking hard".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "summary".into()
    }));

    // commandExecution: ToolCall at started only, exit code 1 => error result.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "c1"))
            .count(),
        1
    );
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "c1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "c1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    // fileChange (single add): WriteFile, refreshed at completion.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::ToolCall {
                    id,
                    call: ToolCall::WriteFile { path, content: None }
                } if id == "f1" && path == "/tmp/new.rs"
            ))
            .count(),
        2,
        "started + completion-refresh: {events:?}"
    );
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "f1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // mcpToolCall with failed status.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "mcp1".into(),
        call: ToolCall::Mcp {
            server: "linear".into(),
            tool: "search".into(),
            input: Some(serde_json::json!({"q": "bug"})),
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "mcp1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    // webSearch lifecycle.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "w1".into(),
        call: ToolCall::WebSearch {
            query: "rust".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "w1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // Completion-only todoList still opens and closes the lifecycle.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "td1".into(),
        call: ToolCall::Todo {
            items: vec![
                TodoItem {
                    text: "a".into(),
                    done: true
                },
                TodoItem {
                    text: "b".into(),
                    done: false
                },
            ]
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "td1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // Streamed agentMessage must not re-emit its completed text…
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "Hello world")),
        "streamed message text re-emitted: {events:?}"
    );
    // …but a never-streamed one falls back to the completed text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "unstreamed tail".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
            .count(),
        2
    );

    // Usage rides just before the terminal Done.
    let usage_pos = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Usage {
                    input_tokens: 42,
                    output_tokens: 7
                }
            )
        })
        .expect("usage emitted");
    let done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("done emitted");
    assert!(usage_pos < done_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn steering_uses_turn_steer_with_expected_turn_id() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    let steered = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Steered {
                assistant_message_id,
                next_assistant_message_id,
            } => Some((
                assistant_message_id.clone(),
                next_assistant_message_id.clone(),
            )),
            _ => None,
        })
        .expect("Steered emitted: {events:?}");
    assert!(steered.0.is_some() && steered.1.is_some());
    assert_ne!(steered.0, steered.1);

    // The fake only emits this delta after verifying expectedTurnId + text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn rejected_steer_falls_back_to_a_follow_up_turn() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer-race"), controls).await;

    // Two turns: the raced one completes, then the fallback carries the steer.
    let dones: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        dones,
        vec![DoneStatus::Completed, DoneStatus::Completed],
        "{events:?}"
    );
    let steered_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("Steered emitted on fallback");
    let first_done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("first done");
    assert!(
        first_done_pos < steered_pos,
        "fallback turn starts after the raced turn ends: {events:?}"
    );
    // Only emitted by the fake when the fallback turn/start carried the text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "fallback".into()
    }));
}

#[tokio::test]
async fn approvals_round_trip_as_input_requests() {
    // Approvals must reach the ENGINE's input bridge (`request_input`) — and
    // the harness must NOT emit its own `InputRequested`/`InputResolved`
    // twins: the bridge owns that lifecycle (it mints the request id the
    // resolver is parked under; a harness-emitted copy folded an unanswerable
    // duplicate chip into the doc).
    let asked: Arc<Mutex<Vec<UserInputQuestion>>> = Arc::new(Mutex::new(Vec::new()));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let _steer = steer_tx;
    let token = CancellationToken::new();
    let seen = asked.clone();
    let controls = RunControls {
        plan: comet_harness::PlanControls::off(),
        request_input: Box::new(move |questions| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["Yes".into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    let mut req = request("scenario:approve");
    req.auto_approve = false;
    let events = run_to_end(&harness(), req, controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2, "{events:?}");
    assert_eq!(asked[0].header, "Approve command");
    assert!(asked[0].question.contains("rm -rf /tmp/x"));
    assert_eq!(asked[0].options, vec!["Yes".to_string(), "No".to_string()]);
    assert_eq!(asked[1].header, "Approve file change");
    assert!(asked[1].question.contains("/tmp/a.rs"));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "harness must not emit input lifecycle events itself: {events:?}"
    );

    // The fake only completes the turn after seeing BOTH accept decisions.
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn approval_no_answer_becomes_decline() {
    let (controls, _steer, _token) = controls("No");
    let mut req = request("scenario:decline");
    req.auto_approve = false;
    let events = run_to_end(&harness(), req, controls).await;

    // The fake only completes the turn after seeing the decline decision.
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn interrupt_sends_turn_interrupt_and_maps_aborted() {
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness()
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel(); // interrupt mid-turn
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn unresponsive_child_is_reaped_with_interrupted_done() {
    let harness = CodexHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness
        .run(request("scenario:wedge"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("escalation completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn turn_failed_maps_to_errored_done() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:fail"), controls).await;
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some("boom".into()),
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn resume_falls_back_to_fresh_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-fail".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-fresh"
        )),
        "fresh thread expected: {events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-fresh".into()),
        })
    );
}

#[tokio::test]
async fn resume_reuses_the_existing_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-ok".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-resumed"
        )),
        "resumed thread expected: {events:?}"
    );
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let harness = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    let (controls, _steer, _token) = controls("Yes");
    let err = harness
        .run(request("scenario:happy"), controls)
        .await
        .err()
        .expect("spawn fails");
    assert!(matches!(err, HarnessError::NotInstalled(_)), "{err:?}");
}

#[tokio::test]
async fn models_returns_curated_catalog() {
    let models = harness().models().await.expect("models");
    assert_eq!(models.len(), 7);
    assert_eq!(models[0].id, "gpt-5.6-sol");
    assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
    assert!(
        models
            .iter()
            .all(|m| m.options.iter().any(|o| o.id == "serviceTier"))
    );

    let missing = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    // models() requires a resolvable binary… but with_executable trusts the
    // caller's path, so only the default resolution can report NotInstalled —
    // exercise the harness identity surface instead.
    assert_eq!(missing.id(), HarnessId::Codex);
    // "Codex" — comet composer/defaults.ts HARNESS_LABEL (and the registry's
    // lazy descriptor must stay in lockstep).
    assert_eq!(missing.display_name(), "Codex");
    assert_eq!(missing.reasoning_levels().len(), 7);
}

/// ARCHITECTURE.md §11.2: codex has no client-settable collaboration mode, so
/// the composer's toggle stays hidden — the descriptor, not the harness id,
/// decides.
#[tokio::test]
async fn plan_mode_is_unsupported_on_the_codex_wire() {
    assert!(!harness().plan_mode());
}

/// The READ-ONLY half: a thread codex itself put in plan mode still reports
/// the mode and renders its plan.
#[tokio::test]
async fn plan_item_and_reported_mode_map_to_plan_events() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:plan"), controls).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::PlanModeChanged { active: true })),
        "thread/settings/updated must report the plan collaboration mode: {events:?}"
    );

    let plans: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::PlanUpdated { text, path } => {
                assert_eq!(*path, None, "codex keeps no plan file on this wire");
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        plans,
        vec![
            // The started item reports nothing; each delta reports the
            // accumulation so far…
            "# Plan\n\n",
            "# Plan\n\n1. Look",
            // …and the completed item's text is authoritative (the schema
            // warns the deltas may not concatenate to it).
            "# Plan\n\n1. Look around\n2. Report back",
        ],
        "{events:?}"
    );
    // The child thread's plan delta is consumed, never folded into the
    // parent's plan.
    assert!(
        plans.iter().all(|p| !p.contains("child plan")),
        "{events:?}"
    );
}

/// Tripwire (ARCHITECTURE.md §11.2): the day `thread/start`/`turn/start` grow a
/// collaboration mode, codex gets native plan mode and this test says so.
/// `cargo test -p comet-harness --test codex -- --ignored live_plan_schema`.
#[test]
#[ignore = "runs the real codex CLI to regenerate the app-server JSON schema"]
fn live_plan_schema_tripwire() {
    let exe = std::env::var("CODEX_EXECUTABLE").unwrap_or_else(|_| "codex".into());
    let dir = tempfile::tempdir().expect("tempdir");
    let out = match std::process::Command::new(&exe)
        .args(["app-server", "generate-json-schema", "--out"])
        .arg(dir.path())
        .output()
    {
        Ok(out) => out,
        // Not installed on this machine: nothing to check.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("codex not installed — skipping the plan-schema tripwire");
            return;
        }
        Err(e) => panic!("codex app-server generate-json-schema: {e}"),
    };
    assert!(
        out.status.success(),
        "generate-json-schema failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for params in ["TurnStartParams.json", "ThreadStartParams.json"] {
        let path = dir.path().join("v2").join(params);
        let schema: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {params}: {e}")),
        )
        .unwrap_or_else(|e| panic!("parse {params}: {e}"));
        let properties = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{params} has no properties"));
        assert!(
            !properties
                .keys()
                .any(|k| k == "collaborationMode" || k == "collaboration_mode"),
            "Codex app-server now accepts a collaboration mode on thread/turn start \
             — implement native plan mode (ARCHITECTURE.md §11.2)"
        );
    }
}

#[tokio::test]
async fn child_thread_routing_tags_and_never_settles_parent() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:subagent"), controls).await;

    // Exactly one Done — the child's turn/completed must NOT settle the
    // parent turn (the swallowed-catch-all bug class this table exists for).
    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 1, "one parent Done only: {events:?}");

    // Parent output that follows the child's turn/completed still streams.
    let late_parent = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta after child turn end");
    assert!(late_parent < dones[0]);

    // The spawn chip lives on the parent feed, named from the agent path,
    // and resolves when the activity completes.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "call_alpha" && name == "Agent: alpha"
    )));
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, is_error: false, .. } if id == "call_alpha")
    ));
    // The root's own subAgentActivity produces no chip.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "call_root")),
        "root self-activity must not register or render: {events:?}"
    );

    // Child deltas and items arrive tagged with the spawn call id — never
    // bare (child threads stream deltas on this wire; live-verified 0.146.1).
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "child says hi".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "cs1".into(),
            call: ToolCall::Exec {
                command: "echo hi".into()
            },
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolResult {
            id: "cs1".into(),
            is_error: false,
            output: None,
            diff: None,
        }),
    }));
    // The parent's steer (a userMessage item on the CHILD thread) arrives as
    // exactly one tagged UserMessage — completed only, never doubled by the
    // started lifecycle event, never leaked untagged.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "also check the rebuild")
            ))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { .. })),
        "steer leaked into the parent feed: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "cs1")),
        "child tool call leaked into the parent feed: {events:?}"
    );

    // The child's turn/completed (and later thread/closed) become tagged
    // terminal events — real fan-outs never call close_agent, so the turn
    // end is what flips the chip off "running".
    assert!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
            ))
            .count()
            >= 1,
        "{events:?}"
    );
    // The tagged terminal must arrive from turn/completed — BEFORE the
    // parent delta that follows it in the script (not only at thread/closed).
    let child_done = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { .. })
            )
        })
        .expect("tagged done");
    let late_parent_delta = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta");
    assert!(child_done < late_parent_delta, "{events:?}");
}

/// Live smoke against the REAL codex app-server (0.146.x, installed + authed):
/// one trivial turn, ending on turn/completed.
/// `cargo test -p comet-harness --test codex -- --ignored`.
#[tokio::test]
#[ignore = "spawns the real codex app-server; needs install + auth + network"]
async fn live_real_app_server_single_turn() {
    let harness = CodexHarness::new();
    let mut req = request("Reply with exactly the word: pong");
    req.cwd = std::env::temp_dir().display().to_string();
    let (controls, _steer, _token) = controls("Yes");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    // The session parks after the turn (steering mailbox open) — collect up
    // to the first Done, not stream end.
    let events = tokio::time::timeout(Duration::from_secs(120), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished in time");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Slash-command discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commands_come_from_skills_list() {
    let h = harness();
    let commands = h.commands(None).await.expect("discovery succeeds");
    assert_eq!(
        commands.len(),
        2,
        "same-name skills across cwd groups dedupe: {commands:?}"
    );
    assert_eq!(commands[0].name, "imagegen");
    assert_eq!(
        commands[0].description, "Generate or edit images",
        "interface.shortDescription wins over the model-facing paragraph"
    );
    assert_eq!(commands[1].name, "bare");
    assert_eq!(
        commands[1].description, "No interface block",
        "top-level description is the fallback"
    );
    assert_eq!(h.commands(None).await.expect("cache hit"), commands);
}

/// §10.4: the catalog is cwd-scoped, so the probe asks `skills/list` about the
/// directory the run would use — `{"cwds": [cwd]}` — and the repo-scoped group
/// codex answers with leads the list, ahead of the user-scoped copies.
#[tokio::test]
async fn commands_probe_scopes_skills_list_to_the_requested_cwd() {
    let h = harness();
    let project = h
        .commands(Some(std::path::Path::new("/w/project")))
        .await
        .expect("discovery succeeds");
    assert_eq!(project[0].name, "project-skill");
    assert_eq!(
        project[0].description, "/w/project",
        "the probe named the requested cwd in `cwds`: {project:?}"
    );
    // Same instance, another directory: its own catalog, not the cached one.
    let other = h
        .commands(Some(std::path::Path::new("/w/other")))
        .await
        .expect("discovery succeeds");
    assert_eq!(other[0].description, "/w/other");
    // And without a cwd the request carries no `cwds` at all, so codex answers
    // with the groups it lists on its own.
    let none = h.commands(None).await.expect("discovery succeeds");
    assert!(none.iter().all(|c| c.name != "project-skill"), "{none:?}");
}

/// `enabled: false` is a `[[skills.config]]` opt-out in `config.toml`; codex's
/// own pickers do not offer those, so neither does the catalog.
#[tokio::test]
async fn disabled_skills_never_reach_the_commands_catalog() {
    let commands = harness().commands(None).await.expect("discovery succeeds");
    assert!(
        !commands.iter().any(|c| c.name == "switched-off"),
        "a skill the wire reports as enabled:false must not be offered: {commands:?}"
    );
}

// ---------------------------------------------------------------------------
// Slash parity (ARCHITECTURE.md §10.5)
// ---------------------------------------------------------------------------

/// Runs `prompt` (and optionally one steer) against the fake app server in a
/// fresh cwd carrying the `comet-turns.jsonl` sentinel, and returns the turn
/// frames the fixture recorded there.
async fn recorded_turn_frames(prompt: &str, steer: Option<&str>) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("comet-turns.jsonl");
    std::fs::write(&log, "").expect("recording sentinel");
    let (controls, steer_tx, _token) = controls("Yes");
    if let Some(text) = steer {
        steer_tx
            .send(SteerMessage {
                prompt: text.into(),
                message_id: None,
            })
            .await
            .expect("steer queued");
    }
    let mut req = request(prompt);
    req.cwd = dir.path().to_string_lossy().into_owned();
    run_to_end(&harness(), req, controls).await;
    std::fs::read_to_string(&log)
        .expect("recorded turns")
        .lines()
        .map(|line| serde_json::from_str(line).expect("turn frame is json"))
        .collect()
}

/// The codex TUI submits the typed text first and appends a `skill` item for
/// every skill the text mentions with `$name`; `/name args` must produce that
/// exact frame.
#[tokio::test]
async fn slash_invocation_parity_sends_the_native_skill_input_item() {
    let frames = recorded_turn_frames("/imagegen scenario:parity a cat", None).await;
    let frame = frames.first().expect("a turn was recorded");
    assert_eq!(frame["method"], "turn/start");
    assert_eq!(
        frame["params"]["input"],
        serde_json::json!([
            { "type": "text", "text": "$imagegen scenario:parity a cat" },
            { "type": "skill", "name": "imagegen", "path": "/w/.agents/skills/imagegen/SKILL.md" },
        ])
    );
}

/// A `/name` the session's own catalog does not list is sent verbatim, so the
/// CLI reacts exactly as it would natively.
#[tokio::test]
async fn unknown_slash_invocation_stays_plain_text() {
    let frames = recorded_turn_frames("/nope scenario:parity hi", None).await;
    let frame = frames.first().expect("a turn was recorded");
    assert_eq!(
        frame["params"]["input"],
        serde_json::json!([{ "type": "text", "text": "/nope scenario:parity hi" }])
    );
}

/// A disabled skill is not in the catalog, so its name is not translated
/// either — the enablement filter gates invocation, not just the picker.
#[tokio::test]
async fn disabled_skill_slash_invocation_stays_plain_text() {
    let frames = recorded_turn_frames("/switched-off scenario:parity go", None).await;
    let frame = frames.first().expect("a turn was recorded");
    assert_eq!(
        frame["params"]["input"],
        serde_json::json!([{ "type": "text", "text": "/switched-off scenario:parity go" }])
    );
}

/// The steer path translates too: `turn/steer` carries the same native frame.
#[tokio::test]
async fn steered_slash_invocation_parity_sends_the_native_skill_item() {
    let frames = recorded_turn_frames("scenario:parity-steer", Some("/imagegen a cat")).await;
    let steer = frames.get(1).expect("a steer was recorded");
    assert_eq!(steer["method"], "turn/steer");
    assert_eq!(steer["params"]["expectedTurnId"], "t-1");
    assert_eq!(
        steer["params"]["input"],
        serde_json::json!([
            { "type": "text", "text": "$imagegen a cat" },
            { "type": "skill", "name": "imagegen", "path": "/w/.agents/skills/imagegen/SKILL.md" },
        ])
    );
}

/// Live smoke against the real CLI: `cargo test -p comet-harness --test
/// codex -- --ignored live_commands`. Every skill turned off with
/// `[[skills.config]] enabled = false` in `~/.codex/config.toml` must be
/// absent — `skills/list` still reports those, flagged `enabled: false`.
#[tokio::test]
#[ignore]
async fn live_commands_discovery() {
    let h = CodexHarness::new();
    let commands = h.commands(None).await.expect("live discovery");
    let disabled = disabled_skill_names();
    eprintln!(
        "{} commands, {} disabled in config.toml, first: {:?}",
        commands.len(),
        disabled.len(),
        commands.first()
    );
    for name in disabled {
        assert!(
            !commands.iter().any(|c| c.name == name),
            "disabled skill {name:?} leaked into the catalog"
        );
    }

    // §10.4 evidence that the catalog is cwd-scoped and codex, not comet,
    // resolves it: a skill dropped into the probe directory's `.agents/skills`
    // is offered only for the probe that names that directory in `cwds`.
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = dir
        .path()
        .join(".agents")
        .join("skills")
        .join("comet-live-probe");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: comet-live-probe\ndescription: comet live codex discovery probe.\n---\n\nProbe body.\n",
    )
    .expect("SKILL.md");
    let scoped = h
        .commands(Some(dir.path()))
        .await
        .expect("cwd-scoped live discovery");
    assert!(
        scoped.iter().any(|c| c.name == "comet-live-probe"),
        "the project skill under the probe cwd is missing: {:?}",
        scoped.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        commands.iter().all(|c| c.name != "comet-live-probe"),
        "the cwd-less catalog must not carry a project skill"
    );
}

/// Names from `[[skills.config]]` blocks with `enabled = false` in the user's
/// `~/.codex/config.toml` (a flat two-key block; no toml dependency needed).
fn disabled_skill_names() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let Ok(config) = std::fs::read_to_string(PathBuf::from(home).join(".codex/config.toml")) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut in_block = false;
    let mut disabled = false;
    let mut name: Option<String> = None;
    for line in config.lines().chain(std::iter::once("[end]")) {
        let line = line.trim();
        if line.starts_with('[') {
            if in_block
                && disabled
                && let Some(name) = name.take()
            {
                names.push(name);
            }
            in_block = line == "[[skills.config]]";
            disabled = false;
            name = None;
        } else if in_block {
            if line == "enabled = false" {
                disabled = true;
            } else if let Some(value) = line.strip_prefix("name = ") {
                name = Some(value.trim_matches('"').to_owned());
            }
        }
    }
    names
}
