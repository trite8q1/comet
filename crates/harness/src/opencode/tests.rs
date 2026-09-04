use super::*;
use serde_json::json;

#[test]
fn models_map_provider_catalog_with_variant_ladders() {
    let providers = json!({
        "all": [
            {
                "id": "anthropic",
                "name": "Anthropic",
                "models": {
                    "claude-opus-5": {
                        "name": "Claude Opus 5",
                        "variants": {"low": {}, "medium": {}, "high": {}, "max": {}},
                    },
                    "claude-haiku-4-5": {"name": "Claude Haiku 4.5"},
                }
            },
            {
                "id": "opencode",
                "name": "OpenCode Zen",
                "models": {"big-pickle": {"name": "Big Pickle"}}
            }
        ],
        "default": {},
        "connected": ["anthropic"],
    });
    let models = models_from_providers(&providers);
    // `connected` filters: the full catalog is 194 providers / 7k models of
    // which the user can run almost none (v0.2.21 field report).
    assert_eq!(models.len(), 2);
    let opus = models
        .iter()
        .find(|m| m.id == "anthropic/claude-opus-5")
        .expect("opus");
    assert_eq!(opus.label, "Claude Opus 5");
    assert_eq!(opus.description.as_deref(), Some("Anthropic"));
    assert_eq!(
        opus.reasoning_levels,
        vec![
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::Max
        ]
    );
    let haiku = models
        .iter()
        .find(|m| m.id == "anthropic/claude-haiku-4-5")
        .expect("haiku");
    assert!(haiku.reasoning_levels.is_empty());
    assert!(
        !models.iter().any(|m| m.id == "opencode/big-pickle"),
        "unconnected providers stay out of the picker"
    );
}

#[test]
fn missing_connected_list_falls_back_to_the_full_catalog() {
    let providers = json!({
        "all": [
            {"id": "a", "models": {"m1": {}}},
            {"id": "b", "models": {"m2": {}}},
        ],
    });
    assert_eq!(models_from_providers(&providers).len(), 2);
    let providers = json!({
        "all": [
            {"id": "a", "models": {"m1": {}}},
            {"id": "b", "models": {"m2": {}}},
        ],
        "connected": [],
    });
    assert_eq!(models_from_providers(&providers).len(), 2);
}

#[test]
fn variants_only_ride_models_that_advertise_them() {
    let providers = json!({
        "all": [{
            "id": "anthropic",
            "models": {
                "opus": {"variants": {"high": {}, "max": {}}},
                "haiku": {},
            }
        }]
    });
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::High)).as_deref(),
        Some("high")
    );
    // XHigh clamps down the candidate ladder to an advertised id.
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::XHigh)).as_deref(),
        Some("high")
    );
    assert_eq!(
        pick_variant(&providers, "anthropic", "haiku", Some(ReasoningLevel::High)),
        None
    );
    assert_eq!(pick_variant(&providers, "anthropic", "opus", None), None);
    assert_eq!(
        pick_variant(&providers, "missing", "opus", Some(ReasoningLevel::Low)),
        None
    );
}

#[test]
fn prompt_body_carries_model_variant_and_attachments() {
    let body = prompt_body(
        "hello",
        &Some(("anthropic".into(), "claude-opus-5".into())),
        Some("high"),
        &["/tmp/shot.png".to_owned()],
        "build",
    );
    assert_eq!(body["model"]["providerID"], "anthropic");
    assert_eq!(body["model"]["modelID"], "claude-opus-5");
    assert_eq!(body["variant"], "high");
    assert_eq!(body["parts"][0]["type"], "text");
    assert_eq!(body["parts"][0]["text"], "hello");
    assert_eq!(body["parts"][1]["type"], "file");
    assert_eq!(body["parts"][1]["mime"], "image/png");
    assert_eq!(body["parts"][1]["url"], "file:///tmp/shot.png");
    assert_eq!(body["agent"], "build");
}

/// §11.2, OpenCode row: plan mode IS the `agent` field — the two built-in
/// agents, nothing synthesized.
#[test]
fn the_requested_plan_mode_rides_the_prompt_as_the_agent() {
    let plan = prompt_body("plan it", &None, None, &[], "plan");
    assert_eq!(plan["agent"], "plan");
    let build = prompt_body("build it", &None, None, &[], "build");
    assert_eq!(build["agent"], "build");
}

#[test]
fn only_a_plan_dir_markdown_file_is_a_plan() {
    assert!(is_plan_file("/w/.opencode/plans/1-veil-port.md"));
    assert!(is_plan_file(
        "/home/u/.local/share/opencode/plans/2-thing.MD"
    ));
    assert!(!is_plan_file("/w/.opencode/plans/notes.txt"));
    assert!(!is_plan_file("/w/plans/nested/deep.md"));
    assert!(!is_plan_file("/w/src/main.md"));
}

#[test]
fn plan_exit_parts_bind_their_question_by_call_id() {
    let part = json!({
        "id": "prt_x", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "plan_exit", "callID": "call-exit",
        "state": {"status": "running", "input": {}},
    });
    assert_eq!(plan_exit_call(&part).as_deref(), Some("call-exit"));
    let other = json!({
        "id": "prt_y", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "bash", "callID": "call-bash",
        "state": {"status": "running", "input": {}},
    });
    assert_eq!(plan_exit_call(&other), None);
}

/// The plan card is the gate: its tool parts must never also chip.
#[test]
fn plan_gate_parts_are_not_chip_tools() {
    let gate = |tool: &str| {
        json!({
            "id": "prt_x", "messageID": "msg_a", "sessionID": "ses_1",
            "type": "tool", "tool": tool, "callID": "call-1",
            "state": {"status": "completed", "input": {}, "output": "ok"},
        })
    };
    assert!(is_gate_tool(&gate("plan_exit")));
    assert!(is_gate_tool(&gate("plan_enter")));
    assert!(!is_gate_tool(&gate("bash")));
    assert!(!is_gate_tool(&json!({
        "id": "prt_t", "type": "text", "text": "plan_exit",
    })));
}

#[tokio::test]
async fn completed_plan_tools_report_the_mode_and_re_read_the_plan() {
    let enter = json!({
        "id": "prt_e", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "plan_enter", "callID": "call-enter",
        "state": {"status": "completed", "input": {}, "output": "ok"},
    });
    assert!(matches!(
        plan_signals(&enter, true).await.as_slice(),
        [AgentEvent::PlanModeChanged { active: true }]
    ));
    // Only the snapshot that DECODED the completion signals (the TUI's own
    // once-per-part rule); re-delivered snapshots are silent.
    assert!(plan_signals(&enter, false).await.is_empty());

    let exit = json!({
        "id": "prt_x", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "plan_exit", "callID": "call-exit",
        "state": {"status": "completed", "input": {}, "output": "ok"},
    });
    assert!(matches!(
        plan_signals(&exit, true).await.as_slice(),
        [AgentEvent::PlanModeChanged { active: false }]
    ));
    // "No" rejects the tool: an errored plan_exit never leaves plan mode.
    let rejected = json!({
        "id": "prt_x", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "plan_exit", "callID": "call-exit",
        "state": {"status": "error", "input": {}, "error": "rejected"},
    });
    assert!(plan_signals(&rejected, true).await.is_empty());

    let dir = tempfile::tempdir().expect("tempdir");
    let plans = dir.path().join(".opencode").join("plans");
    std::fs::create_dir_all(&plans).expect("plans dir");
    let plan = plans.join("1-veil-port.md");
    std::fs::write(&plan, "# Veil port\n").expect("plan file");
    let edit = json!({
        "id": "prt_w", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "write", "callID": "call-write",
        "state": {"status": "completed", "input": {"filePath": plan.to_str().unwrap()}},
    });
    let events = plan_signals(&edit, true).await;
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::PlanUpdated { text, path }]
            if text == "# Veil port\n" && path.as_deref() == plan.to_str()
    ));
    // An edit anywhere else is an ordinary edit.
    let source = json!({
        "id": "prt_s", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "edit", "callID": "call-edit",
        "state": {"status": "completed", "input": {"filePath": "/w/src/main.rs"}},
    });
    assert!(plan_signals(&source, true).await.is_empty());
}

fn feed_with_assistant(message: &str) -> SessionFeed {
    let mut feed = SessionFeed::default();
    feed.assistant_messages.insert(message.into(), true);
    feed
}

#[test]
fn reasoning_parts_stream_as_reasoning_deltas() {
    let mut feed = feed_with_assistant("msg_a");
    // Opening snapshot: empty reasoning part fixes the kind.
    let open = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "",
    });
    assert!(part_snapshot_events(&mut feed, &open, true, None).is_empty());
    // Deltas append as ReasoningDelta, not text.
    let props = json!({"sessionID": "ses_1", "messageID": "msg_a", "partID": "prt_r"});
    let events = part_delta_events(&mut feed, &props, "prt_r", "thinking hard");
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "thinking hard"
    ));
    // The closing full snapshot re-sends everything: dedup emits nothing.
    let close = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard",
    });
    assert!(part_snapshot_events(&mut feed, &close, true, None).is_empty());
    // A longer snapshot emits only the suffix.
    let more = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard about it",
    });
    let events = part_snapshot_events(&mut feed, &more, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == " about it"
    ));
}

#[test]
fn reasoning_ahead_of_its_message_role_is_held_and_replayed() {
    let mut feed = SessionFeed::default();
    let part = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "early thought",
    });
    assert!(part_snapshot_events(&mut feed, &part, true, None).is_empty());
    assert_eq!(feed.pending_parts.len(), 1);
    // The role lands; replay drains the held part.
    feed.assistant_messages.insert("msg_a".into(), true);
    let mut turn = TurnState::begin(None);
    let events = replay_pending(&mut feed, "msg_a", true, &mut turn);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "early thought"
    ));
    assert!(turn.saw_content);
}

#[test]
fn main_feed_user_text_is_the_prompt_echo_and_never_renders() {
    let mut feed = SessionFeed::default();
    feed.assistant_messages.insert("msg_u".into(), false);
    let part = json!({
        "id": "prt_u", "messageID": "msg_u", "sessionID": "ses_1",
        "type": "text", "text": "the prompt",
    });
    assert!(part_snapshot_events(&mut feed, &part, true, None).is_empty());
    // On a CHILD feed the same shape is the message INTO the child.
    let mut child = SessionFeed::default();
    child.assistant_messages.insert("msg_u".into(), false);
    let events = part_snapshot_events(&mut child, &part, false, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::UserMessage { text }] if text == "the prompt"
    ));
    // Re-delivered snapshots don't double the entry.
    assert!(part_snapshot_events(&mut child, &part, false, None).is_empty());
}

#[test]
fn tool_parts_open_and_resolve_once() {
    let mut feed = feed_with_assistant("msg_a");
    let running = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "bash", "callID": "call-1",
        "state": {"status": "running", "input": {"command": "echo ok"}},
    });
    let events = part_snapshot_events(&mut feed, &running, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolCall { id, call: ToolCall::Exec { command } }]
            if id == "call-1" && command == "echo ok"
    ));
    assert!(part_snapshot_events(&mut feed, &running, true, None).is_empty());
    let done = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "bash", "callID": "call-1",
        "state": {"status": "completed", "input": {"command": "echo ok"}, "output": "ok\n"},
    });
    let events = part_snapshot_events(&mut feed, &done, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolResult { id, is_error: false, output: Some(o), .. }]
            if id == "call-1" && o == "ok\n"
    ));
}

#[test]
fn task_spawn_registers_child_by_metadata_and_completion_settles() {
    let mut feed = feed_with_assistant("msg_a");
    let mut children = HashMap::new();
    let mut pending = VecDeque::new();
    let mut unbound = HashMap::new();
    let running = json!({
        "id": "prt_task", "messageID": "msg_a", "sessionID": "ses_parent",
        "type": "tool", "tool": "task",
        "state": {
            "status": "running",
            "input": {"description": "Scan crates", "prompt": "scan", "subagent_type": "general"},
            "metadata": {"sessionId": "ses_child", "parentSessionId": "ses_parent"},
        },
    });
    let events = part_snapshot_events(
        &mut feed,
        &running,
        true,
        Some((&mut children, &mut pending, &mut unbound)),
    );
    // Genus-gated spawn naming, keyed by the PART id.
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }]
            if id == "prt_task" && name == "Agent: Scan crates"
    ));
    let child = children.get("ses_child").expect("bound child");
    assert_eq!(child.parent_tool_use_id, "prt_task");

    let completed = json!({
        "id": "prt_task", "messageID": "msg_a", "sessionID": "ses_parent",
        "type": "tool", "tool": "task",
        "state": {
            "status": "completed",
            "input": {"description": "Scan crates"},
            "output": "<task_result>done</task_result>",
            "metadata": {"sessionId": "ses_child"},
        },
    });
    assert_eq!(
        task_completion(&completed),
        Some(("ses_child".to_owned(), false))
    );
}

#[test]
fn child_binding_falls_back_to_title_match() {
    let mut children = HashMap::new();
    let mut pending = VecDeque::new();
    pending.push_back(PendingSpawn {
        tool_part_id: "prt_1".into(),
        description: "Scan crates".into(),
    });
    pending.push_back(PendingSpawn {
        tool_part_id: "prt_2".into(),
        description: "Write docs".into(),
    });
    assert!(bind_child(
        &mut children,
        &mut pending,
        "ses_b",
        "Write docs (@general subagent)"
    ));
    assert_eq!(children.get("ses_b").unwrap().parent_tool_use_id, "prt_2");
    assert_eq!(pending.len(), 1);
    // Unmatched title binds FIFO.
    assert!(bind_child(&mut children, &mut pending, "ses_a", "mystery"));
    assert_eq!(children.get("ses_a").unwrap().parent_tool_use_id, "prt_1");
    // Nothing pending: no bind.
    assert!(!bind_child(
        &mut children,
        &mut pending,
        "ses_c",
        "anything"
    ));
}

#[test]
fn questions_map_to_input_panel_shape() {
    let props = json!({
        "id": "que_1",
        "sessionID": "ses_1",
        "questions": [{
            "question": "Which color?",
            "header": "Color",
            "options": [
                {"label": "Red", "description": "warm"},
                {"label": "Blue", "description": "cool"},
            ],
            "multiple": true,
        }],
    });
    let questions = map_questions(&props);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].id, "q0");
    assert_eq!(questions[0].header, "Color");
    assert_eq!(questions[0].question, "Which color?");
    assert_eq!(questions[0].options, vec!["Red", "Blue"]);
    assert!(questions[0].multi_select);
}

#[test]
fn tool_names_type_the_common_calls() {
    let call = oc_tool_call("bash", &json!({"command": "ls -la"}));
    assert_eq!(
        call,
        ToolCall::Exec {
            command: "ls -la".into()
        }
    );
    let call = oc_tool_call(
        "edit",
        &json!({"filePath": "/w/a.rs", "oldString": "a", "newString": "b"}),
    );
    assert_eq!(
        call,
        ToolCall::EditFile {
            path: "/w/a.rs".into(),
            old_string: Some("a".into()),
            new_string: Some("b".into()),
        }
    );
    let call = oc_tool_call("task", &json!({"description": "Scan crates"}));
    assert!(matches!(&call, ToolCall::Unknown { name, .. } if name == "Agent: Scan crates"));
    assert!(call.is_subagent_spawn());
    let call = oc_tool_call(
        "todowrite",
        &json!({"todos": [
            {"content": "step one", "status": "completed"},
            {"content": "step two", "status": "pending"},
        ]}),
    );
    assert!(matches!(
        &call,
        ToolCall::Todo { items } if items.len() == 2 && items[0].done && !items[1].done
    ));
    let call = oc_tool_call("mystery", &json!({"x": 1}));
    assert!(matches!(&call, ToolCall::Unknown { name, input: Some(_) } if name == "mystery"));
    assert!(!call.is_subagent_spawn());
}

#[test]
fn commands_map_from_wire() {
    // `GET /command` is the whole invocable catalog: `.opencode/command/*.md`
    // entries and skills alike, told apart only by `source` (live 1.18.10 —
    // `Command.source` is one of command | mcp | skill). Skills are therefore
    // explicitly invocable here, not implicit-only, and comet catalogs them
    // exactly as the server lists them: no `source` filter, no SKILL.md scan.
    let wire = json!([
        {"name": "init", "description": "Create AGENTS.md", "source": "command", "hints": ["$ARGUMENTS"]},
        {"name": "share"},
        {"name": "cometalpha", "description": "Alpha probe skill.", "source": "skill", "hints": []},
        {"description": "nameless is dropped"},
    ]);
    let commands = commands_from_wire(&wire);
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].name, "init");
    assert_eq!(commands[0].description, "Create AGENTS.md");
    assert_eq!(commands[1].name, "share");
    assert_eq!(commands[2].name, "cometalpha");
    assert_eq!(commands[2].description, "Alpha probe skill.");
    // `hints` is the template's placeholder list ("$ARGUMENTS"), not a
    // user-facing argument hint, so the popup shows none.
    assert_eq!(commands[0].input_hint, None);
}

#[test]
fn stall_env_and_startup_env_parse() {
    // Defaults (no env in test runner): bounded stall, 300s startup.
    assert_eq!(stall_bound(), Some(DEFAULT_STALL_BOUND));
    assert_eq!(startup_timeout(), DEFAULT_STARTUP_TIMEOUT);
}

#[test]
fn directory_header_percent_encodes() {
    assert_eq!(
        encode_directory("/home/u/my project"),
        "/home/u/my%20project"
    );
    assert_eq!(encode_directory("/plain/path"), "/plain/path");
}
