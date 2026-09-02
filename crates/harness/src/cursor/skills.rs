//! Cursor Agent Skills → the invocable catalog (ARCHITECTURE.md §10.4's
//! filesystem exception).
//!
//! WHY A SCAN HERE. Cursor's wire is the pinned `@cursor/sdk` (see
//! [`super`]), and 1.0.28 exposes no listing and no skill input:
//! `Agent`/`Cursor` carry only `models`/`repositories`/`auth`/`messages`
//! (`stubs.d.ts`), `SendOptions` is `{model, mcpServers, mode, onStep,
//! onDelta, local, cloud, idempotencyKey}` and `SDKUserMessage` is
//! `{text, images?}` (`agent.d.ts`, `options.d.ts`) — skills appear in the
//! typings only as something the workspace scan loads ("project rules,
//! skills, and request-context workspace metadata load from every unique
//! path", `LocalAgentOptions.dirs`). The CLI's own listing surfaces need
//! `cursor-agent login`, which is SEPARATE from the SDK credentials comet
//! runs on (verified: `cursor-agent acp` → `session/new` answers
//! `Authentication required` on a machine whose SDK key works), so they are
//! not a probe comet can rely on. Hence the scan, adapter-local, over the
//! roots the agent itself reads.
//!
//! ROOTS AND PRECEDENCE, from Cursor's own code (identical in both):
//! `@cursor/sdk` 1.0.28 `dist/esm/357.js` and `cursor-agent`
//! 2026.04.17-787b533 `index.js`
//! (`ls`/`cs`/`us` = builtin/project/user root builders), and matching the
//! docs (cursor.com/docs/skills: project `.agents/skills/` + `.cursor/skills/`,
//! user `~/.agents/skills/` + `~/.cursor/skills/`, "for compatibility"
//! `.claude/skills/` and `.codex/skills/`, "Cursor walks the skills root
//! recursively and picks up any `SKILL.md` it finds"). The CLI loads them in
//! the order builtin → project → user into one id-keyed map
//! (`CustomCommandsService.loadSkillRoots`), so a later root wins a name it
//! shares with an earlier one.
//!
//! WHAT IS OFFERED. The invocable name is the SKILL.md's own directory name —
//! the CLI's palette submits `/${id}` for it and resolves `/name` back through
//! `getSkillById(id)`, with the frontmatter `name` used only as the row's
//! title. `metadata.surfaces`, when present, gates the surface: the CLI drops
//! any skill whose surfaces omit `"cli"` (`parseSkillMarkdown`), and so does
//! comet. `disable-model-invocation: true` is NOT a filter — it is the
//! opposite, a skill the docs say is "only included when explicitly invoked
//! via /skill-name".
//!
//! The walk is recursive, per the docs ("Cursor walks the skills root
//! recursively and picks up any `SKILL.md` it finds") and the runtime comet
//! actually drives; the CLI's palette happens to read one level only, so a
//! skill nested under a category folder is offered here and not there. The
//! run gets it either way — it is loaded by the same workspace scan.
//!
//! Cursor's *custom commands* (`.cursor/commands`, `.claude/commands`, team
//! and global) are deliberately absent: the CLI expands their markdown body
//! client-side before sending, and §10.5 forbids comet pre-expanding a
//! command, so offering them would promise something this wire cannot do.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use comet_proto::SlashCommand;

/// Depth cap for the recursive walk of one skills root, mirroring the
/// agent's own bounded workspace walk (`maxDepth: 10`).
const MAX_DEPTH: usize = 10;

/// `.codex/skills` entries Cursor never offers — Codex's own built-ins,
/// skipped by name in both the CLI and the SDK (`os`/`et` set).
const CODEX_BUILTIN_SKILLS: &[&str] = &[
    "imagegen",
    "openai-docs",
    "opneai-docs",
    "plugin-creator",
    "skill-creator",
    "skill-installer",
];

/// Every skills root, in the agent's load order: built-in, then the project's,
/// then the user's. A name found twice keeps its first position and takes the
/// later root's description — the CLI's map semantics.
fn roots(home: &Path, project_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".cursor").join("skills-cursor")];
    for base in project_root.into_iter().chain(std::iter::once(home)) {
        for config_dir in [".claude", ".codex", ".agents", ".cursor"] {
            roots.push(base.join(config_dir).join("skills"));
        }
    }
    roots
}

/// The catalog for one `(HOME, project root)` pair. Missing roots are simply
/// absent; an unreadable file or directory is skipped, never an error — the
/// popup's job is to complete what the agent would accept, and the agent
/// tolerates the same.
pub fn scan(home: &Path, project_root: Option<&Path>) -> Vec<SlashCommand> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, SlashCommand> = HashMap::new();
    for root in roots(home, project_root) {
        let codex_root = root.parent().and_then(Path::file_name) == Some(".codex".as_ref());
        for dir in skill_dirs(&root) {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if codex_root && CODEX_BUILTIN_SKILLS.contains(&name) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else {
                continue;
            };
            let front = Frontmatter::parse(&text);
            if !front.surfaces.is_empty() && !front.surfaces.iter().any(|s| s == "cli") {
                continue;
            }
            let command = SlashCommand {
                name: name.to_owned(),
                description: front.description.unwrap_or_default(),
                input_hint: None,
                aliases: Vec::new(),
            };
            if by_name.insert(name.to_owned(), command).is_none() {
                order.push(name.to_owned());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect()
}

/// Directories holding a `SKILL.md` under `root`, recursively, sorted by path
/// so the catalog is stable across filesystems. Dot-entries are skipped (the
/// agent ignores any path segment starting with `.` below a skills root).
fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_skill_dirs(root, 0, &mut found);
    found.sort();
    found
}

fn collect_skill_dirs(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // metadata (not symlink_metadata): `~/.cursor/skills-cursor` is a
        // symlink to `~/.agents/skills` on a synced install, and skills
        // themselves are commonly symlinked in.
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            found.push(path.clone());
        }
        collect_skill_dirs(&path, depth + 1, found);
    }
}

/// The two frontmatter fields this catalog needs. Cursor parses SKILL.md
/// frontmatter with gray-matter (YAML); comet reads only `description` and
/// `metadata.surfaces` — never the body, which is the agent's to load.
#[derive(Default)]
struct Frontmatter {
    description: Option<String>,
    surfaces: Vec<String>,
}

impl Frontmatter {
    fn parse(text: &str) -> Self {
        let mut out = Self::default();
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some("---") {
            return out;
        }
        let block: Vec<&str> = lines.take_while(|l| l.trim() != "---").collect();
        let mut i = 0;
        while i < block.len() {
            let line = block[i];
            i += 1;
            if line.starts_with([' ', '\t']) {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "description" => {
                    out.description = Some(if value.starts_with(['>', '|']) {
                        // A block scalar: the indented lines that follow,
                        // folded (`>`) onto one line or kept (`|`) as they are.
                        let joiner = if value.starts_with('|') { "\n" } else { " " };
                        take_indented(&block, &mut i)
                            .iter()
                            .map(|l| l.trim())
                            .collect::<Vec<_>>()
                            .join(joiner)
                    } else {
                        // A plain scalar, possibly continued on indented lines
                        // (`description:` followed by an indented paragraph is
                        // the shape skills written by hand often take); YAML
                        // folds those continuation lines with single spaces.
                        let mut text = unquote(value).to_owned();
                        for line in take_indented(&block, &mut i) {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(line.trim());
                        }
                        text
                    });
                }
                "metadata" => {
                    let nested = take_indented(&block, &mut i);
                    let mut j = 0;
                    while j < nested.len() {
                        let line = nested[j];
                        j += 1;
                        let Some((key, value)) = line.split_once(':') else {
                            continue;
                        };
                        if key.trim() != "surfaces" {
                            continue;
                        }
                        let value = value.trim();
                        if value.is_empty() {
                            // A YAML list on the following lines.
                            while let Some(item) =
                                nested.get(j).and_then(|l| l.trim().strip_prefix('-'))
                            {
                                out.surfaces.push(unquote(item.trim()).to_owned());
                                j += 1;
                            }
                        } else {
                            out.surfaces = inline_list(value);
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// The indented continuation lines starting at `i`, advancing `i` past them.
fn take_indented<'a>(block: &[&'a str], i: &mut usize) -> Vec<&'a str> {
    let start = *i;
    while *i < block.len() && (block[*i].starts_with([' ', '\t']) || block[*i].trim().is_empty()) {
        *i += 1;
    }
    block[start..*i]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// `cli` or `[cli, ide]` — a scalar or a flow sequence on one line.
fn inline_list(value: &str) -> Vec<String> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| unquote(s.trim()).to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// The project root the agent would scan for `cwd`: its git root when there
/// is one (`cursor-agent` resolves the workspace the same way), else `cwd`.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut dir = cwd;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return cwd.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Frontmatter;

    #[test]
    fn skill_description_scalars_parse_like_yaml() {
        // Plain one-liner, quoted or not.
        assert_eq!(
            Frontmatter::parse("---\ndescription: \"Review a PR\"\n---\n").description,
            Some("Review a PR".into())
        );
        // Plain scalar continued on indented lines (a hand-written
        // paragraph after a bare `description:`): folded with spaces.
        let plain = "---\nname: x\ndescription:\n  React composition patterns.\n  Use when refactoring.\nlicense: MIT\n---\n";
        assert_eq!(
            Frontmatter::parse(plain).description,
            Some("React composition patterns. Use when refactoring.".into())
        );
        // The same with text on the first line too.
        let mixed =
            "---\ndescription: First line\n  second line\nmetadata:\n  surfaces: [cli]\n---\n";
        let front = Frontmatter::parse(mixed);
        assert_eq!(front.description, Some("First line second line".into()));
        assert_eq!(front.surfaces, vec!["cli".to_string()]);
        // Folded and literal block scalars keep their own joining rules.
        assert_eq!(
            Frontmatter::parse("---\ndescription: >\n  a\n  b\n---\n").description,
            Some("a b".into())
        );
        assert_eq!(
            Frontmatter::parse("---\ndescription: |\n  a\n  b\n---\n").description,
            Some("a\nb".into())
        );
    }
}
