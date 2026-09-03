//! AcpHarness integration tests against the fake ACP agent in
//! `tests/fixtures/fake-acp.sh` (no real `grok` binary involved).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot, watch};

use comet_harness::{AcpHarness, CancellationToken, Harness, RunControls, SteerMessage};
use comet_proto::{
    AgentEvent, DoneStatus, HarnessId, PlanDecision, RunRequest, SandboxLevel, SteeringMode,
    TodoItem, ToolCall, UserInputAnswer, UserInputQuestion,
};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-acp.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> AcpHarness {
    AcpHarness::grok().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: Some("grok-4.5".into()),
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        plan_mode: false,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
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
                    labels: vec!["Yes".into()],
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
    harness: &AcpHarness,
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

fn dones(events: &[AgentEvent]) -> Vec<(DoneStatus, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn happy_path_maps_chunks_tools_diffs_plans_and_commands() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:happy"), controls).await;

    // SessionStarted from session/new's id.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { harness, session_id, cwd, .. }
                if *harness == HarnessId::Grok && session_id == "s-1" && cwd == "/tmp"
        )),
        "{events:?}"
    );

    // The catalog is the probe's alone (§10.4 "One discovery path"): the
    // fixture advertises commands in the handshake AND mid-run, and neither
    // reaches the run stream.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::AvailableCommands { .. })),
        "the retired run-time catalog event was emitted: {events:?}"
    );

    // Chunks; the wrong-session and non-text chunks never surface.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking".into()
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("WRONG"))),
        "{events:?}"
    );

    // Execute tool: pending opens the call, the completed update resolves it
    // with capped multi-line output (newlines preserved verbatim).
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t1".into(),
        call: ToolCall::Exec {
            command: "cargo test -p comet-harness".into()
        },
    }));
    let exec_output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult {
                id,
                is_error: false,
                output: Some(output),
                ..
            } if id == "t1" => Some(output.clone()),
            _ => None,
        })
        .expect("exec output present");
    assert!(exec_output.starts_with("   Compiling comet-harness"));
    assert_eq!(exec_output.lines().count(), 6, "{exec_output:?}");

    // Edit tool: single-shot completed call carries the inline diff.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "t2".into(),
        call: ToolCall::EditFile {
            path: "/w/src/resolve.rs".into(),
            old_string: None,
            new_string: None,
        },
    }));
    let diff = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult {
                id,
                diff: Some(diff),
                ..
            } if id == "t2" => Some(diff.clone()),
            _ => None,
        })
        .expect("edit diff present");
    assert_eq!(diff.path, "/w/src/resolve.rs");
    assert!(
        diff.old_text
            .as_deref()
            .is_some_and(|t| t.contains(".filter(|p| p.exists())")),
        "{diff:?}"
    );
    assert!(diff.new_text.contains("split_paths"), "{diff:?}");

    // Plan → stable todo chip.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "acp-plan".into(),
        call: ToolCall::Todo {
            items: vec![
                TodoItem {
                    text: "read".into(),
                    done: true
                },
                TodoItem {
                    text: "fix".into(),
                    done: false
                },
            ]
        },
    }));

    // usage_update maps to nothing (context gauge, not per-turn tokens).
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));

    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn config_options_apply_requested_model_and_effort() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:config");
    req.reasoning = Some(comet_proto::ReasoningLevel::Medium);
    let events = run_to_end(&harness(), req, controls).await;
    // The fixture answers refusal unless BOTH set_config_option calls
    // (model grok-4.5, effort medium) arrived before the prompt.
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: "configured".into()
        }),
        "{events:?}"
    );
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn resumed_first_class_model_is_switched_before_prompt() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:model-api");
    req.resume = Some("existing-grok-session".into());
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: "model switched".into()
        }),
        "{events:?}"
    );
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn permission_requests_auto_accept_the_preferred_allow_option() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:permission"), controls).await;
    // The fixture answers refusal unless the harness selected "always".
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "approved".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

#[tokio::test]
async fn steering_extension_injects_mid_turn() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-ext"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first") {
                steer
                    .send(SteerMessage {
                        prompt: "redirect please".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

/// The steering response racing the turn's own end: the injection landed in
/// the dying turn, and the prompt response reached the wire first. The
/// boundary must still be emitted BEFORE the Done — a Steered after Done
/// re-armed the consumer (parked session → Working) with no next turn and no
/// Done ever coming (the stranded-Working / eternal-timer bug).
#[tokio::test]
async fn steer_racing_the_turn_end_never_emits_steered_after_done() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-race"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first") {
                steer
                    .send(SteerMessage {
                        prompt: "redirect please".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None)],
        "{events:?}"
    );
    let steered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("steer landed in the turn: a Steered boundary must exist");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("checked above");
    assert!(
        steered < done,
        "Steered after Done strands the session: {events:?}"
    );
}

#[tokio::test]
async fn rejected_steer_queues_and_delivers_at_the_turn_boundary() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:steer-queue"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        let mut steer = Some(steer);
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "first")
                && let Some(steer) = &steer
            {
                steer
                    .send(SteerMessage {
                        prompt: "redirect please".into(),
                        message_id: None,
                    })
                    .await
                    .expect("steer sent");
            }
            // Close the mailbox once the boundary turn streams so the
            // persistent session winds down and the stream ends.
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "boundary") {
                steer = None;
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");

    // First turn completes, then the queued steer becomes the boundary turn.
    assert_eq!(
        dones(&events),
        vec![(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "boundary".into()
    }));
}

#[tokio::test]
async fn interrupt_sends_session_cancel_and_ends_interrupted() {
    let (controls, _steer, token) = controls();
    let harness = harness();
    let stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("run finished in time");
    assert_eq!(dones(&events), vec![(DoneStatus::Interrupted, None)]);
}

#[tokio::test]
async fn wedged_agent_escalates_to_signals_and_still_ends_interrupted() {
    let (controls, _steer, token) = controls();
    let harness = harness().with_graces(Duration::from_millis(100), Duration::from_millis(200));
    let stream = harness
        .run(request("scenario:wedge"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        let mut stream = stream;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { ref text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("escalation reaped the child in time");
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    assert_eq!(dones[0].0, DoneStatus::Interrupted);
}

#[tokio::test]
async fn refusal_maps_to_an_errored_done() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness(), request("scenario:refusal"), controls).await;
    let dones = dones(&events);
    assert_eq!(dones.len(), 1);
    assert_eq!(dones[0].0, DoneStatus::Errored);
    assert!(dones[0].1.as_deref().unwrap_or("").contains("refused"));
}

#[tokio::test]
async fn resume_loads_the_session_and_drops_replayed_history() {
    let (controls, _steer, _token) = controls();
    let mut req = request("scenario:resumed");
    req.resume = Some("s-loaded".into());
    let events = run_to_end(&harness(), req, controls).await;
    // The 600-update replay is drained without surfacing…
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("old reply"))),
        "{events:?}"
    );
    // …the loaded session id sticks, and the live turn still streams.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "s-loaded"
    )));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "back again".into()
    }));
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}
#[test]
fn descriptor_surface_matches_registry_expectations() {
    let harness = AcpHarness::grok();
    assert_eq!(harness.id(), HarnessId::Grok);
    assert_eq!(harness.display_name(), "Grok");
    assert!(harness.supports_steering());
    assert_eq!(harness.steering_mode(), SteeringMode::TurnBoundary);
    assert_eq!(
        harness.reasoning_levels(),
        &[
            comet_proto::ReasoningLevel::Low,
            comet_proto::ReasoningLevel::Medium,
            comet_proto::ReasoningLevel::High,
        ]
    );
}

#[tokio::test]
async fn models_are_discovered_from_the_acp_session() {
    // ACP is the source of truth: the fixture advertises a model config
    // option, so the picker list comes from the wire, not the static catalog.
    let harness = AcpHarness::hermes().with_executable(fixture_path());
    let models = harness.models().await.expect("discovery");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["grok-4-fast", "grok-4.5"], "{models:?}");
    // Unmatched ids inherit the probe session's thought_level ladder.
    assert_eq!(
        models[0].reasoning_levels,
        vec![
            comet_proto::ReasoningLevel::Low,
            comet_proto::ReasoningLevel::Medium,
            comet_proto::ReasoningLevel::High,
        ],
        "{models:?}"
    );
    assert_eq!(models[0].description.as_deref(), Some("Fast tier"));
    // Cached: a second call returns the same list without respawning.
    let again = harness.models().await.expect("cached");
    assert_eq!(again, models);
}

#[tokio::test]
async fn models_enrich_from_the_static_catalog_on_id_match() {
    // grok's static catalog knows "grok-4.5" — the discovered entry keeps the
    // wire label but inherits the curated description and ladder.
    let harness = AcpHarness::grok().with_executable(fixture_path());
    let models = harness.models().await.expect("discovery");
    let grok45 = models
        .iter()
        .find(|m| m.id == "grok-4.5")
        .expect("grok-4.5");
    assert_eq!(
        grok45.description.as_deref(),
        Some("xAI's coding model — 500k context"),
        "{grok45:?}"
    );
}

#[tokio::test]
async fn models_fall_back_to_the_static_catalog_when_the_probe_fails() {
    let harness = AcpHarness::pi().with_executable("/nonexistent/never-a-pi-acp");
    let models = harness.models().await.expect("static fallback");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["default"], "{models:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn hung_handshake_errors_instead_of_spinning_forever() {
    // An agent that consumes stdin and never answers initialize — the
    // "thinking for minutes, then nothing" startup class (issue #93). The
    // run must end with a Done that names the timeout, not hang.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hung-agent.sh");
    // sleep inherits the stdio pipes and holds them open without ever
    // answering — a true wedge, not a crash.
    std::fs::write(&script, "#!/bin/sh\nexec sleep 1000\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let harness = AcpHarness::grok()
        .with_executable(&script)
        .with_handshake_timeout(Duration::from_millis(300));
    let (controls, _steer, _token) = controls();
    let events = run_to_end(&harness, request("hi"), controls).await;
    let dones = dones(&events);
    assert_eq!(dones.len(), 1, "{events:?}");
    let (status, error) = &dones[0];
    assert_eq!(*status, DoneStatus::Errored);
    let error = error.as_deref().unwrap_or_default();
    assert!(
        error.contains("did not complete the ACP handshake"),
        "{error}"
    );
}
#[test]
fn hermes_and_pi_descriptor_surfaces_match_registry_expectations() {
    let hermes = AcpHarness::hermes();
    assert_eq!(hermes.id(), HarnessId::Hermes);
    assert_eq!(hermes.display_name(), "Hermes");
    assert!(hermes.supports_steering());
    assert_eq!(hermes.steering_mode(), SteeringMode::TurnBoundary);
    assert!(hermes.reasoning_levels().is_empty());

    let pi = AcpHarness::pi();
    assert_eq!(pi.id(), HarnessId::Pi);
    assert_eq!(pi.display_name(), "Pi");
    assert!(pi.supports_steering());
    assert_eq!(pi.steering_mode(), SteeringMode::TurnBoundary);
    assert_eq!(
        pi.reasoning_levels(),
        &[
            comet_proto::ReasoningLevel::Minimal,
            comet_proto::ReasoningLevel::Low,
            comet_proto::ReasoningLevel::Medium,
            comet_proto::ReasoningLevel::High,
            comet_proto::ReasoningLevel::XHigh,
            comet_proto::ReasoningLevel::Max,
        ]
    );
}

#[tokio::test]
async fn prompt_complete_extension_settles_a_hung_prompt_response() {
    // The grok field hang: `_x.ai/session/prompt_complete` fires (echoing
    // the minted _meta.promptId) but the session/prompt RPC never answers.
    let (controls, _steer, _token) = controls();
    let mut stream = harness()
        .run(request("scenario:prompt-complete-hang"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async {
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
    .expect("notification settled the turn despite the hung RPC");
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "pong".into()
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn stale_prompt_complete_never_settles_a_newer_turn() {
    let (controls, _steer, _token) = controls();
    let mut stream = harness()
        .run(request("scenario:prompt-complete-stale"), controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async {
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
    .expect("real response settled the turn");
    // Exactly one Done, AFTER the real content — the stale/foreign
    // completions (emitted before the 1s pause) must not have settled first,
    // and must not have marked the turn Interrupted.
    let text = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "real answer"))
        .expect("real content precedes the settle");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("done");
    assert!(text < done, "{events:?}");
    assert!(matches!(
        &events[done],
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    ));
    // Grok-style `_meta` usage on the response is captured.
    assert!(events.contains(&AgentEvent::Usage {
        input_tokens: 9,
        output_tokens: 4
    }));
}

#[tokio::test]
async fn grok_subagent_lifecycle_tails_the_disk_transcript_into_tagged_events() {
    // The child session's chat_history.jsonl, one level under the sessions
    // root exactly like grok's `<root>/<urlencoded-cwd>/<session-id>/` layout
    // (entry shapes captured from a real 1.0.4 run).
    let tmp = tempfile::tempdir().unwrap();
    let child_dir = tmp.path().join("%2Ftmp").join("sub-1");
    std::fs::create_dir_all(&child_dir).unwrap();
    let history = child_dir.join("chat_history.jsonl");
    std::fs::write(
        &history,
        concat!(
            "{\"type\":\"system\",\"content\":\"You are a Grok Build subagent\"}\n",
            "{\"type\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Count the files.\"}],\"prompt_index\":0}\n",
            "{\"type\":\"reasoning\",\"id\":\"rs-1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Listing the directory.\"}],\"encrypted_content\":\"opaque\",\"status\":\"completed\"}\n",
            "{\"type\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"id\":\"call-1-0\",\"name\":\"run_terminal_command\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}],\"model_id\":\"grok-4.6-build\"}\n",
            "{\"type\":\"tool_result\",\"tool_call_id\":\"call-1-0\",\"content\":\"a.txt\\nb.txt\"}\n",
        ),
    )
    .unwrap();
    // A mid-run append: the tail must pick it up incrementally, before the
    // wire's subagent_finished lands (the fake agent sleeps 1.4s).
    let append_to = history.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(append_to)
            .unwrap();
        writeln!(
            f,
            "{}",
            "{\"type\":\"assistant\",\"content\":\"two files\",\"model_id\":\"grok-4.6-build\"}"
        )
        .unwrap();
    });

    let (controls, _steer, _token) = controls();
    let harness = harness().with_sessions_root(tmp.path());
    let events = run_to_end(&harness, request("scenario:subagent"), controls).await;

    // The spawn chip is named after the task, claude-driver parity.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
                if id == "sp1" && name == "Agent: Count files"
        )),
        "{events:?}"
    );

    // Tagged transcript: every wrapped event attributes to the spawn chip,
    // and the disk entries surfaced in order — reasoning, the typed tool
    // call + result, the mid-run append — then the lifecycle Done.
    let tagged: Vec<&AgentEvent> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } => {
                assert_eq!(parent_tool_use_id, "sp1", "{events:?}");
                Some(event.as_ref())
            }
            _ => None,
        })
        .collect();
    let pos = |pred: &dyn Fn(&AgentEvent) -> bool| tagged.iter().position(|e| pred(e));
    let reasoning = pos(&|e| {
        matches!(e, AgentEvent::ReasoningDelta { text } if text.starts_with("Listing the directory."))
    })
    .expect("reasoning entry tailed");
    let tool = pos(&|e| {
        matches!(
            e,
            AgentEvent::ToolCall { id, call: comet_proto::ToolCall::Exec { command } }
                if id == "call-1-0" && command == "ls"
        )
    })
    .expect("tool call typed from disk");
    let result = pos(&|e| {
        matches!(
            e,
            AgentEvent::ToolResult { id, is_error: false, output: Some(o), .. }
                if id == "call-1-0" && o.contains("a.txt")
        )
    })
    .expect("tool result tailed");
    let text =
        pos(&|e| matches!(e, AgentEvent::TextDelta { text } if text.starts_with("two files")))
            .expect("mid-run append tailed");
    let done = pos(&|e| {
        matches!(
            e,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )
    })
    .expect("tagged done from subagent_finished");
    assert!(
        reasoning < tool && tool < result && result < text && text < done,
        "{tagged:?}"
    );
    // The nested spawned update (another parent session) bound nothing —
    // every wrapped event attributed to sp1 (the assert in the filter) — and
    // the parent's own turn settled cleanly with its single untagged Done.
    assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
}

// ---------------------------------------------------------------------------
// Agent Skills as slash commands (ARCHITECTURE.md §10) — ACP agents.
//
// Grok, Hermes and pi-acp all advertise their invocables the same way: never
// in the handshake, always through `session/update: available_commands_update`
// after `session/new`. The shapes the fixture replays were captured live from
// grok 1.0.13, Hermes Agent 0.13.0, and pi-acp 0.0.33 driving pi 0.84.3.
// ---------------------------------------------------------------------------

/// `commands()` reads the agents' real `availableCommands` shapes: grok's
/// `_meta`-tagged skills with `input: null`, pi's `skill:<name>` entries with
/// no `input` key at all, and hermes' description-first built-ins.
///
/// Also the precedence: grok's handshake `_meta` carries a partial catalog
/// with no skills in it, so the session channel's list has to win.
#[tokio::test]
async fn discovery_lists_the_agents_real_skill_commands() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Puts the fixture in "real agent" mode: a partial handshake catalog, the
    // complete one on the session channel.
    std::fs::write(dir.path().join("comet-skill-probe"), "").expect("marker");

    let commands = harness()
        .commands(Some(dir.path()))
        .await
        .expect("discovery");
    let named = |n: &str| {
        commands
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} missing from {commands:#?}"))
    };

    // grok: a workflow carrying an argument hint, a bare-named user skill
    // (`input: null`), and the qualified names grok itself minted for the
    // names that collide with a built-in or another scope.
    assert_eq!(
        named("deep-research").input_hint.as_deref(),
        Some("<query>")
    );
    assert_eq!(named("automate").input_hint, None);
    assert_eq!(
        named("user:goal").description,
        "Set a goal that Cursor will pursue to completion."
    );
    named("vercel:workflow");
    // pi advertises every skill as `skill:<name>`, with no `input` member.
    assert_eq!(named("skill:rename-chat").input_hint, None);
    // hermes serializes description-first and hints its argument-takers.
    assert_eq!(
        named("model").input_hint.as_deref(),
        Some("model name to switch to")
    );
    named("help");

    // Comet adds nothing: no aliasing, no re-qualification, no filtering —
    // the catalog is the agent's list, in the agent's order (§10.4).
    assert_eq!(commands.len(), 9, "{commands:#?}");
    assert_eq!(commands[0].name, "compact");
    assert!(commands.iter().all(|c| c.aliases.is_empty()));
    // The session catalog REPLACES the handshake's partial one — a name only
    // the handshake advertised must not linger beside the real list.
    assert!(
        commands.iter().all(|c| c.name != "session-info"),
        "the partial handshake catalog leaked into the result: {commands:#?}"
    );

    // The probe stood in the project, not in $HOME: grok's catalog is
    // cwd-scoped (a project's `.agents/skills` joins the user-level ones).
    let seen = std::fs::read_to_string(dir.path().join("probe-cwd.txt")).expect("probe cwd");
    assert_eq!(seen, dir.path().display().to_string());
}

/// The other side of that precedence: an agent that refuses to advertise on
/// the session channel (no provider, not logged in) still surfaces whatever
/// its handshake carried, rather than an empty popup.
#[tokio::test]
async fn discovery_keeps_the_handshake_skill_commands_when_the_session_advertises_none() {
    let commands = harness().commands(None).await.expect("discovery");
    assert_eq!(commands.len(), 2, "{commands:#?}");
    assert_eq!(commands[0].name, "compact");
    assert_eq!(commands[1].name, "goal");
    assert_eq!(commands[1].input_hint.as_deref(), Some("the goal"));
}

/// §10.5 parity: ACP has no command RPC — an invocation IS the prompt text.
/// A `/name args` the catalog advertises must reach `session/prompt` byte for
/// byte, for grok's bare names and for pi's `skill:` ones alike.
#[tokio::test]
async fn slash_invocation_parity_sends_a_known_command_as_plain_text() {
    for prompt in ["/deep-research the acp command wire", "/skill:rename-chat"] {
        let (controls, _steer, _token) = controls();
        let events = run_to_end(&harness(), request(prompt), controls).await;
        assert!(
            events.contains(&AgentEvent::TextDelta {
                text: format!("invocation:{prompt}")
            }),
            "{prompt} was rewritten on the wire: {events:?}"
        );
        assert_eq!(dones(&events), vec![(DoneStatus::Completed, None)]);
    }
}

/// The other half of §10.5: a `/name` the catalog does not know is left
/// alone too, so the agent reacts exactly as it would in its own CLI.
#[tokio::test]
async fn slash_invocation_parity_leaves_an_unknown_command_as_text() {
    let (controls, _steer, _token) = controls();
    let prompt = "/not-a-command with args";
    let events = run_to_end(&harness(), request(prompt), controls).await;
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: format!("invocation:{prompt}")
        }),
        "{events:?}"
    );
}

// ---------------------------------------------------------------------------
// Live discovery against the real agents: `cargo test -p comet-harness --test
// acp -- --ignored live_commands`. Initialize + session/new only — no prompt,
// no model turn, no API cost.
// ---------------------------------------------------------------------------

/// Run the real probe, skipping cleanly where the agent is not installed on
/// this machine. Any other failure is a real one.
async fn live_catalog(
    h: &AcpHarness,
    cwd: Option<&std::path::Path>,
    agent: &str,
) -> Option<Vec<comet_proto::SlashCommand>> {
    match h.commands(cwd).await {
        Ok(commands) => Some(commands),
        Err(comet_harness::HarnessError::NotInstalled(hint)) => {
            eprintln!("skipping live {agent} discovery: not installed ({hint})");
            None
        }
        // A managed npm adapter that has not been fetched yet installs in the
        // background and errors this probe; that is a missing CLI, not a bug.
        Err(e) if e.to_string().contains("installing in the background") => {
            eprintln!("skipping live {agent} discovery: adapter still installing ({e})");
            None
        }
        Err(e) => panic!("live {agent} discovery failed: {e}"),
    }
}

fn report(agent: &str, commands: &[comet_proto::SlashCommand]) {
    eprintln!(
        "{agent}: {} commands, {} advertised as skill:<name>, first: {:?}",
        commands.len(),
        commands
            .iter()
            .filter(|c| c.name.starts_with("skill:"))
            .count(),
        commands.first(),
    );
}

/// §10.4 evidence for grok: the catalog is the CLI's and it is cwd-scoped —
/// a skill dropped into the probe directory's `.agents/skills` shows up
/// beside the built-ins, because grok read it, not because comet did.
#[tokio::test]
#[ignore]
async fn live_commands_grok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = dir
        .path()
        .join(".agents")
        .join("skills")
        .join("comet-live-probe");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: comet-live-probe\ndescription: comet live ACP discovery probe.\n---\n\nProbe body.\n",
    )
    .expect("SKILL.md");

    let h = AcpHarness::grok();
    let Some(commands) = live_catalog(&h, Some(dir.path()), "grok").await else {
        return;
    };
    assert!(
        commands.iter().any(|c| c.name == "compact"),
        "grok ships /compact: {commands:#?}"
    );
    assert!(
        commands.iter().any(|c| c.name == "comet-live-probe"),
        "the project skill under the probe cwd is missing: {:?}",
        commands.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    report("grok", &commands);
}

/// §10.4 evidence for Hermes: its ACP adapter advertises a fixed built-in
/// list and no skills at all — `~/.hermes/skills` reaches the model through
/// the skills tool, never as an invocable. Comet must not invent them.
#[tokio::test]
#[ignore]
async fn live_commands_hermes() {
    let h = AcpHarness::hermes();
    let Some(commands) = live_catalog(&h, None, "hermes").await else {
        return;
    };
    if commands.is_empty() {
        eprintln!(
            "hermes advertised nothing: its ACP adapter refuses session/new \
             until a provider is configured (`hermes model`)"
        );
        return;
    }
    assert!(
        commands.iter().any(|c| c.name == "help"),
        "hermes advertises /help: {commands:#?}"
    );
    assert!(
        commands.iter().all(|c| !c.name.starts_with("skill:")),
        "hermes' ACP surface exposes no skills: {commands:#?}"
    );
    report("hermes", &commands);
}

// ---------------------------------------------------------------------------
// Native plan mode (ARCHITECTURE.md §11)
// ---------------------------------------------------------------------------

/// Plan controls with a driveable mode watch and a scripted exit decision.
/// Returns the sender (the composer's toggle) and the recorded gate calls.
fn plan_controls(
    initial: bool,
    decision: PlanDecision,
) -> (
    RunControls,
    mpsc::Sender<SteerMessage>,
    watch::Sender<bool>,
    Arc<AtomicUsize>,
) {
    let (controls, steer_tx, _token) = controls();
    let (mode_tx, mode_rx) = watch::channel(initial);
    let calls = Arc::new(AtomicUsize::new(0));
    let recorder = calls.clone();
    let controls = RunControls {
        plan: comet_harness::PlanControls {
            mode: mode_rx,
            request_exit: Box::new(move || {
                recorder.fetch_add(1, Ordering::SeqCst);
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(decision.clone());
                rx
            }),
        },
        ..controls
    };
    (controls, steer_tx, mode_tx, calls)
}

/// The decision an unanswered gate degrades to, and the default this file's
/// mode-only tests use.
fn keep_planning() -> PlanDecision {
    PlanDecision {
        approved: false,
        feedback: None,
    }
}

fn plan_request(prompt: &str, cwd: &str, plan_mode: bool) -> RunRequest {
    RunRequest {
        cwd: cwd.into(),
        plan_mode,
        ..request(prompt)
    }
}

fn texts(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn plan_modes(events: &[AgentEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::PlanModeChanged { active } => Some(*active),
            _ => None,
        })
        .collect()
}

fn plans(events: &[AgentEvent]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::PlanUpdated { text, path } => Some((text.clone(), path.clone())),
            _ => None,
        })
        .collect()
}

/// §11.4 step 1: a run that asked for plan mode switches the session right
/// after `session/new`, before the first prompt. Grok's id is the spec's
/// (`plan`), accepted although `session/new` advertises no `modes` — verified
/// live against 1.0.13 (tests/fixtures/grok-plan-mode.json).
#[tokio::test]
async fn plan_mode_sets_the_session_mode_after_session_new() {
    let (controls, _steer, _mode, _calls) = plan_controls(true, keep_planning());
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-mode", "/tmp", true),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "planning", "{events:?}");
    // The accepted switch is the agent's own report; its current_mode_update
    // fires inside the setup drain, which drops notifications.
    assert!(plan_modes(&events).contains(&true), "{events:?}");
    assert_eq!(
        dones(&events).first().map(|d| d.0),
        Some(DoneStatus::Completed)
    );
}

/// A run that did NOT ask for plan mode never sends `session/set_mode` — the
/// scenario refuses the turn when one arrived.
#[tokio::test]
async fn plan_mode_off_sends_no_set_mode() {
    let (controls, _steer, _token) = controls();
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-modes", "/tmp", false),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "nosetmode", "{events:?}");
    assert!(plan_modes(&events).is_empty(), "{events:?}");
}

/// §11.2 generic ACP: hermes/pi have no static id, so plan mode exists only
/// when `session/new` advertises one. A `modes` state WITHOUT a plan id
/// yields no `set_mode` at all; one WITH a plan id is discovered and used.
#[tokio::test]
async fn advertised_modes_without_a_plan_id_never_set_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("comet-acp-modes"),
        r#"{"currentModeId":"default","availableModes":[{"id":"default","name":"Default"},{"id":"ask","name":"Ask"}]}"#,
    )
    .unwrap();
    let harness = AcpHarness::hermes().with_executable(fixture_path());
    let (controls, _steer, _mode, _calls) = plan_controls(true, keep_planning());
    let events = run_to_end(
        &harness,
        plan_request("scenario:plan-modes", dir.path().to_str().unwrap(), true),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "nosetmode", "{events:?}");
    assert!(plan_modes(&events).is_empty(), "{events:?}");
}

#[tokio::test]
async fn advertised_plan_mode_is_discovered_by_a_generic_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("comet-acp-modes"),
        r#"{"currentModeId":"default","availableModes":[{"id":"default","name":"Default"},{"id":"plan","name":"Plan"}]}"#,
    )
    .unwrap();
    let harness = AcpHarness::hermes().with_executable(fixture_path());
    let (controls, _steer, _mode, _calls) = plan_controls(true, keep_planning());
    let events = run_to_end(
        &harness,
        plan_request("scenario:plan-modes", dir.path().to_str().unwrap(), true),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "setmode:plan", "{events:?}");
    assert_eq!(plan_modes(&events), vec![true], "{events:?}");
}

/// §11.4 step 2: the toggle flipped mid-run reaches the CLI's live switch,
/// and the agent's `current_mode_update` reports the result. `default` is
/// grok's non-plan mode id (live-verified).
#[tokio::test]
async fn toggling_plan_mode_mid_run_sets_the_agents_non_plan_mode() {
    let (controls, _steer, mode_tx, _calls) = plan_controls(true, keep_planning());
    let stream = harness()
        .run(plan_request("scenario:plan-toggle", "/tmp", true), controls)
        .await
        .expect("run starts");
    let mut stream = stream;
    let mut events = Vec::new();
    let collected = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            // Flip once the turn is visibly live, so the toggle can't be
            // consumed as the watch's initial value.
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "planning") {
                mode_tx.send(false).expect("toggle");
            }
            events.push(ev);
        }
    })
    .await;
    assert!(collected.is_ok(), "run finished in time: {events:?}");
    assert_eq!(texts(&events), "planningswitched:default", "{events:?}");
    assert_eq!(plan_modes(&events), vec![true, true, false], "{events:?}");
}

/// §11.4 step 4, the exit gate: grok raises it as `_x.ai/exit_plan_mode`
/// (NOT a permission request — verified live). The adapter emits nothing of
/// its own, parks on the engine's bridge and answers `{approved, abandoned}`.
/// The decision's feedback is the ENGINE's to deliver, as the user's next
/// message through the ordinary steer path (§11.2 "Feedback delivery"): the
/// adapter sends NO follow-up prompt of its own.
#[tokio::test]
async fn plan_exit_gate_rejects_without_sending_the_feedback_itself() {
    let (controls, steer, _mode, calls) = plan_controls(
        true,
        PlanDecision {
            approved: false,
            feedback: Some("Add a rollback step.".into()),
        },
    );
    // No steer sender: the loop may end the run the moment the turn settles,
    // UNLESS something queued a follow-up — which is exactly the regression
    // this pins. The fixture answers any such prompt with "unexpected-prompt".
    drop(steer);
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-exit", "/tmp", true),
        controls,
    )
    .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the gate reached the bridge"
    );
    // The adapter never mints PlanExitRequested/Resolved — the engine does.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::PlanExitRequested { .. } | AgentEvent::PlanExitResolved { .. }
        )),
        "{events:?}"
    );
    assert_eq!(
        plans(&events),
        vec![(
            "# Plan\n\n1. Rename README.md to README2.md.\n".to_owned(),
            None
        )],
        "{events:?}"
    );
    // The fixture answers any prompt that arrives after the rejected gate
    // with "unexpected-prompt" — the adapter must send none.
    assert_eq!(texts(&events), "", "{events:?}");
}

/// Approving answers the same gate with `approved: true`; the agent's own
/// `current_mode_update` then flips the reported mode off.
#[tokio::test]
async fn approving_the_plan_exit_gate_leaves_plan_mode() {
    let (controls, _steer, _mode, calls) = plan_controls(
        true,
        PlanDecision {
            approved: true,
            feedback: None,
        },
    );
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-approve", "/tmp", true),
        controls,
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(texts(&events), "building", "{events:?}");
    assert_eq!(plan_modes(&events), vec![true, true, false], "{events:?}");
}

/// The generic shape (§11.2 hermes/pi row): a `session/request_permission`
/// naming the plan-exit tool stops auto-approving and takes the reject
/// option on "keep planning" — while every OTHER permission in the same
/// plan-mode turn still auto-accepts. The plan text rides the exit tool's
/// own `rawOutput` (`ExitPlanModeOutput::PlanReady`).
#[tokio::test]
async fn plan_exit_permission_is_the_only_one_that_stops_auto_approving() {
    let (controls, _steer, _mode, calls) = plan_controls(
        false,
        PlanDecision {
            approved: false,
            feedback: None,
        },
    );
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-permission", "/tmp", false),
        controls,
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{events:?}");
    assert_eq!(texts(&events), "still planning", "{events:?}");
    assert_eq!(
        plans(&events),
        vec![(
            "# Plan\n\n1. Ship it.\n".to_owned(),
            Some("/w/plans/ship.md".to_owned())
        )],
        "{events:?}"
    );
}

/// The gate is the TOOL NAME, not the reported mode bit: an agent that never
/// sends `current_mode_update` (or sends it after the request) must still stop
/// the auto-approve, or §11.2's one non-auto-approving permission silently
/// approves the exit and the agent starts executing the plan.
#[tokio::test]
async fn an_unreported_plan_mode_still_stops_the_exit_auto_approve() {
    let (controls, _steer, _mode, calls) = plan_controls(
        false,
        PlanDecision {
            approved: false,
            feedback: None,
        },
    );
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-gate-unreported", "/tmp", false),
        controls,
    )
    .await;
    assert!(plan_modes(&events).is_empty(), "{events:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{events:?}");
    assert_eq!(texts(&events), "still planning", "{events:?}");
}

/// §11.6: the plan card represents the gate, so the `enter_plan_mode` /
/// `exit_plan_mode` tool calls must NOT also fold into a tool chip (a
/// rejected exit otherwise reads as a stray failed tool). The plan events
/// derived from the same updates survive, and an ordinary tool in the same
/// turn still renders.
#[tokio::test]
async fn plan_gate_tool_calls_render_no_chip_but_still_yield_the_plan_events() {
    let (controls, _steer, _mode, _calls) = plan_controls(true, keep_planning());
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-chips", "/tmp", true),
        controls,
    )
    .await;
    let chips: Vec<(String, ToolCall)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } => Some((id.clone(), call.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        chips,
        vec![(
            "r1".to_owned(),
            ToolCall::ReadFile {
                path: "/w/a.rs".into()
            }
        )],
        "{events:?}"
    );
    let results: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results, vec!["r1".to_owned()], "{events:?}");
    assert_eq!(plan_modes(&events), vec![true, true], "{events:?}");
}

/// An edit-kind call on a plan file publishes the FILE, not the hunk: the
/// adapter re-reads it from disk (grok's plan.md, Claude/opencode's
/// `**/plans/*.md`).
#[tokio::test]
async fn editing_a_plan_file_republishes_it_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("plans")).unwrap();
    // The agent reports the path it saw from its own cwd; on macOS the temp
    // root is a symlink, so compare against the resolved path.
    let root = dir.path().canonicalize().expect("canonical tempdir");
    let plan = root.join("plans").join("build.md");
    std::fs::write(&plan, "# Build plan\n\n1. Read.\n2. Write.\n").unwrap();
    let (controls, _steer, _token) = controls();
    let events = run_to_end(
        &harness(),
        plan_request("scenario:plan-file", dir.path().to_str().unwrap(), false),
        controls,
    )
    .await;
    assert_eq!(
        plans(&events),
        vec![(
            "# Build plan\n\n1. Read.\n2. Write.\n".to_owned(),
            Some(plan.to_string_lossy().into_owned())
        )],
        "{events:?}"
    );
}

// grok's ask-the-user extension (_x.ai/ask_user_question)
// ---------------------------------------------------------------------------

/// The questions the adapter put on the input bridge.
type AskLog = Arc<std::sync::Mutex<Vec<UserInputQuestion>>>;

/// Controls that record every question and answer it with `labels` — or, when
/// `labels` is `None`, DROP the resolver (the engine went away).
fn ask_controls(labels: Option<Vec<String>>) -> (RunControls, mpsc::Sender<SteerMessage>, AskLog) {
    let (controls, steer_tx, _token) = controls();
    let log: AskLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = log.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions: Vec<UserInputQuestion>| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            if let Some(labels) = labels.clone() {
                let answers: Vec<UserInputAnswer> = questions
                    .iter()
                    .map(|q| UserInputAnswer {
                        question_id: q.id.clone(),
                        labels: labels.clone(),
                    })
                    .collect();
                let _ = tx.send(answers);
            }
            rx
        }),
        ..controls
    };
    (controls, steer_tx, log)
}

/// The reply the fixture recorded, parsed.
fn ask_reply(dir: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(dir.join("ask-reply.txt")).expect("the agent got a reply");
    serde_json::from_str(&raw).expect("valid JSON-RPC")
}

/// grok's `_x.ai/ask_user_question` reverse request rides the ENGINE's input
/// bridge (the adapter never mints an `InputRequested` of its own), and is
/// answered with the `AskUserQuestionExtResponse::Accepted` shape: answers
/// keyed by the QUESTION TEXT, the picked label as the value. Captured live
/// against grok 1.0.13 (tests/fixtures/grok-plan-mode.json); a reply without
/// `outcome` fails the tool.
#[tokio::test]
async fn ask_user_question_extension_asks_through_the_input_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (controls, _steer, log) = ask_controls(Some(vec!["Red".into()]));
    let events = run_to_end(
        &harness(),
        plan_request("scenario:ask-user", dir.path().to_str().unwrap(), false),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "answered", "{events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::InputRequested { .. })),
        "the engine mints the request, not the adapter: {events:?}"
    );
    let asked = log.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].header, "Question");
    assert_eq!(asked[0].question, "Which color do you prefer, red or blue?");
    assert_eq!(asked[0].options, vec!["Red".to_owned(), "Blue".to_owned()]);
    assert!(!asked[0].multi_select);
    assert_eq!(
        ask_reply(dir.path()),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "outcome": "accepted",
                "answers": { "Which color do you prefer, red or blue?": "Red" },
            },
        })
    );
}

/// `multiSelect: true` → the answer is an ARRAY of labels, and the tool's own
/// `_meta` label heads the question card.
#[tokio::test]
async fn ask_user_question_multi_select_answers_with_an_array() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (controls, _steer, log) = ask_controls(Some(vec!["Red".into(), "Blue".into()]));
    let events = run_to_end(
        &harness(),
        plan_request("scenario:ask-multi", dir.path().to_str().unwrap(), false),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "answered", "{events:?}");
    let asked = log.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].header, "Ask User");
    assert!(asked[0].multi_select);
    assert_eq!(
        ask_reply(dir.path()),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "outcome": "accepted",
                "answers": { "Which colors do you like?": ["Red", "Blue"] },
            },
        })
    );
}

/// A dropped resolver still answers `accepted` — with NO answers. The other
/// `AskUserQuestionExtResponse` variant names are unknown, and a reply
/// without `outcome` fails the tool ("missing field `outcome`").
#[tokio::test]
async fn a_dropped_ask_resolver_answers_accepted_with_no_answers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (controls, _steer, _log) = ask_controls(None);
    let events = run_to_end(
        &harness(),
        plan_request("scenario:ask-user", dir.path().to_str().unwrap(), false),
        controls,
    )
    .await;
    assert_eq!(texts(&events), "answered", "{events:?}");
    assert_eq!(
        ask_reply(dir.path()),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": { "outcome": "accepted", "answers": {} },
        })
    );
}

/// LIVE (§11.8 `--live`): drive the real `grok agent stdio` through one plan
/// turn and answer its exit gate "keep planning". Everything the run sees is
/// logged, so the wire shapes in tests/fixtures/grok-plan-mode.json can be
/// re-pinned when grok changes them.
///
/// `cargo test -q -p comet-harness --test acp live_plan_grok -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn live_plan_grok_exit_gate() {
    let harness = AcpHarness::grok();
    if !harness.installed() {
        eprintln!("skipping live grok plan test: grok is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "# Probe\n\nHello.\n").unwrap();

    let (controls, _steer, _mode, calls) = plan_controls(
        true,
        PlanDecision {
            approved: false,
            feedback: None,
        },
    );
    let request = plan_request(
        "Plan mode is already active. Write a two-line plan to rename \
         README.md to README2.md, then call exit_plan_mode.",
        dir.path().to_str().unwrap(),
        true,
    );
    let request = RunRequest {
        model: None,
        ..request
    };
    let mut stream = harness.run(request, controls).await.expect("run starts");
    // A real agent stays parked for the steering mailbox after a turn: read
    // until the turn's own Done rather than to stream end.
    let mut events: Vec<AgentEvent> = Vec::new();
    let collected = tokio::time::timeout(Duration::from_secs(300), async {
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            eprintln!("LIVE EVENT {ev:?}");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
    })
    .await;
    assert!(
        collected.is_ok(),
        "live grok run finished in time: {events:?}"
    );
    // At least once: a live model may re-present the plan after a reject
    // (each retry is a fresh gate, and each one must reach the bridge).
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "grok's exit gate reached the decision bridge: {events:?}"
    );
    assert!(
        plan_modes(&events).contains(&true),
        "grok reported plan mode: {events:?}"
    );
    let plans = plans(&events);
    assert!(!plans.is_empty(), "grok published a plan: {events:?}");
    eprintln!("LIVE PLAN {:?}", plans.last());
    // "Keep planning" leaves the session in plan mode: grok answers the tool
    // with "The user wants to revise the plan…" and never reports `default`.
    assert!(
        !plan_modes(&events).contains(&false),
        "a rejected exit stays in plan mode: {events:?}"
    );
}

/// The descriptor gate (§11.3): grok drives plan mode end to end, the
/// discovery-only agents do not advertise a toggle.
#[test]
fn plan_mode_is_advertised_only_where_the_id_is_known_without_a_session() {
    assert!(AcpHarness::grok().plan_mode());
    assert!(!AcpHarness::hermes().plan_mode());
    assert!(!AcpHarness::pi().plan_mode());
}

/// §10.4 evidence for pi: pi-acp advertises pi's own `get_commands` list,
/// where every skill is prefixed `skill:` and extension-sourced commands are
/// already filtered out agent-side.
#[tokio::test]
#[ignore]
async fn live_commands_pi() {
    let h = AcpHarness::pi();
    let Some(commands) = live_catalog(&h, None, "pi").await else {
        return;
    };
    assert!(
        commands.iter().any(|c| c.name == "compact"),
        "pi-acp ships /compact: {commands:#?}"
    );
    report("pi", &commands);
}
