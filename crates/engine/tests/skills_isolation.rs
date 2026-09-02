//! ARCHITECTURE.md §10.6 / §10.7(2): `ListCommands {harness, cwd?}` answers from
//! exactly the resolved harness's catalog, for exactly the requested directory.
//! Two registered harnesses with disjoint catalogs must never bleed into each
//! other, an unregistered harness is an error — never a neighbour's list, never
//! a default harness's list — and two directories on one harness are two
//! catalogs.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use comet_engine::{EngineCore, HarnessRegistry};
use comet_harness::{Harness, HarnessError, RunControls};
use comet_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode,
};
use comet_rpc::methods;

/// A harness whose only trait of interest is its (fixed) catalog.
struct CatalogHarness {
    id: HarnessId,
    catalog: Vec<SlashCommand>,
}

fn cmd(name: &str, hint: Option<&str>) -> SlashCommand {
    SlashCommand {
        name: name.into(),
        description: format!("{name} description"),
        input_hint: hint.map(str::to_owned),
        aliases: Vec::new(),
    }
}

#[async_trait]
impl Harness for CatalogHarness {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        "Catalog"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    /// Catalogs are cwd-scoped (§10.4): a probe standing in a project folds
    /// that project's own entry into the harness's list.
    async fn commands(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        let mut catalog = self.catalog.clone();
        if let Some(cwd) = cwd {
            catalog.push(cmd(&format!("project:{}", cwd.display()), None));
        }
        Ok(catalog)
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::iter([Ok(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        })])
        .boxed())
    }
}

fn assemble(dir: &std::path::Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CatalogHarness {
        id: HarnessId::ClaudeCode,
        catalog: vec![
            cmd("architect", None),
            cmd("vercel:deploy", Some("[prod]")),
            cmd("compact", Some("<instructions>")),
        ],
    }));
    registry.register(Arc::new(CatalogHarness {
        id: HarnessId::Codex,
        catalog: vec![cmd("imagegen", None), cmd("architect", None)],
    }));
    EngineCore::assemble(dir, Arc::new(registry), HarnessId::ClaudeCode, None)
        .expect("engine assembles")
}

async fn list_commands(
    client: &comet_rpc::RpcClient,
    harness: &str,
) -> Result<Vec<SlashCommand>, comet_rpc::RpcError> {
    // No `cwd` member at all: the shape an old caller sends.
    let value = client
        .call(
            methods::LIST_COMMANDS,
            serde_json::json!({ "harness": harness }),
        )
        .await?;
    Ok(serde_json::from_value(value).expect("ListCommands reply decodes as [SlashCommand]"))
}

async fn list_commands_in(
    client: &comet_rpc::RpcClient,
    harness: &str,
    cwd: &str,
) -> Vec<SlashCommand> {
    let value = client
        .call(
            methods::LIST_COMMANDS,
            serde_json::json!({ "harness": harness, "cwd": cwd }),
        )
        .await
        .expect("ListCommands succeeds");
    serde_json::from_value(value).expect("ListCommands reply decodes as [SlashCommand]")
}

fn names(commands: &[SlashCommand]) -> Vec<&str> {
    commands.iter().map(|c| c.name.as_str()).collect()
}

#[tokio::test]
async fn list_commands_answers_from_the_resolved_harness_only() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let client = comet_rpc::memory_client(core.rpc_service());

    let claude = list_commands(&client, "claude-code").await.unwrap();
    assert_eq!(
        names(&claude),
        ["architect", "vercel:deploy", "compact"],
        "claude's catalog verbatim, in the adapter's order"
    );
    assert_eq!(claude[1].input_hint.as_deref(), Some("[prod]"));

    let codex = list_commands(&client, "codex").await.unwrap();
    assert_eq!(names(&codex), ["imagegen", "architect"]);

    // Same name on two harnesses stays two entries in two catalogs; neither
    // list carries the other's exclusive entries.
    assert!(!names(&claude).contains(&"imagegen"));
    assert!(!names(&codex).contains(&"vercel:deploy"));
    assert!(!names(&codex).contains(&"compact"));
}

#[tokio::test]
async fn unregistered_harness_is_an_error_not_a_neighbours_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let client = comet_rpc::memory_client(core.rpc_service());

    // Grok is not registered on this engine: no fallback to the default
    // harness (claude-code) and no fallback to any other slot.
    let err = list_commands(&client, "grok")
        .await
        .expect_err("unregistered harness fails");
    let text = err.to_string();
    assert!(
        !text.contains("architect") && !text.contains("imagegen"),
        "error must not leak a catalog: {text}"
    );
}

#[tokio::test]
async fn wire_shape_is_additive_and_omits_empty_optionals() {
    // The phone decodes this JSON with its own model; keep the shape honest:
    // absent hint and aliases are omitted, never null/empty-array noise.
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let client = comet_rpc::memory_client(core.rpc_service());
    let value = client
        .call(
            methods::LIST_COMMANDS,
            serde_json::json!({ "harness": "codex" }),
        )
        .await
        .unwrap();
    assert_eq!(
        value,
        serde_json::json!([
            { "name": "imagegen", "description": "imagegen description" },
            { "name": "architect", "description": "architect description" },
        ])
    );
}

#[tokio::test]
async fn list_commands_is_scoped_to_the_requested_cwd() {
    // §10.4: every CLI resolves project skills against the directory it runs
    // in, so one harness answers two directories with two catalogs — and an
    // omitted `cwd` still answers, with the engine-directory catalog.
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let client = comet_rpc::memory_client(core.rpc_service());

    let alpha = list_commands_in(&client, "claude-code", "/spaces/alpha").await;
    let beta = list_commands_in(&client, "claude-code", "/spaces/beta").await;
    assert_eq!(
        names(&alpha),
        [
            "architect",
            "vercel:deploy",
            "compact",
            "project:/spaces/alpha"
        ],
        "the probe ran in the requested directory"
    );
    assert_eq!(
        names(&beta),
        [
            "architect",
            "vercel:deploy",
            "compact",
            "project:/spaces/beta"
        ],
        "a second directory is a second catalog, never the first one's"
    );

    // Wire compat: a caller that omits `cwd` gets the cwd-less catalog.
    let unscoped = list_commands(&client, "claude-code").await.unwrap();
    assert_eq!(names(&unscoped), ["architect", "vercel:deploy", "compact"]);

    // And the cwd never crosses harnesses: codex answers from its own list.
    let codex = list_commands_in(&client, "codex", "/spaces/alpha").await;
    assert_eq!(
        names(&codex),
        ["imagegen", "architect", "project:/spaces/alpha"]
    );
}
