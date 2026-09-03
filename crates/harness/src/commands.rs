//! The one invocation grammar every adapter shares (ARCHITECTURE.md §10.5).
//!
//! A composer sends `/name args` as plain prompt text. Each adapter's run
//! path calls [`split_invocation`], matches the name against ITS OWN catalog,
//! and translates a hit into the harness's native wire form (claude/ACP:
//! unchanged text; codex: skill input item; opencode: command endpoint). A
//! miss is left as text so the CLI reacts exactly as it would natively.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use comet_proto::SlashCommand;

use crate::HarnessError;

/// How long a successful probe stays authoritative (ARCHITECTURE.md §10.4
/// "Freshness"): within it a call is answered from memory, after it the next
/// call re-probes.
pub(crate) const COMMANDS_TTL: Duration = Duration::from_secs(30);

/// One directory's catalog and the instant the probe behind it answered.
struct Entry {
    commands: Vec<SlashCommand>,
    probed_at: Instant,
}

/// Per-cwd memo of an adapter's discovery probe (ARCHITECTURE.md §10.4: the
/// catalog is cwd-scoped, so the cache is too), every entry expiring
/// [`COMMANDS_TTL`] after the probe that filled it.
pub(crate) struct CommandCache {
    entries: tokio::sync::Mutex<HashMap<Option<PathBuf>, Entry>>,
    /// `Instant::now` in production; a frozen, hand-advanced clock in tests.
    now: fn() -> Instant,
}

impl Default for CommandCache {
    fn default() -> Self {
        Self {
            entries: tokio::sync::Mutex::default(),
            now: Instant::now,
        }
    }
}

impl CommandCache {
    /// The catalog for `cwd`, re-probing once its entry is older than
    /// [`COMMANDS_TTL`]. A re-probe that fails keeps serving the last good
    /// entry and logs, so a transient CLI hiccup never blanks a list that was
    /// fine a moment ago; a probe that never succeeded caches nothing and is
    /// retried on the next call. The lock is held across the probe, so
    /// concurrent opens coalesce instead of racing several cold CLI boots.
    pub(crate) async fn get_or_try_init(
        &self,
        cwd: Option<&Path>,
        probe: impl AsyncFnOnce() -> Result<Vec<SlashCommand>, HarnessError>,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        let key = cwd.map(Path::to_path_buf);
        let mut entries = self.entries.lock().await;
        if let Some(hit) = entries.get(&key)
            && (self.now)().saturating_duration_since(hit.probed_at) < COMMANDS_TTL
        {
            return Ok(hit.commands.clone());
        }
        match probe().await {
            Ok(commands) => {
                entries.insert(
                    key,
                    Entry {
                        commands: commands.clone(),
                        probed_at: (self.now)(),
                    },
                );
                Ok(commands)
            }
            Err(err) => match entries.get(&key) {
                // Stale-on-error: the expired entry outlives the failed probe.
                Some(stale) => {
                    tracing::warn!(
                        target: "comet_harness::commands",
                        cwd = ?key,
                        error = %err,
                        "command discovery failed; serving the last good catalog"
                    );
                    Ok(stale.commands.clone())
                }
                None => Err(err),
            },
        }
    }

    /// A warm entry, fresh or stale, or `None` — never probes. Age is
    /// deliberately ignored here: the caller (opencode's `run`) only needs to
    /// know whether a leading `/name` is worth handing to the command
    /// endpoint, the server resolves the name itself, and the run's own
    /// `/command` fetch is the authority — so an old list costs nothing while
    /// a re-probe would delay the run.
    pub(crate) async fn get(&self, cwd: Option<&Path>) -> Option<Vec<SlashCommand>> {
        self.entries
            .lock()
            .await
            .get(&cwd.map(Path::to_path_buf))
            .map(|entry| entry.commands.clone())
    }

    /// Seed an entry a neighbouring probe already answered, stamped now.
    pub(crate) async fn insert(&self, cwd: Option<&Path>, commands: Vec<SlashCommand>) {
        self.entries.lock().await.insert(
            cwd.map(Path::to_path_buf),
            Entry {
                commands,
                probed_at: (self.now)(),
            },
        );
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
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    thread_local! {
        /// The clock [`CommandCache::frozen`] reads: one per test thread, so
        /// tests advance time without sleeping and without seeing each other.
        static CLOCK: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    fn frozen_now() -> Instant {
        CLOCK.with(|clock| {
            let now = clock.get().unwrap_or_else(Instant::now);
            clock.set(Some(now));
            now
        })
    }

    fn advance(by: Duration) {
        let next = frozen_now() + by;
        CLOCK.with(|clock| clock.set(Some(next)));
    }

    impl CommandCache {
        /// A cache whose clock only moves when [`advance`] says so.
        fn frozen() -> Self {
            CLOCK.with(|clock| clock.set(Some(Instant::now())));
            Self {
                entries: tokio::sync::Mutex::default(),
                now: frozen_now,
            }
        }
    }

    /// A probe that counts its calls and answers with a one-command catalog.
    async fn probe(probes: &AtomicUsize, name: &str) -> Result<Vec<SlashCommand>, HarnessError> {
        probes.fetch_add(1, Ordering::SeqCst);
        Ok(vec![cmd(name)])
    }

    /// A probe that counts its calls and fails, as a broken CLI would.
    async fn failing_probe(probes: &AtomicUsize) -> Result<Vec<SlashCommand>, HarnessError> {
        probes.fetch_add(1, Ordering::SeqCst);
        Err(HarnessError::Protocol("cli unavailable".into()))
    }

    fn names(commands: &[SlashCommand]) -> Vec<&str> {
        commands.iter().map(|c| c.name.as_str()).collect()
    }

    #[tokio::test]
    async fn commands_inside_the_ttl_come_from_memory() {
        let cache = CommandCache::frozen();
        let probes = AtomicUsize::new(0);

        let cold = cache
            .get_or_try_init(None, async || probe(&probes, "one").await)
            .await
            .expect("cold probe");
        advance(COMMANDS_TTL - Duration::from_secs(1));
        let warm = cache
            .get_or_try_init(None, async || probe(&probes, "two").await)
            .await
            .expect("warm hit");

        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(names(&warm), names(&cold));
    }

    #[tokio::test]
    async fn commands_are_reprobed_once_the_ttl_expires() {
        let cache = CommandCache::frozen();
        let probes = AtomicUsize::new(0);

        cache
            .get_or_try_init(None, async || probe(&probes, "one").await)
            .await
            .expect("cold probe");
        advance(COMMANDS_TTL);
        let fresh = cache
            .get_or_try_init(None, async || probe(&probes, "two").await)
            .await
            .expect("re-probe");

        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(names(&fresh), ["two"]);
        assert_eq!(names(&cache.get(None).await.expect("warm")), ["two"]);
    }

    #[tokio::test]
    async fn a_failed_reprobe_keeps_serving_the_last_good_skill_list() {
        let cache = CommandCache::frozen();
        let probes = AtomicUsize::new(0);

        cache
            .get_or_try_init(None, async || probe(&probes, "one").await)
            .await
            .expect("cold probe");
        advance(COMMANDS_TTL);
        let stale = cache
            .get_or_try_init(None, async || failing_probe(&probes).await)
            .await
            .expect("stale entry, not an error");

        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(names(&stale), ["one"]);
        // The failure did not clear the entry: it survives another one.
        let still = cache
            .get_or_try_init(None, async || failing_probe(&probes).await)
            .await
            .expect("stale entry again");
        assert_eq!(probes.load(Ordering::SeqCst), 3);
        assert_eq!(names(&still), ["one"]);
        assert_eq!(names(&cache.get(None).await.expect("warm")), ["one"]);
    }

    #[tokio::test]
    async fn a_first_failed_probe_caches_no_commands_and_is_retried() {
        let cache = CommandCache::frozen();
        let probes = AtomicUsize::new(0);

        let err = cache
            .get_or_try_init(None, async || failing_probe(&probes).await)
            .await
            .expect_err("nothing to serve");
        assert!(matches!(err, HarnessError::Protocol(_)));
        assert!(cache.get(None).await.is_none());

        // The very next call retries, without waiting out the TTL.
        let recovered = cache
            .get_or_try_init(None, async || probe(&probes, "one").await)
            .await
            .expect("retry");
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert_eq!(names(&recovered), ["one"]);
    }

    #[tokio::test]
    async fn commands_for_two_cwds_age_independently() {
        let cache = CommandCache::frozen();
        let probes = AtomicUsize::new(0);
        let (a, b) = (Path::new("/spaces/a"), Path::new("/spaces/b"));

        cache
            .get_or_try_init(Some(a), async || probe(&probes, "a-one").await)
            .await
            .expect("cold a");
        advance(COMMANDS_TTL / 2 + Duration::from_secs(1));
        cache
            .get_or_try_init(Some(b), async || probe(&probes, "b-one").await)
            .await
            .expect("cold b");
        advance(COMMANDS_TTL / 2 + Duration::from_secs(1));

        // `a` is past the TTL, `b` is not.
        let a_now = cache
            .get_or_try_init(Some(a), async || probe(&probes, "a-two").await)
            .await
            .expect("re-probe a");
        let b_now = cache
            .get_or_try_init(Some(b), async || probe(&probes, "b-two").await)
            .await
            .expect("warm b");

        assert_eq!(probes.load(Ordering::SeqCst), 3);
        assert_eq!(names(&a_now), ["a-two"]);
        assert_eq!(names(&b_now), ["b-one"]);
    }

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
