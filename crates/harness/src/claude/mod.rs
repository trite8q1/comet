//! Claude Code harness: spawns the installed `claude` CLI and speaks its
//! stream-json protocol directly — no adapter process in between. Resurrected
//! from the pre-ACP driver (see docs/research/harness.md) and modernized
//! against CLI 2.1.228.
//!
//! - stdout JSONL frames are normalized into [`AgentEvent`]s (init dedupe,
//!   subagent tagging, typed tool decoding, error-code mapping).
//! - PERMISSIONS ride the stdio control channel: `--permission-prompt-tool
//!   stdio` (undocumented — absent from `claude --help`, but it is the same
//!   transport the Claude Agent SDK's `query()` drives, and was re-validated
//!   live against 2.1.228: `can_use_tool` control requests arrive and
//!   allow/deny responses are honored). The alternative channel — an MCP
//!   permission tool — needs a server process and was rejected. Tool calls
//!   auto-allow (comet sessions run unattended, parity with the ACP
//!   harness's preferred-allow behavior); `AskUserQuestion` round-trips
//!   through [`RunControls::request_input`].
//! - DONE is the CLI's own `result` frame, eagerly: background work (a
//!   spawned subagent) never holds the turn. The CLI natively runs a second
//!   wake turn when a background task finishes — a fresh `init` (same
//!   session id, deduped) plus another `result` — and both are forwarded;
//!   the engine's parked-session resume path turns them into the
//!   done→Working→done wake.
//! - SUBAGENT frames arrive on the same stdout tagged with a top-level
//!   `parent_tool_use_id`; they are wrapped in [`AgentEvent::Subagent`] and
//!   NEVER folded into the parent feed (a background subagent interleaves
//!   with the parent's own stream — folding them in split contiguous text
//!   around phantom tool calls).
//! - Steering: queued [`SteerMessage`]s are written to stdin as user lines at
//!   any time; the CLI folds them into the running turn at its own step
//!   boundary.
//! - Interrupt: cancelling [`RunControls::interrupt`] sends the protocol-level
//!   interrupt control request, then escalates to SIGTERM and SIGKILL.
//! - PLAN MODE is the CLI's own (ARCHITECTURE.md §11.2): launch with
//!   `--permission-mode plan`, switch live with a `set_permission_mode`
//!   control request, report the mode from `system/init.permissionMode` and
//!   from a successful `EnterPlanMode`, re-read the plan file after every
//!   successful Write/Edit on a `**/plans/*.md` path, and answer the
//!   `ExitPlanMode` `can_use_tool` gate with the user's decision — the one
//!   permission request that is NOT auto-allowed.

pub mod catalog;
mod normalize;
mod wire;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use comet_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, PlanDecision, ReasoningLevel, RunRequest,
    SlashCommand, SteeringMode, ToolCall, UserInputAnswer, UserInputQuestion,
};

use crate::{
    Harness, HarnessError, PlanControls, RunControls, Signal, send_signal, shutdown_child,
};
use catalog::{apply_ultrathink, static_models, to_effort};
use normalize::Normalizer;
use wire::{ControlRequestFrame, Frame, allow_response, control_response_line};

/// Locate the device's installed Claude Code CLI: `CLAUDE_CODE_EXECUTABLE`,
/// then our own PATH, then the login-shell PATH snapshot (the user's shell
/// init shapes PATH in ways a GUI/service launch never sees — see
/// [`crate::shell_env`]), then known install locations as a last resort.
/// Resolved per call — cheap after the snapshot is cached.
fn resolve_claude_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECUTABLE")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    let exe = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe))
                .collect()
        })
        .unwrap_or_default();
    if let Some(shell_path) = crate::shell_env::login_shell_path() {
        candidates.extend(
            std::env::split_paths(shell_path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe)),
        );
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".claude").join("local").join("claude"));
        candidates.push(home.join(".local").join("bin").join("claude"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    candidates.push(PathBuf::from("/usr/local/bin/claude"));
    candidates.extend(
        crate::node_version_manager_bins()
            .into_iter()
            .map(|d| d.join(exe)),
    );
    candidates.into_iter().find(|p| p.exists())
}

fn option_is_on(options: &serde_json::Map<String, Value>, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "on" || s == "true",
        _ => false,
    }
}

/// The Claude Code harness. Construct with [`ClaudeHarness::new`]; tests point
/// it at a fake CLI with [`ClaudeHarness::with_executable`].
pub struct ClaudeHarness {
    executable: Option<PathBuf>,
    /// Grace between the interrupt control request and SIGTERM.
    interrupt_grace: Duration,
    /// Grace between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Command discovery cache, per probe cwd: only a successful probe is
    /// cached, so a broken CLI retries on the next picker open (ACP-harness
    /// parity).
    commands: crate::commands::CommandCache,
}

impl Default for ClaudeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            commands: crate::commands::CommandCache::default(),
        }
    }
}

impl ClaudeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a fixed CLI binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Tune the interrupt→SIGTERM→SIGKILL escalation timing.
    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.executable {
            return Ok(p.clone());
        }
        resolve_claude_executable().ok_or_else(|| {
            HarnessError::NotInstalled(
                "claude (searched PATH, the login shell's PATH, ~/.claude/local, \
                 ~/.local/bin, /opt/homebrew/bin, /usr/local/bin, and \
                 fnm/nvm/volta/pnpm/bun install dirs; set CLAUDE_CODE_EXECUTABLE \
                 to override)"
                    .into(),
            )
        })
    }

    fn build_command(&self, exe: &PathBuf, request: &RunRequest) -> Command {
        let mut cmd = Command::new(exe);
        crate::compose_child_path(&mut cmd, exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // Required by the CLI alongside `-p --output-format stream-json`.
            "--verbose",
            "--include-partial-messages",
            // Newer Claude models emit no readable thinking text unless a
            // summary is asked for (raw reasoning stays provider-private).
            "--thinking-display",
            "summarized",
            // Route permission prompts to the stdio control channel so
            // `can_use_tool` (and AskUserQuestion in particular) reaches us.
            // Undocumented flag; validated live against 2.1.228.
            "--permission-prompt-tool",
            "stdio",
        ]);
        // The 1M context window is selected via a model-id suffix
        // (`sonnet[1m]`), exactly how the CLI itself does it; fast mode and
        // always-on thinking are settings overrides.
        if let Some(model) = &request.model {
            let one_m = request
                .model_options
                .get("contextWindow")
                .and_then(Value::as_str)
                == Some("1m");
            cmd.arg("--model");
            cmd.arg(if one_m {
                format!("{model}[1m]")
            } else {
                model.clone()
            });
        }
        if let Some(effort) = to_effort(request.reasoning, request.model.as_deref()) {
            cmd.args(["--effort", effort]);
        }
        // Plan mode is a permission mode, so it and `auto_approve`'s bypass
        // are the same switch: a run REQUESTED in plan mode starts in plan
        // mode, exactly as `claude --permission-mode plan` would.
        if request.plan_mode {
            cmd.args(["--permission-mode", "plan"]);
        } else if request.auto_approve {
            cmd.args([
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions",
            ]);
        } else {
            cmd.args(["--permission-mode", "default"]);
        }
        if let Some(resume) = &request.resume {
            cmd.arg(format!("--resume={resume}"));
        }
        let mut settings = serde_json::Map::new();
        if option_is_on(&request.model_options, "fastMode") {
            settings.insert("fastMode".into(), Value::Bool(true));
        }
        if option_is_on(&request.model_options, "thinking") {
            settings.insert("alwaysThinkingEnabled".into(), Value::Bool(true));
        }
        if request.reasoning == Some(ReasoningLevel::Ultracode) {
            settings.insert("ultracode".into(), Value::Bool(true));
        }
        if !settings.is_empty() {
            cmd.arg("--settings");
            cmd.arg(Value::Object(settings).to_string());
        }
        if !request.cwd.is_empty() {
            cmd.current_dir(&request.cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Short-lived discovery probe: spawn the CLI in stream-json mode, send
    /// the `initialize` control request, and read the commands out of its
    /// control_response. No user message is ever written, so no turn (and no
    /// API call) happens; the child is torn down as soon as the response
    /// lands. The child stands in `cwd`, so the CLI folds that project's
    /// `.claude/skills` and `.claude/commands` into the listing; `None`
    /// leaves it in the process directory.
    async fn discover_commands(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        crate::compose_child_path(&mut cmd, &exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // Mandatory with --print + stream-json output; without it the
            // CLI exits immediately with a usage error.
            "--verbose",
        ]);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("claude child has no stdio".into()));
        };
        const PROBE_ID: &str = "comet-command-probe";
        let discovery = async {
            let request = serde_json::json!({
                "type": "control_request",
                "request_id": PROBE_ID,
                "request": { "subtype": "initialize" },
            });
            stdin
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(HarnessError::Io)?;
            stdin.flush().await.map_err(HarnessError::Io)?;
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = frame.get("response").cloned().unwrap_or(Value::Null);
                if response.get("request_id").and_then(Value::as_str) != Some(PROBE_ID) {
                    continue;
                }
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let msg = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("initialize control request failed");
                    return Err(HarnessError::Protocol(msg.into()));
                }
                return Ok(parse_initialize_commands(&response));
            }
            Err(HarnessError::Protocol(
                "claude exited before answering the initialize control request".into(),
            ))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }
}

/// `commands` out of an `initialize` control_response payload
/// (`response.response.commands`: name / description / argumentHint /
/// aliases). Verified live against 2.1.228: plugin skills arrive namespaced
/// (`vercel:deploy`) with the bare name as an alias, and built-ins carry
/// their own (`code-review` → `review`), which is what the CLI's own popup
/// matches on.
fn parse_initialize_commands(response: &Value) -> Vec<SlashCommand> {
    response
        .get("response")
        .and_then(|r| r.get("commands"))
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let name = c.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some(SlashCommand {
                name: name.to_owned(),
                description: c
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: c
                    .get("argumentHint")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned),
                aliases: c
                    .get("aliases")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|a| !a.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

#[async_trait]
impl Harness for ClaudeHarness {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Claude Code"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    }
    fn installed(&self) -> bool {
        self.executable.is_some() || resolve_claude_executable().is_some()
    }
    /// Done is the CLI's own terminal frame, for wake turns too.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    /// The CLI's `plan` permission mode, driven end to end (ARCHITECTURE.md
    /// §11.2): launch flag, live `set_permission_mode`, reported mode, plan
    /// file, and the `ExitPlanMode` gate.
    fn plan_mode(&self) -> bool {
        true
    }

    /// The curated static catalog (see [`catalog`]); requires an installed CLI
    /// so an absent binary surfaces as [`HarnessError::NotInstalled`] here,
    /// like the discovery call would.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        Ok(static_models())
    }

    /// Slash commands from the CLI's `initialize` control-request handshake —
    /// the same channel the Claude Agent SDK's `query()` opens. The response
    /// carries every command with description + argument hint and involves no
    /// model turn (verified live, 2.1.228: the control_response is the first
    /// stdout line, well before any API traffic). Cached per probe cwd on
    /// success.
    async fn commands(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        self.commands
            .get_or_try_init(cwd, async || self.discover_commands(cwd).await)
            .await
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = self.build_command(&exe, &request);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "comet_harness::claude", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinMsg>();
        tokio::spawn(stdin_writer(stdin, stdin_rx));

        // The initial prompt as the first stdin user line (streaming-input
        // mode). Ultrathink rides every user message — steers included.
        // Staged image attachments are inlined as base64 image content blocks
        // ahead of the text (verified against the real CLI); their path refs
        // also ride the prompt text, so a skipped/unreadable file degrades to
        // the old-app behavior (the agent opens the path with its Read tool).
        let images = load_image_blocks(&request.attachments).await;
        let first = wire::user_message_line_with_images(
            &apply_ultrathink(request.reasoning, &request.prompt),
            &images,
        );
        let _ = stdin_tx.send(StdinMsg::Line(first));

        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            child,
            stdout_lines: BufReader::new(stdout).lines(),
            stdin_tx,
            event_tx,
            controls,
            reasoning: request.reasoning,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            stderr_tail,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

enum StdinMsg {
    Line(String),
    /// Close stdin (end of steering input): the CLI finishes the current turn
    /// and exits, which ends the run stream at stdout EOF.
    Close,
}

/// Anthropic's API caps inline images at 5MB of raw bytes; larger files stay
/// path refs only.
const MAX_INLINE_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Media type for an inline image block — extension first, magic bytes as the
/// fallback (pasted screenshots may carry odd names). Only the API-supported
/// inline types map; anything else (svg/bmp/tiff/…) returns `None`.
fn image_media_type(path: &std::path::Path, bytes: &[u8]) -> Option<&'static str> {
    let by_ext = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    };
    by_ext.or(match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    })
}

/// Load `RunRequest::attachments` into inline image blocks, best-effort: an
/// unreadable, oversized, or unsupported file is skipped — its path ref still
/// rides the prompt text — never fatal to the run.
async fn load_image_blocks(paths: &[String]) -> Vec<wire::ImageBlock> {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    for path in paths {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(target: "comet_harness::claude", %path, error = %err, "attachment unreadable; path ref only");
                continue;
            }
        };
        if bytes.len() as u64 > MAX_INLINE_IMAGE_BYTES {
            tracing::debug!(target: "comet_harness::claude", %path, "attachment over inline cap; path ref only");
            continue;
        }
        let Some(media_type) = image_media_type(std::path::Path::new(path), &bytes) else {
            tracing::debug!(target: "comet_harness::claude", %path, "attachment not an inline-supported image; path ref only");
            continue;
        };
        blocks.push(wire::ImageBlock {
            media_type: media_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    blocks
}

/// Owns the child's stdin; a write failure (EPIPE after the child died) is
/// tolerated and logged.
async fn stdin_writer(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<StdinMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            StdinMsg::Line(line) => {
                let write = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if let Err(e) = write.await {
                    tracing::debug!(target: "comet_harness::claude", "stdin write failed (tolerated): {e}");
                    return;
                }
            }
            StdinMsg::Close => {
                let _ = stdin.shutdown().await;
                return;
            }
        }
    }
}

struct Session {
    child: Child,
    stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    reasoning: Option<ReasoningLevel>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    /// Rolling stderr tail for the crash message on an unexpected exit.
    stderr_tail: crate::StderrTail,
}

/// The per-run event loop: one task multiplexing stdout frames, the steering
/// mailbox, the interrupt token, and consumer liveness.
async fn run_session(session: Session) {
    let Session {
        mut child,
        mut stdout_lines,
        stdin_tx,
        event_tx,
        controls,
        reasoning,
        interrupt_grace,
        kill_grace,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        plan,
    } = controls;
    let PlanControls {
        mode: mut plan_mode,
        request_exit,
    } = plan;
    let request_input = Arc::new(request_input);
    let request_exit = Arc::new(request_exit);

    let mut norm = Normalizer::new();
    let mut plan_files = PlanFiles::default();
    let mut steering_open = true;
    let mut plan_watch_open = true;
    // `set_permission_mode` request id → the mode it asks for; the CLI's
    // `control_response` for that id is what we report as the new mode.
    let mut pending_modes: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut any_done = false;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        tokio::select! {
            line = stdout_lines.next_line() => match line {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let frame = match wire::parse_frame(line) {
                        Ok(frame) => frame,
                        Err(e) => {
                            tracing::debug!(target: "comet_harness::claude", "unparseable frame (skipped): {e}");
                            continue;
                        }
                    };
                    if let Frame::ControlRequest(req) = frame {
                        handle_control_request(req, &request_input, &request_exit, &stdin_tx, &event_tx);
                        continue;
                    }
                    // The CLI's answer to a mode switch WE asked for is the
                    // report that flips the toggle on every device.
                    if let Frame::ControlResponse(resp) = frame {
                        if let Some(active) = pending_modes.remove(&resp.response.request_id) {
                            if resp.response.subtype == "success" {
                                if event_tx.send(Ok(AgentEvent::PlanModeChanged { active })).await.is_err() {
                                    break 'main;
                                }
                            } else {
                                tracing::debug!(
                                    target: "comet_harness::claude",
                                    "set_permission_mode rejected: {}",
                                    resp.response.error.unwrap_or_default()
                                );
                            }
                        }
                        continue;
                    }
                    for ev in norm.normalize(frame, interrupted) {
                        let is_done = matches!(ev, AgentEvent::Done { .. });
                        // Claude keeps the plan in a file; a successful write
                        // to one is the only "plan changed" signal the wire
                        // has (ARCHITECTURE.md §11.2).
                        let follow_up = plan_files.follow_up(&ev).await;
                        // The gate tools ARE the plan card (ARCHITECTURE.md
                        // §11.2 "Gate tools are the card, not chips"): their
                        // call/result never fold into a tool chip, only the
                        // plan events derived from them above.
                        if !plan_files.is_gate_event(&ev)
                            && event_tx.send(Ok(ev)).await.is_err()
                        {
                            break 'main; // consumer gone — reap below
                        }
                        for ev in follow_up {
                            if event_tx.send(Ok(ev)).await.is_err() {
                                break 'main;
                            }
                        }
                        if is_done {
                            any_done = true;
                            if interrupted {
                                done_after_interrupt = true;
                                break 'main;
                            }
                        }
                    }
                }
                Ok(None) => break 'main, // stdout EOF: the CLI exited
                Err(e) => {
                    let _ = event_tx.send(Err(HarnessError::Io(e))).await;
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    let line = wire::user_message_line(&apply_ultrathink(reasoning, &msg.prompt));
                    let _ = stdin_tx.send(StdinMsg::Line(line));
                    // The CLI consumes the queued line at its own step
                    // boundary; rotate the assistant message id so post-steer
                    // output folds into a fresh message.
                    let (prev, next) = norm.rotate_for_steer();
                    let ev = AgentEvent::Steered {
                        assistant_message_id: Some(prev),
                        next_assistant_message_id: Some(next),
                    };
                    if event_tx.send(Ok(ev)).await.is_err() {
                        break 'main;
                    }
                }
                None => {
                    // Mailbox closed: end the input so the run can finish
                    // after the current turn.
                    steering_open = false;
                    let _ = stdin_tx.send(StdinMsg::Close);
                }
            },

            changed = plan_mode.changed(), if plan_watch_open && !interrupted => {
                if changed.is_err() {
                    // The host dropped the watch (run settling): stop polling
                    // it, never spin on the closed channel.
                    plan_watch_open = false;
                    continue;
                }
                let active = *plan_mode.borrow_and_update();
                let request_id = format!("mode_{}", uuid::Uuid::new_v4());
                pending_modes.insert(request_id.clone(), active);
                let _ = stdin_tx.send(StdinMsg::Line(
                    wire::set_permission_mode_line(&request_id, active),
                ));
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                let _ = stdin_tx.send(StdinMsg::Line(wire::interrupt_request_line("int_1")));
                // Escalate if the CLI doesn't wind down within the grace
                // periods: SIGTERM (kills bash trees, runs SessionEnd hooks),
                // then SIGKILL. Aborted once the child is reaped.
                if let Some(pid) = child.id() {
                    escalation = Some(tokio::spawn(async move {
                        tokio::time::sleep(interrupt_grace).await;
                        send_signal(pid, Signal::Term);
                        tokio::time::sleep(kill_grace).await;
                        send_signal(pid, Signal::Kill);
                    }));
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: norm.session_id.clone(),
                }))
                .await;
        } else if !interrupted && !any_done {
            let status = child.try_wait().ok().flatten();
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message("claude", status, &stderr_tail)),
                    session_id: norm.session_id.clone(),
                }))
                .await;
        }
    }

    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

type RequestExitFn = Box<dyn Fn() -> tokio::sync::oneshot::Receiver<PlanDecision> + Send + Sync>;

/// The CLI's own wording for a rejected tool use (verbatim from 2.1.258;
/// there is no plan-specific variant). The deny `message` is required by the
/// control-channel schema; the user's feedback never rides it — the CLI's own
/// TUI delivers feedback as a user message, and so does the engine.
const TOOL_USE_REJECTED: &str = "The user doesn't want to proceed with this tool use. The tool \
     use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). \
     STOP what you are doing and wait for the user to tell you how to proceed.";

/// Serve one `can_use_tool` control request. Every tool is auto-approved
/// (unattended parity — the CLI still blocks until SOME response arrives, so
/// every request must be answered) except the two that ARE the user's:
/// `AskUserQuestion` is intercepted — surface the questions through the
/// engine's input bridge (which owns the `InputRequested`/`InputResolved`
/// lifecycle), wait for the user's answers (in a subtask so the frame loop
/// keeps flowing), and hand them back keyed by question text, as the tool
/// expects — and `ExitPlanMode` is the plan gate (ARCHITECTURE.md §11.2),
/// answered by the user's own decision.
fn handle_control_request(
    req: ControlRequestFrame,
    request_input: &Arc<RequestInputFn>,
    request_exit: &Arc<RequestExitFn>,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
) {
    if req.request.subtype != "can_use_tool" {
        tracing::debug!(
            target: "comet_harness::claude",
            "unhandled control_request subtype: {}", req.request.subtype
        );
        return;
    }
    if req.request.tool_name == "ExitPlanMode" {
        handle_plan_exit(req, request_exit, stdin_tx, event_tx);
        return;
    }
    if req.request.tool_name != "AskUserQuestion" {
        let line = control_response_line(&req.request_id, allow_response(req.request.input));
        let _ = stdin_tx.send(StdinMsg::Line(line));
        return;
    }
    let request_input = Arc::clone(request_input);
    let stdin_tx = stdin_tx.clone();
    tokio::spawn(async move {
        let request_id = req.request_id;
        let input = req.request.input;
        let questions = parse_questions(&input);
        // The engine's input bridge is the SOLE emitter of
        // `InputRequested`/`InputResolved`: it mints the request id, parks the
        // resolver for `respond_input`, and surfaces both events. Emitting our
        // own copy here (keyed by Claude's control-request id) folded a SECOND
        // input part into the doc whose id no resolver knew — the QuestionPanel
        // answered that unanswerable twin and the run never resumed.
        //
        // A dropped sender (caller went away) degrades to empty answers so the
        // agent is unblocked rather than wedged.
        let answers = (request_input)(questions.clone()).await.unwrap_or_default();
        let updated = updated_input_with_answers(&input, &questions, &answers);
        let line = control_response_line(&request_id, allow_response(updated));
        let _ = stdin_tx.send(StdinMsg::Line(line));
    });
}

/// Serve the `ExitPlanMode` gate: the CLI injects the plan file's text and
/// path into the tool input (`plan` / `planFilePath`), so the plan is
/// published first, then the user's decision is awaited in a subtask (the
/// frame loop keeps flowing, exactly as for `AskUserQuestion`).
///
/// The ENGINE owns the gate's lifecycle — it mints the request id and is the
/// sole emitter of `PlanExitRequested`/`PlanExitResolved`; a harness copy
/// would fold a second, unanswerable card into the doc. A dropped decision
/// sender (the engine went away) reads as "keep planning", never as a silent
/// approval.
fn handle_plan_exit(
    req: ControlRequestFrame,
    request_exit: &Arc<RequestExitFn>,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
) {
    let request_exit = Arc::clone(request_exit);
    let stdin_tx = stdin_tx.clone();
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        let request_id = req.request_id;
        let input = req.request.input;
        if let Some(text) = input.get("plan").and_then(Value::as_str) {
            let ev = AgentEvent::PlanUpdated {
                text: text.to_owned(),
                path: input
                    .get("planFilePath")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            if event_tx.send(Ok(ev)).await.is_err() {
                return;
            }
        }
        let decision = (request_exit)()
            .await
            .unwrap_or(PlanDecision::keep_planning(None));
        // Keep planning: the CLI's own rejection sentence, never the user's
        // text — the CLI's TUI denies with that sentence and delivers typed
        // feedback as a USER message (`feedbackIsFromUser`); the engine
        // steers the feedback the same way (ARCHITECTURE.md §11.2). Raw
        // feedback in a tool error read to the model as an injected
        // instruction (2026-09-02 user report).
        let response = if decision.approved {
            wire::plan_exit_allow_response(input)
        } else {
            wire::deny_response(TOOL_USE_REJECTED)
        };
        let _ = stdin_tx.send(StdinMsg::Line(control_response_line(&request_id, response)));
        if decision.approved {
            // The allow payload's `setMode` leaves plan mode for the rest of
            // the session; report it so the toggle follows on every device.
            let _ = event_tx
                .send(Ok(AgentEvent::PlanModeChanged { active: false }))
                .await;
        }
    });
}

/// Adapter-local plan-file bookkeeping. Claude's plan lives in a file the
/// model writes (`~/.claude/plans/<slug>.md`, or a project `plansDirectory`),
/// so a successful Write/Edit on a `**/plans/*.md` path is what "the plan
/// changed" looks like on the wire; the text is read back from disk (the CLI
/// runs on this host). `EnterPlanMode` completing is the CLI's own report
/// that it entered plan mode.
#[derive(Default)]
struct PlanFiles {
    /// tool_use id → plan file, from the call until its result.
    writes: std::collections::HashMap<String, PathBuf>,
    /// tool_use ids of `EnterPlanMode` calls awaiting their result.
    enters: std::collections::HashSet<String>,
    /// tool_use ids of the gate tools (`EnterPlanMode`/`ExitPlanMode`) seen
    /// this run: neither their call nor their result renders as a chip.
    gates: std::collections::HashSet<String>,
}

impl PlanFiles {
    /// Whether `event` is a gate tool's call or result (the card's, not a
    /// chip's). Records gate calls as a side effect; must run AFTER
    /// [`Self::follow_up`] on the same event so the plan events still derive.
    fn is_gate_event(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::ToolCall {
                id,
                call: ToolCall::Unknown { name, .. },
            } if name == "ExitPlanMode" || name == "EnterPlanMode" => {
                self.gates.insert(id.clone());
                true
            }
            AgentEvent::ToolResult { id, .. } => self.gates.contains(id),
            _ => false,
        }
    }

    /// Record plan-relevant calls, and turn their (non-error) results into
    /// the plan events. Subagent traffic arrives wrapped in
    /// `AgentEvent::Subagent` and therefore never matches.
    async fn follow_up(&mut self, event: &AgentEvent) -> Vec<AgentEvent> {
        match event {
            AgentEvent::ToolCall { id, call } => {
                match call {
                    ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. } => {
                        if let Some(plan) = plan_file_path(path) {
                            self.writes.insert(id.clone(), plan);
                        }
                    }
                    ToolCall::Unknown { name, .. } if name == "EnterPlanMode" => {
                        self.enters.insert(id.clone());
                    }
                    _ => {}
                }
                Vec::new()
            }
            AgentEvent::ToolResult { id, is_error, .. } => {
                let wrote = self.writes.remove(id);
                let entered = self.enters.remove(id);
                if *is_error {
                    return Vec::new();
                }
                let mut out = Vec::new();
                if entered {
                    out.push(AgentEvent::PlanModeChanged { active: true });
                }
                if let Some(path) = wrote {
                    match tokio::fs::read_to_string(&path).await {
                        Ok(text) => out.push(AgentEvent::PlanUpdated {
                            text,
                            path: Some(path.display().to_string()),
                        }),
                        Err(err) => tracing::debug!(
                            target: "comet_harness::claude",
                            path = %path.display(), error = %err,
                            "plan file unreadable after its write"
                        ),
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

/// A path the CLI would keep a plan at: `**/plans/*.md`, `~` expanded.
fn plan_file_path(raw: &str) -> Option<PathBuf> {
    let path = match raw.strip_prefix("~/").zip(std::env::var_os("HOME")) {
        Some((rest, home)) => PathBuf::from(home).join(rest),
        None => PathBuf::from(raw),
    };
    let is_markdown = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    let in_plans = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("plans");
    (is_markdown && in_plans).then_some(path)
}

/// Parse Claude's `AskUserQuestion` tool input into [`UserInputQuestion`]s
/// (tolerant of `header`/`title`, `question`/`prompt`, string or object
/// options — option descriptions are dropped, the wire type carries labels).
fn parse_questions(input: &Value) -> Vec<UserInputQuestion> {
    let raw = input.get("questions").and_then(Value::as_array);
    raw.map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let field =
                |keys: [&str; 2]| keys.iter().find_map(|k| q.get(*k).and_then(Value::as_str));
            UserInputQuestion {
                id: uuid::Uuid::new_v4().to_string(),
                header: field(["header", "title"]).unwrap_or("Question").into(),
                question: field(["question", "prompt"]).unwrap_or("").into(),
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|op| match op {
                        Value::String(s) => s.clone(),
                        other => other
                            .get("label")
                            .or_else(|| other.get("value"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Merge the user's answers back into the tool input, keyed by question text
/// (single-select ⇒ a string, multi-select ⇒ an array), as the tool expects.
fn updated_input_with_answers(
    input: &Value,
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> Value {
    let mut updated = match input {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut by_question = serde_json::Map::new();
    for q in questions {
        let labels: Vec<String> = answers
            .iter()
            .find(|a| a.question_id == q.id)
            .map(|a| a.labels.clone())
            .unwrap_or_default();
        let value = if q.multi_select {
            Value::Array(labels.into_iter().map(Value::String).collect())
        } else {
            Value::String(labels.into_iter().next().unwrap_or_default())
        };
        by_question.insert(q.question.clone(), value);
    }
    updated.insert("answers".into(), Value::Object(by_question));
    Value::Object(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_questions_tolerantly() {
        let input = json!({
            "questions": [
                {
                    "header": "Choice",
                    "question": "Pick one",
                    "options": ["A", {"label": "B", "description": "second"}],
                    "multiSelect": false
                },
                { "title": "Alt", "prompt": "Pick many", "multi_select": true }
            ]
        });
        let qs = parse_questions(&input);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "Choice");
        assert_eq!(qs[0].options, vec!["A".to_string(), "B".to_string()]);
        assert!(!qs[0].multi_select);
        assert_eq!(qs[1].header, "Alt");
        assert_eq!(qs[1].question, "Pick many");
        assert!(qs[1].multi_select);
    }

    #[test]
    fn answers_key_by_question_text() {
        let input =
            json!({"questions": [{"header": "H", "question": "Pick one", "options": ["A", "B"]}]});
        let qs = parse_questions(&input);
        let answers = vec![UserInputAnswer {
            question_id: qs[0].id.clone(),
            labels: vec!["B".into()],
        }];
        let updated = updated_input_with_answers(&input, &qs, &answers);
        assert_eq!(updated["answers"]["Pick one"], json!("B"));
        // Original input is preserved alongside the answers.
        assert!(updated["questions"].is_array());
    }
}
