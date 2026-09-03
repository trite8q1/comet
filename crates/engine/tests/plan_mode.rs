//! Native plan mode, engine half (ARCHITECTURE.md §11.3–11.4): the plan-exit
//! bridge mirrors the input bridge (engine-minted id, parked resolver,
//! `RespondPlanExit` answers it), the requested mode reaches a live run
//! through the watch (`SetPlanMode`), the gate flips the session to
//! AwaitingInput, and the harness's reported mode reconciles the chat's
//! requested mode. A scripted harness stands in for an adapter.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use comet_doc::{MessagePart, PlanStatus, SessionCommandPayload, SessionMessageEntry};
use comet_engine::{EngineCore, HarnessRegistry};
use comet_harness::{Harness, HarnessError, RunControls};
use comet_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, PlanDecision, ReasoningLevel, RunRequest,
    SandboxLevel, SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-plan";

/// Plays the native cycle: reports the requested mode, drafts twice, raises
/// the exit gate, then either executes (approved → mode off) or keeps
/// planning and records the feedback. Records every decision and every mode
/// value it observed so the test can assert the wire-facing half.
struct PlanHarness {
    decisions: Arc<Mutex<Vec<PlanDecision>>>,
    modes_seen: Arc<Mutex<Vec<bool>>>,
    /// Every `RunRequest` this harness was started with (the feedback path
    /// on a turn-boundary agent re-dispatches with the feedback as prompt).
    requests: Arc<Mutex<Vec<RunRequest>>>,
    /// A turn-boundary steerer (Grok/OpenCode shape): the turn keeps running
    /// after a rejected gate and only an interrupt ends it.
    turn_boundary: bool,
}

#[async_trait]
impl Harness for PlanHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "PlanMock"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        if self.turn_boundary {
            SteeringMode::TurnBoundary
        } else {
            SteeringMode::StepBoundary
        }
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    fn plan_mode(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let decisions = self.decisions.clone();
        let modes_seen = self.modes_seen.clone();
        self.requests.lock().unwrap().push(request.clone());
        let turn_boundary = self.turn_boundary;
        let RunControls {
            plan,
            mut steering,
            interrupt,
            ..
        } = controls;
        let mut mode = plan.mode;
        let request_exit = plan.request_exit;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        tokio::spawn(async move {
            let initial = *mode.borrow();
            modes_seen.lock().unwrap().push(initial);
            let _ = tx.send(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-plan".into(),
                assistant_message_id: "a-1".into(),
            });
            let _ = tx.send(AgentEvent::PlanModeChanged { active: initial });
            if !initial {
                // Not a planning run: wait for a toggle (the SetPlanMode
                // test), report it, and finish.
                if tokio::time::timeout(Duration::from_secs(5), mode.changed())
                    .await
                    .is_ok()
                {
                    let now = *mode.borrow();
                    modes_seen.lock().unwrap().push(now);
                    let _ = tx.send(AgentEvent::PlanModeChanged { active: now });
                }
                let _ = tx.send(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some("sess-plan".into()),
                });
                return;
            }
            let _ = tx.send(AgentEvent::PlanUpdated {
                text: "# v1".into(),
                path: Some("/tmp/p.md".into()),
            });
            let _ = tx.send(AgentEvent::PlanUpdated {
                text: "# v2".into(),
                path: None,
            });
            let decision = request_exit()
                .await
                .unwrap_or(PlanDecision::keep_planning(None));
            decisions.lock().unwrap().push(decision.clone());
            if decision.approved {
                let _ = tx.send(AgentEvent::PlanModeChanged { active: false });
                let _ = tx.send(AgentEvent::TextDelta {
                    text: "executing".into(),
                });
            } else if turn_boundary {
                // A rejected gate does not end this agent's turn: it goes on
                // (asking its own question, raising the gate again…) until
                // the host cancels it — exactly the shape the feedback path
                // must cut through.
                let _ = tokio::time::timeout(Duration::from_secs(5), interrupt.cancelled()).await;
                let _ = tx.send(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some("sess-plan".into()),
                });
                return;
            } else {
                // The engine delivers feedback as a steer: confirm it like a
                // real adapter (a `Steered` boundary) and draft from it.
                let fed = tokio::time::timeout(Duration::from_secs(5), steering.recv())
                    .await
                    .ok()
                    .flatten();
                let _ = tx.send(AgentEvent::Steered {
                    assistant_message_id: Some("a-1".into()),
                    next_assistant_message_id: Some("a-2".into()),
                });
                let _ = tx.send(AgentEvent::PlanUpdated {
                    text: format!("# v3 ({})", fed.map(|m| m.prompt).unwrap_or_default()),
                    path: None,
                });
            }
            let _ = tx.send(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-plan".into()),
            });
        });
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event), rx))
        });
        Ok(stream.boxed())
    }
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

/// The LATEST plan part: a steer (feedback delivery) splits the segment, and
/// the next draft is a new card in the new entry — the older card stays as
/// history (ARCHITECTURE.md §11.5).
fn plan_part(core: &EngineCore) -> Option<MessagePart> {
    entries(core)
        .iter()
        .flat_map(|e| e.parts.iter())
        .rfind(|p| matches!(p, MessagePart::Plan { .. }))
        .cloned()
}

fn plan_status(core: &EngineCore) -> Option<(PlanStatus, Option<String>, String)> {
    match plan_part(core)? {
        MessagePart::Plan {
            status,
            request_id,
            plan,
            ..
        } => Some((status, request_id, plan)),
        _ => None,
    }
}

fn run_payload(message_id: &str, plan_mode: bool) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            prompt: "plan the veil port".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "~".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            plan_mode,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
        },
        message_id: message_id.into(),
    }
}

async fn assemble(harness: Arc<PlanHarness>) -> (tempfile::TempDir, EngineCore) {
    let tmp = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(harness);
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let client = comet_rpc::memory_client(core.rpc_service());
    client
        .call(
            comet_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "createChat",
                "chatId": CHAT,
                "deviceId": core.device_id,
            }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
    core.workspace
        .set_chat_config(
            CHAT,
            &ChatConfig {
                harness: HarnessId::Mock,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                plan_mode: true,
            },
        )
        .expect("set config");
    (tmp, core)
}

fn harness() -> Arc<PlanHarness> {
    Arc::new(PlanHarness {
        decisions: Arc::new(Mutex::new(Vec::new())),
        modes_seen: Arc::new(Mutex::new(Vec::new())),
        requests: Arc::new(Mutex::new(Vec::new())),
        turn_boundary: false,
    })
}

fn turn_boundary_harness() -> Arc<PlanHarness> {
    Arc::new(PlanHarness {
        turn_boundary: true,
        ..PlanHarness {
            decisions: Arc::new(Mutex::new(Vec::new())),
            modes_seen: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            turn_boundary: false,
        }
    })
}

/// §11.4 the third answer: REJECT. The gate is answered on the harness's own
/// wire (never a silent approval, and distinct from keep-planning), the turn
/// ends, the card settles `rejected`, and plan mode is left — the torn-down
/// run can never report a mode itself, so Comet writes the toggle.
///
/// Also pins the ordering that makes it work: `PlanExitResolved` rides
/// `engine_tx`, which the run loop's `biased` select drains ahead of the
/// harness stream, so the reject folds BEFORE the interrupt's `Done` — which
/// would otherwise stamp the part `revising` on its way out.
#[tokio::test(flavor = "multi_thread")]
async fn rejecting_a_plan_ends_the_turn_and_leaves_plan_mode() {
    let h = turn_boundary_harness();
    let (_tmp, core) = assemble(h.clone()).await;
    core.doc_host
        .queue_command(CHAT, run_payload("m-1", true))
        .expect("queue run");
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "gate",
    )
    .await;
    assert!(
        core.workspace.chat_config(CHAT).unwrap().plan_mode,
        "the run is a planning run"
    );
    let (_, request_id, _) = plan_status(&core).unwrap();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.unwrap(),
                approved: false,
                rejected: true,

                feedback: None,
            },
        )
        .expect("queue reject");
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "the turn ends",
    )
    .await;
    assert_eq!(
        h.decisions.lock().unwrap().clone(),
        vec![PlanDecision::reject()],
        "the harness is told reject, not keep-planning",
    );
    let (status, _, _) = plan_status(&core).expect("the card survives as history");
    assert_eq!(status, PlanStatus::Rejected);
    assert!(
        !core.workspace.chat_config(CHAT).unwrap().plan_mode,
        "a reject leaves plan mode",
    );
    // One turn only: a reject never re-dispatches (that is feedback's job).
    assert_eq!(h.requests.lock().unwrap().len(), 1);
}

/// Stop with the gate still parked: the drain answers the harness "keep
/// planning" (never a silent approval), and the CARD has to settle with it.
/// Left `AwaitingApproval` the plan part would outlive its run — buttons live
/// on a dead request id, and every later composer send taken as feedback on a
/// plan nobody is waiting for.
#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_parked_gate_settles_the_card() {
    let h = turn_boundary_harness();
    let (_tmp, core) = assemble(h.clone()).await;
    core.doc_host
        .queue_command(CHAT, run_payload("m-1", true))
        .expect("queue run");
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "gate",
    )
    .await;
    core.sessions.interrupt(CHAT).await.expect("interrupt");
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "settles",
    )
    .await;
    let (status, _, _) = plan_status(&core).expect("the card survives as history");
    assert_eq!(
        status,
        PlanStatus::Revising,
        "a stopped gate settles where the drain answered it",
    );
    assert_eq!(
        h.decisions.lock().unwrap().clone(),
        vec![PlanDecision::keep_planning(None)],
        "the drain must never silently approve",
    );
}

/// ARCHITECTURE.md §11.2 "Feedback delivery" on a turn-boundary agent: a
/// message queued behind the parked gate keeps the session AwaitingInput
/// (never a Working spinner over nothing), and "keep planning" feedback
/// cancels the blocked turn and opens the next one on the resumed session
/// with the feedback as its prompt.
#[tokio::test(flavor = "multi_thread")]
async fn turn_boundary_feedback_cancels_the_turn_and_prompts_the_resumed_session() {
    let h = turn_boundary_harness();
    let (_tmp, core) = assemble(h.clone()).await;
    core.doc_host
        .queue_command(CHAT, run_payload("m-1", true))
        .expect("queue run");
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "gate",
    )
    .await;
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::AwaitingInput)
        },
        "AwaitingInput",
    )
    .await;

    // An ordinary message while the gate is parked: queued, still awaiting.
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Steer {
                prompt: "still there?".into(),
                message_id: Some("m-queued".into()),
            },
        )
        .expect("queue steer");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::AwaitingInput),
        "a message queued behind the gate must not read as Working"
    );

    // Every status the session publishes from here on: the cancel that
    // delivers the feedback must never surface an Idle (that edge is the
    // "done" chime and the sidebar's settled dot — on every device).
    let seen: Arc<Mutex<Vec<SessionStatus>>> = Arc::new(Mutex::new(Vec::new()));
    let mut watch = core.sessions.watch_sessions();
    let recorder = {
        let seen = seen.clone();
        tokio::spawn(async move {
            while watch.changed().await.is_ok() {
                let status = watch
                    .borrow()
                    .iter()
                    .find(|s| s.chat_id == CHAT)
                    .map(|s| s.status);
                if let Some(status) = status {
                    seen.lock().unwrap().push(status);
                }
            }
        })
    };
    let (_, request_id, _) = plan_status(&core).unwrap();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.unwrap(),
                approved: false,
                rejected: false,

                feedback: Some("smaller steps".into()),
            },
        )
        .expect("queue answer");
    // The blocked turn is cancelled and the feedback starts the next run on
    // the remembered harness session, still in plan mode.
    wait_for(|| h.requests.lock().unwrap().len() == 2, "second run").await;
    wait_for(
        || seen.lock().unwrap().contains(&SessionStatus::Working),
        "replacement run reports Working",
    )
    .await;
    recorder.abort();
    let statuses = seen.lock().unwrap().clone();
    assert!(
        !statuses.contains(&SessionStatus::Idle),
        "a replaced turn must not flash Idle: {statuses:?}"
    );
    let second = h.requests.lock().unwrap()[1].clone();
    assert_eq!(second.prompt, "smaller steps");
    assert_eq!(second.resume.as_deref(), Some("sess-plan"));
    assert!(second.plan_mode);
    assert!(
        entries(&core)
            .iter()
            .any(|e| e.role == comet_doc::MessageRole::User
                && e.parts.iter().any(
                    |p| matches!(p, MessagePart::Text { text, .. } if text == "smaller steps")
                )),
        "feedback must be a visible user entry"
    );
    // The new run raises its own gate; answer it so the test settles.
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "second gate",
    )
    .await;
    let (_, request_id, _) = plan_status(&core).unwrap();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.unwrap(),
                approved: true,
                rejected: false,

                feedback: None,
            },
        )
        .expect("approve");
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "settles",
    )
    .await;
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_gate_round_trips_through_the_ledger_and_reconciles_config() {
    let h = harness();
    let (_tmp, core) = assemble(h.clone()).await;

    core.doc_host
        .queue_command(CHAT, run_payload("m-1", true))
        .expect("queue run");

    // The gate lands as one in-place plan part, awaiting approval, and the
    // session reads as a question.
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "plan part awaiting approval",
    )
    .await;
    let (_, request_id, text) = plan_status(&core).unwrap();
    assert_eq!(text, "# v2", "drafts refresh the same part");
    assert_eq!(
        entries(&core)
            .iter()
            .flat_map(|e| e.parts.iter())
            .filter(|p| matches!(p, MessagePart::Plan { .. }))
            .count(),
        1
    );
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::AwaitingInput)
        },
        "AwaitingInput while the gate is open",
    )
    .await;

    // A wrong id is rejected and resolves nothing.
    assert!(
        !core
            .sessions
            .respond_plan_exit(CHAT, "nope", PlanDecision::approve())
            .unwrap()
    );

    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.clone().unwrap(),
                approved: true,
                rejected: false,

                feedback: None,
            },
        )
        .expect("queue answer");
    wait_for(
        || matches!(plan_status(&core), Some((PlanStatus::Approved, _, _))),
        "plan approved",
    )
    .await;
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "turn settles",
    )
    .await;
    assert_eq!(
        h.decisions.lock().unwrap().as_slice(),
        &[PlanDecision::approve()]
    );
    // The harness reported mode off after approval: the requested mode
    // followed it (ARCHITECTURE.md §11.1).
    wait_for(
        || {
            core.workspace
                .chat_config(CHAT)
                .is_some_and(|c| !c.plan_mode)
        },
        "ChatConfig.plan_mode reconciled to false",
    )
    .await;
    // Answering again is an orphan: rejected, never re-asked.
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.unwrap(),
                approved: false,
                rejected: false,

                feedback: None,
            },
        )
        .expect("queue stale answer");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.decisions.lock().unwrap().len(), 1);

    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_planning_carries_feedback_and_stays_in_plan_mode() {
    let h = harness();
    let (_tmp, core) = assemble(h.clone()).await;
    core.doc_host
        .queue_command(CHAT, run_payload("m-1", true))
        .expect("queue run");
    wait_for(
        || {
            matches!(
                plan_status(&core),
                Some((PlanStatus::AwaitingApproval, Some(_), _))
            )
        },
        "gate",
    )
    .await;
    let (_, request_id, _) = plan_status(&core).unwrap();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::RespondPlanExit {
                request_id: request_id.unwrap(),
                approved: false,
                rejected: false,

                feedback: Some("smaller steps".into()),
            },
        )
        .expect("queue answer");
    // The feedback reached the run as a steer and became the next draft.
    wait_for(
        || matches!(plan_status(&core), Some((PlanStatus::Drafting, _, text)) if text.contains("smaller steps")),
        "revised draft",
    )
    .await;
    // …and it is the user's own message in the transcript.
    assert!(
        entries(&core)
            .iter()
            .any(|e| e.role == comet_doc::MessageRole::User
                && e.parts.iter().any(
                    |p| matches!(p, MessagePart::Text { text, .. } if text == "smaller steps")
                )),
        "feedback must be a visible user entry: {:?}",
        entries(&core)
    );
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "turn settles",
    )
    .await;
    // No mode-off report: the requested mode stays on.
    assert!(core.workspace.chat_config(CHAT).unwrap().plan_mode);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn set_plan_mode_reaches_the_live_run_and_is_applied_when_idle() {
    let h = harness();
    let (_tmp, core) = assemble(h.clone()).await;
    // Idle: nothing live to switch, still Applied (the config carries it).
    assert!(!core.sessions.set_plan_mode(CHAT, true));
    core.doc_host
        .queue_command(CHAT, SessionCommandPayload::SetPlanMode { active: true })
        .expect("queue idle toggle");

    core.doc_host
        .queue_command(CHAT, run_payload("m-1", false))
        .expect("queue run");
    wait_for(
        || h.modes_seen.lock().unwrap().first() == Some(&false),
        "run started in default mode",
    )
    .await;
    core.doc_host
        .queue_command(CHAT, SessionCommandPayload::SetPlanMode { active: true })
        .expect("queue live toggle");
    wait_for(
        || h.modes_seen.lock().unwrap().as_slice() == [false, true],
        "the live run observed the toggle",
    )
    .await;
    // The harness reported the switch; the requested mode follows.
    wait_for(
        || {
            core.workspace
                .chat_config(CHAT)
                .is_some_and(|c| c.plan_mode)
        },
        "ChatConfig reconciled to the reported mode",
    )
    .await;
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "turn settles",
    )
    .await;
    core.shutdown().await;
}
