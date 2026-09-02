//! The one invocation grammar every adapter shares (ARCHITECTURE.md §10.5).
//!
//! A composer sends `/name args` as plain prompt text. Each adapter's run
//! path calls [`split_invocation`], matches the name against ITS OWN catalog,
//! and translates a hit into the harness's native wire form (claude/ACP:
//! unchanged text; codex: skill input item; opencode: command endpoint). A
//! miss is left as text so the CLI reacts exactly as it would natively.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use comet_proto::SlashCommand;

use crate::HarnessError;

/// Per-cwd memo of an adapter's discovery probe (ARCHITECTURE.md §10.4: the
/// catalog is cwd-scoped, so the cache is too).
#[derive(Default)]
pub(crate) struct CommandCache {
    entries: tokio::sync::Mutex<HashMap<Option<PathBuf>, Vec<SlashCommand>>>,
}

impl CommandCache {
    /// The catalog for `cwd`, probing once per directory. Only a successful
    /// probe is cached, so a broken CLI retries on the next picker open; the
    /// lock is held across the probe, so concurrent opens coalesce instead of
    /// racing several cold CLI boots.
    pub(crate) async fn get_or_try_init(
        &self,
        cwd: Option<&Path>,
        probe: impl AsyncFnOnce() -> Result<Vec<SlashCommand>, HarnessError>,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        let key = cwd.map(Path::to_path_buf);
        let mut entries = self.entries.lock().await;
        if let Some(hit) = entries.get(&key) {
            return Ok(hit.clone());
        }
        let commands = probe().await?;
        entries.insert(key, commands.clone());
        Ok(commands)
    }

    /// A warm entry, or `None` — never probes.
    pub(crate) async fn get(&self, cwd: Option<&Path>) -> Option<Vec<SlashCommand>> {
        self.entries
            .lock()
            .await
            .get(&cwd.map(Path::to_path_buf))
            .cloned()
    }

    /// Seed an entry a neighbouring probe already answered.
    pub(crate) async fn insert(&self, cwd: Option<&Path>, commands: Vec<SlashCommand>) {
        self.entries
            .lock()
            .await
            .insert(cwd.map(Path::to_path_buf), commands);
    }
}

/// A leading `/name [args]` split out of a prompt. `args` is the trimmed
/// remainder (empty when the command was sent bare).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation<'a> {
    pub name: &'a str,
    pub args: &'a str,
}

/// Parse the prompt's leading invocation, if it has one. The `/` must be the
/// very first character (the composer's own token rule: slash commands are
/// whole-prompt prefixes, never inline), the name runs to the first
/// whitespace, and a bare `/` or a path-like `/usr/bin` is not an invocation.
pub fn split_invocation(prompt: &str) -> Option<Invocation<'_>> {
    let rest = prompt.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(Invocation {
        name,
        args: rest[end..].trim(),
    })
}

/// [`split_invocation`] restricted to names the catalog advertises (name or
/// alias). Adapters translate only these; anything else stays prompt text.
pub fn known_invocation<'a>(
    prompt: &'a str,
    catalog: &'a [SlashCommand],
) -> Option<(Invocation<'a>, &'a SlashCommand)> {
    let invocation = split_invocation(prompt)?;
    let command = catalog.iter().find(|c| c.matches(invocation.name))?;
    Some((invocation, command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn splits_name_and_trimmed_args() {
        assert_eq!(
            split_invocation("/review  42 please "),
            Some(Invocation {
                name: "review",
                args: "42 please"
            })
        );
        assert_eq!(
            split_invocation("/compact"),
            Some(Invocation {
                name: "compact",
                args: ""
            })
        );
        assert_eq!(
            split_invocation("/vercel:deploy prod"),
            Some(Invocation {
                name: "vercel:deploy",
                args: "prod"
            })
        );
        assert_eq!(
            split_invocation("/skill:research\nmulti\nline"),
            Some(Invocation {
                name: "skill:research",
                args: "multi\nline"
            })
        );
    }

    #[test]
    fn rejects_non_invocations() {
        assert_eq!(split_invocation("run /compact"), None);
        assert_eq!(split_invocation("/"), None);
        assert_eq!(split_invocation("/ compact"), None);
        assert_eq!(split_invocation("/usr/bin/env"), None);
        assert_eq!(split_invocation(""), None);
    }

    #[test]
    fn known_invocation_matches_only_the_given_catalog() {
        let catalog = vec![cmd("review"), cmd("compact")];
        let (inv, command) = known_invocation("/review 42", &catalog).expect("known");
        assert_eq!(inv.name, "review");
        assert_eq!(inv.args, "42");
        assert_eq!(command.name, "review");
        // A name from some OTHER harness's catalog is never translated.
        assert!(known_invocation("/imagegen cat", &catalog).is_none());
        assert!(known_invocation("plain text", &catalog).is_none());
    }

    #[test]
    fn known_invocation_matches_aliases() {
        let mut review = cmd("review");
        review.aliases = vec!["r".into()];
        let catalog = vec![review];
        let (inv, command) = known_invocation("/r 42", &catalog).expect("alias hit");
        assert_eq!(inv.name, "r");
        assert_eq!(command.name, "review");
    }
}
