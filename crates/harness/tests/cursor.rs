//! CursorHarness integration tests against the fake shim in
//! `tests/fixtures/fake-cursor-shim.sh` (no node/@cursor/sdk involved).

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use comet_harness::{CancellationToken, CursorHarness, Harness, RunControls, SteerMessage};
use comet_proto::{AgentEvent, DoneStatus, HarnessId, RunRequest, SandboxLevel, ToolCall};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-cursor-shim.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> CursorHarness {
    CursorHarness::new().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |_| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token)
}

/// Collect events until the first Done (the session parks afterwards).
async fn run_to_first_done(
    harness: &CursorHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let mut stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(Duration::from_secs(10), async {
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
    .expect("run finished in time")
}

#[tokio::test]
async fn happy_path_maps_shim_frames_and_tags_subagents() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:happy"), controls).await;

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::SessionStarted { harness, model, session_id, .. }
            if *harness == HarnessId::Cursor && model == "composer-2.5" && session_id == "agent-1"
    )));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "planning".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello from cursor".into()
    }));
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "c1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "c1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // The task spawn is a parent chip; its interior arrives tagged, never bare.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "task1" && name == "Agent: scan repo"
    )));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "task1".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "sub scanning".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "task1".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "s1".into(),
            call: ToolCall::Search {
                pattern: "todo".into(),
                path: None,
            },
        }),
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "s1")),
        "subagent tool leaked into the parent feed: {events:?}"
    );
    // The task tool's end doubles as the subagent's tagged terminal — the
    // SDK has no separate frame for it, and without this the chip stays
    // "running" forever.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "task1"
                && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
    )));

    assert!(events.contains(&AgentEvent::Usage {
        input_tokens: 11,
        output_tokens: 5
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: Some(id),
            ..
        }) if id == "agent-1"
    ));
}

#[tokio::test]
async fn steer_after_done_becomes_the_next_turn() {
    let (controls, steer, _token) = controls();
    let harness = harness();
    let mut stream = harness
        .run(request("scenario:happy"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        let mut dones = 0;
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::Done { .. }) {
                dones += 1;
                if dones == 1 {
                    steer
                        .send(SteerMessage {
                            prompt: "follow up".into(),
                            message_id: None,
                        })
                        .await
                        .expect("steer sent");
                }
            }
            let done = dones >= 2;
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("both turns finished in time");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "second turn".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn interrupt_maps_to_interrupted_done() {
    let harness = CursorHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(200), Duration::from_millis(500));
    let (controls, _steer, token) = controls();
    let mut stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::TextDelta { .. }) {
                token.cancel();
            }
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn fatal_frame_surfaces_the_auth_fix_as_an_errored_done() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:fatal"), controls).await;
    match events.last() {
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(message),
            ..
        }) => {
            assert!(message.contains("CURSOR_API_KEY"), "{message}");
        }
        other => panic!("expected errored done, got {other:?}"),
    }
}

#[tokio::test]
async fn shim_crash_mid_run_reports_stderr_tail() {
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness(), request("scenario:crash"), controls).await;
    match events.last() {
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(message),
            ..
        }) => {
            assert!(
                message.contains("shim exploded") || message.contains("exit code 3"),
                "{message}"
            );
        }
        other => panic!("expected errored done, got {other:?}"),
    }
}

#[tokio::test]
async fn model_discovery_maps_the_live_catalog() {
    let models = harness().models().await.expect("models");
    // Parameterized Auto first; its bare `default` alias twin skipped.
    assert_eq!(models.len(), 2, "{models:?}");
    assert_eq!(models[0].id, "auto-smart");
    assert_eq!(models[0].label, "Auto");
    let optimize = &models[0].options[0];
    assert_eq!(optimize.id, "optimize_for");
    assert_eq!(optimize.label, "Optimize For");
    assert_eq!(
        optimize
            .choices
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["intelligence", "balanced", "cost"]
    );
    // The default comes from the isDefault variant, not the first value.
    assert_eq!(optimize.default_choice, "balanced");
    assert_eq!(models[1].id, "claude-fable-5");
    assert_eq!(models[1].description.as_deref(), Some("Anthropic frontier"));
    // A parameter without displayName labels by id; default = first value.
    assert_eq!(models[1].options[0].id, "thinking");
    assert_eq!(models[1].options[0].default_choice, "enabled");
}

// ---------------------------------------------------------------------------
// Agent Skills as slash commands (ARCHITECTURE.md §10.4/§10.5)
// ---------------------------------------------------------------------------

/// Write `<root>/<name>/SKILL.md` with the given frontmatter.
fn write_skill(root: PathBuf, name: &str, frontmatter: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\n{frontmatter}---\n\nBody.\n"),
    )
    .expect("SKILL.md");
}

/// The catalog is exactly what Cursor's own roots hold, in Cursor's own load
/// order: built-in, then the project's, then the user's, a later root winning
/// a shared name. `.claude`/`.codex` are the documented compatibility roots;
/// Codex's own built-ins and non-`cli` surfaces are dropped.
#[test]
fn skill_catalog_follows_cursor_roots_and_precedence() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    let home_root = |config: &str, sub: &str| home.path().join(config).join(sub);
    let project_root = |config: &str, sub: &str| project.path().join(config).join(sub);

    // One name in all three scopes: the user's copy wins the description, the
    // built-in's discovery keeps its place in the list.
    write_skill(
        home_root(".cursor", "skills-cursor"),
        "review",
        "name: review\ndescription: builtin review\n",
    );
    write_skill(
        project_root(".cursor", "skills"),
        "review",
        "name: review\ndescription: project review\n",
    );
    write_skill(
        home_root(".cursor", "skills"),
        "review",
        "name: review\ndescription: user review\n",
    );
    // A folded (`>-`) description, the shape Cursor's own skills ship with,
    // on a skill that is user-invocable ONLY.
    write_skill(
        project_root(".agents", "skills"),
        "scan-repo",
        "name: scan-repo\ndescription: >-\n  Scan the repository for\n  stale TODOs.\ndisable-model-invocation: true\n",
    );
    // The documented compatibility roots.
    write_skill(
        project_root(".claude", "skills"),
        "from-claude",
        "description: claude compat\n",
    );
    write_skill(
        home_root(".codex", "skills"),
        "from-codex",
        "description: codex compat\n",
    );
    // Codex's own built-in: never offered under Cursor.
    write_skill(
        home_root(".codex", "skills"),
        "imagegen",
        "description: codex builtin\n",
    );
    // `metadata.surfaces` gates the surface.
    write_skill(
        project_root(".cursor", "skills"),
        "ide-only",
        "description: ide only\nmetadata:\n  surfaces:\n    - ide\n",
    );
    write_skill(
        project_root(".cursor", "skills"),
        "cli-too",
        "description: cli too\nmetadata:\n  surfaces: [cli, ide]\n",
    );
    // Nested under a category folder, plus a dot-directory that is ignored.
    write_skill(
        project_root(".cursor", "skills").join("frontend"),
        "write-tests",
        "description: nested skill\n",
    );
    write_skill(
        project_root(".cursor", "skills").join(".archive"),
        "retired",
        "description: hidden\n",
    );

    let catalog = comet_harness::cursor::skills::scan(home.path(), Some(project.path()));
    let names: Vec<&str> = catalog.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "review",
            "from-claude",
            "scan-repo",
            "cli-too",
            "write-tests",
            "from-codex",
        ],
        "{catalog:?}"
    );
    let by_name = |name: &str| {
        catalog
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {catalog:?}"))
    };
    assert_eq!(by_name("review").description, "user review");
    assert_eq!(
        by_name("scan-repo").description,
        "Scan the repository for stale TODOs.",
        "a folded block scalar is one line"
    );
    for dropped in ["imagegen", "ide-only", "retired"] {
        assert!(
            catalog.iter().all(|c| c.name != dropped),
            "{dropped} must not be offered: {catalog:?}"
        );
    }
}

/// Without a cwd, `commands()` answers from the roots that do not depend on
/// one — the built-in root and the user's — the way the agent would list them
/// started outside a project.
#[tokio::test]
async fn commands_are_the_user_scoped_skills() {
    let home = tempfile::tempdir().expect("home");
    write_skill(
        home.path().join(".agents").join("skills"),
        "user-skill",
        "description: from the user root\n",
    );
    let commands = CursorHarness::new()
        .with_executable(fixture_path())
        .with_home(home.path())
        .commands(None)
        .await
        .expect("commands");
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(commands[0].name, "user-skill");
    assert_eq!(commands[0].description, "from the user root");
    assert_eq!(commands[0].input_hint, None);
}

/// §10.4: with a cwd, the scan adds that project's roots — the same catalog
/// the run in that directory advertises — and drops them again without one.
#[tokio::test]
async fn commands_add_the_project_skills_for_a_cwd() {
    let home = tempfile::tempdir().expect("home");
    write_skill(
        home.path().join(".agents").join("skills"),
        "user-skill",
        "description: from the user root\n",
    );
    let project = tempfile::tempdir().expect("project");
    write_skill(
        project.path().join(".cursor").join("skills"),
        "project-skill",
        "description: from the project root\n",
    );
    // A nested directory resolves to the git root, exactly as cursor-agent
    // picks the workspace for a run started deeper in the tree.
    std::fs::create_dir_all(project.path().join(".git")).expect(".git");
    let nested = project.path().join("crates").join("inner");
    std::fs::create_dir_all(&nested).expect("nested cwd");

    let harness = CursorHarness::new()
        .with_executable(fixture_path())
        .with_home(home.path());
    let names = |commands: &[comet_proto::SlashCommand]| {
        commands.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    };
    let scoped = harness.commands(Some(&nested)).await.expect("commands");
    assert_eq!(names(&scoped), ["project-skill", "user-skill"]);
    assert_eq!(scoped[0].description, "from the project root");
    // Another project on the same instance sees only its own, and no cwd
    // sees none of them.
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    assert_eq!(
        names(&harness.commands(Some(elsewhere.path())).await.unwrap()),
        ["user-skill"]
    );
    assert_eq!(
        names(&harness.commands(None).await.unwrap()),
        ["user-skill"]
    );
}

/// A run advertises the catalog for ITS cwd, so a project's skills reach the
/// popup exactly as the agent loaded them for that run.
#[tokio::test]
async fn run_advertises_project_skill_commands() {
    let home = tempfile::tempdir().expect("home");
    let project = tempfile::tempdir().expect("project");
    write_skill(
        home.path().join(".cursor").join("skills"),
        "user-skill",
        "description: from the user root\n",
    );
    write_skill(
        project.path().join(".cursor").join("skills"),
        "project-skill",
        "description: from the project root\n",
    );

    let harness = CursorHarness::new()
        .with_executable(fixture_path())
        .with_home(home.path());
    let mut req = request("scenario:happy");
    req.cwd = project.path().display().to_string();
    let (controls, _steer, _token) = controls();
    let events = run_to_first_done(&harness, req, controls).await;

    match events.first() {
        Some(AgentEvent::AvailableCommands { commands }) => {
            let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, vec!["project-skill", "user-skill"], "{commands:?}");
        }
        other => panic!("expected the run to advertise its catalog, got {other:?}"),
    }
}

/// Slash parity (§10.5): the invocation reaches the SDK verbatim, on the run
/// path and on the steer path, whether or not the catalog knows the name —
/// comet never expands a skill, and an unknown `/name` stays the agent's to
/// interpret, exactly as in its own CLI.
#[tokio::test]
async fn slash_invocation_reaches_the_shim_verbatim() {
    let dir = tempfile::tempdir().expect("cwd");
    let harness = CursorHarness::new()
        .with_executable(fixture_path())
        .with_home(dir.path());
    let mut req = request("/review 42");
    req.cwd = dir.path().display().to_string();
    let (controls, steer, _token) = controls();
    let mut stream = harness.run(req, controls).await.expect("run starts");

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut dones = 0;
        while let Some(ev) = stream.next().await {
            if matches!(ev.expect("stream event"), AgentEvent::Done { .. }) {
                dones += 1;
                if dones == 1 {
                    steer
                        .send(SteerMessage {
                            prompt: "/not-a-skill please".into(),
                            message_id: None,
                        })
                        .await
                        .expect("steer sent");
                } else {
                    break;
                }
            }
        }
    })
    .await
    .expect("both turns finished in time");

    let log = std::fs::read_to_string(dir.path().join("slash-parity.jsonl"))
        .expect("the fake shim logged its stdin");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "run prompt + steer: {log}");
    for (line, (op, prompt)) in lines
        .iter()
        .zip([("run", "/review 42"), ("user", "/not-a-skill please")])
    {
        let frame: serde_json::Value = serde_json::from_str(line).expect("stdin frame is json");
        assert_eq!(frame["op"], op, "{frame}");
        assert_eq!(
            frame["prompt"], prompt,
            "the invocation reaches the SDK verbatim — no expansion, no skill frame"
        );
    }
}

/// Does any directory under `root` named `name` hold a `SKILL.md`?
fn skill_exists(root: &std::path::Path, name: &str, depth: usize) -> bool {
    if depth > 10 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            return false;
        }
        (entry.file_name() == name && path.join("SKILL.md").is_file())
            || skill_exists(&path, name, depth + 1)
    })
}

/// Live evidence against the real Cursor install on this machine:
/// `cargo test -p comet-harness --test cursor -- --ignored live_commands`.
/// No model turn, no API cost.
///
/// Two halves. Always: every skill comet offers is backed by a real
/// `SKILL.md` under one of Cursor's documented roots. When `cursor-agent` is
/// installed AND logged in (its login is separate from the SDK credentials
/// comet runs on): the CLI's own `available_commands_update` — the list its
/// `/` palette shows — must contain every name comet offers, which is the
/// §10.4 "the CLI is the authority" claim, executable.
#[tokio::test]
#[ignore]
async fn live_commands_match_the_installed_cursor_skills() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let roots = [
        home.join(".cursor").join("skills-cursor"),
        home.join(".cursor").join("skills"),
        home.join(".agents").join("skills"),
        home.join(".claude").join("skills"),
        home.join(".codex").join("skills"),
    ];
    let catalog = CursorHarness::new().commands(None).await.expect("commands");
    println!(
        "comet offers {} Cursor skills: {:?}",
        catalog.len(),
        catalog.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    if catalog.is_empty() {
        assert!(
            roots.iter().all(|root| !root.is_dir()),
            "nothing offered although a skills root exists: {roots:?}"
        );
        println!("no Cursor skill roots on this machine — nothing more to check");
        return;
    }
    for command in &catalog {
        assert!(
            roots
                .iter()
                .any(|root| skill_exists(root, &command.name, 0)),
            "/{} is backed by no SKILL.md under {roots:?}",
            command.name
        );
        assert!(
            !command.description.is_empty(),
            "/{} has no description",
            command.name
        );
    }

    let Some(cli) = ["/.local/bin/cursor-agent", "/.cursor/bin/cursor-agent"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", home.display())))
        .chain([
            PathBuf::from("/opt/homebrew/bin/cursor-agent"),
            PathBuf::from("/usr/local/bin/cursor-agent"),
        ])
        .find(|p| p.exists())
    else {
        println!("cursor-agent is not installed — skipping the CLI cross-check");
        return;
    };
    match cursor_agent_commands(&cli, &home).await {
        Some(advertised) => {
            println!("cursor-agent advertises {} commands", advertised.len());
            let overlap = catalog
                .iter()
                .filter(|c| advertised.iter().any(|a| a == &c.name))
                .count();
            if overlap == 0 {
                println!(
                    "cursor-agent advertised no skill of ours (skills are gated by a \
                     server-side flag on its account) — nothing to compare"
                );
                return;
            }
            for command in &catalog {
                assert!(
                    advertised.iter().any(|a| a == &command.name),
                    "comet offers /{} but cursor-agent's own list does not: {advertised:?}",
                    command.name
                );
            }
        }
        None => println!(
            "cursor-agent answered no command list (it is logged out — its login is \
             separate from the SDK's) — skipping the CLI cross-check"
        ),
    }
}

/// `cursor-agent acp`'s advertised command names for `cwd`: `initialize` then
/// `session/new`, reading the `available_commands_update` notification. No
/// prompt is ever sent. `None` when the CLI refuses (logged out).
async fn cursor_agent_commands(
    cli: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<Vec<String>> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = tokio::process::Command::new(cli)
        .arg("acp")
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("cursor-agent acp starts");
    async fn write(stdin: &mut tokio::process::ChildStdin, value: serde_json::Value) {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(format!("{value}\n").as_bytes())
            .await
            .expect("write");
        stdin.flush().await.expect("flush");
    }

    let mut stdin = child.stdin.take().expect("stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    write(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": 1, "clientCapabilities": {} },
        }),
    )
    .await;
    write(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "session/new",
            "params": { "cwd": cwd.display().to_string(), "mcpServers": [] },
        }),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(30), async {
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if frame["params"]["update"]["sessionUpdate"] == "available_commands_update" {
                return Some(
                    frame["params"]["update"]["availableCommands"]
                        .as_array()?
                        .iter()
                        .filter_map(|c| c["name"].as_str().map(str::to_owned))
                        .collect(),
                );
            }
            if frame["id"] == 2 && !frame["error"].is_null() {
                println!("cursor-agent session/new: {}", frame["error"]);
                return None;
            }
        }
        None
    })
    .await
    .unwrap_or(None)
}
