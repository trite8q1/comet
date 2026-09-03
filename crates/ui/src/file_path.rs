//! The one way Comet renders a file path.
//!
//! Before this module every path site invented its own treatment — five text
//! sizes, three colors, two font families, and a tail ellipsis everywhere that
//! cut off the *filename*, the only part a reader needs. The diff file header
//! was the best of them, so its treatment is the one kept here: monospace at
//! [`Theme::text_dim`], the token whose own doc comment calls it "the diff
//! file-path tone".
//!
//! Two rules earn their own home:
//!
//! - **The basename never truncates.** gpui's [`Styled::truncate`] is a tail
//!   ellipsis, so a long path renders as `/Users/me/very/long/dir…` — every
//!   character that identifies the file, gone. [`path_line`] pins the basename
//!   `flex_none` and truncates the directory instead. Plan files make this
//!   load-bearing rather than cosmetic: a session-scoped plan path observed
//!   live is 239 characters, of which the last 7 are the whole message.
//! - **`$HOME` shows as `~`.** Purely lexical, so it stays harness-agnostic
//!   (ARCHITECTURE.md §11.8 keeps path *knowledge* inside `crates/harness`;
//!   this module only shortens what it is handed).

use gpui::prelude::*;
use gpui::{Div, SharedString, div, px};

use crate::theme::Theme;

/// Path text size — the diff file header's, so the plan card and the changes
/// pane read as the same object.
const PATH_TEXT: f32 = 12.0;

/// `$HOME/x` → `~/x`, and a path already written with `~` is left alone
/// (the mock harness emits one, so this has to be idempotent).
///
/// Nothing else is rewritten: no percent-decoding (an encoded segment is the
/// harness's own bookkeeping and mangling it would misname the file), and no
/// relativizing against the chat's cwd — most plan files live *outside* the
/// project, where that only buys a `../../../` prefix.
pub fn home_relative(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        return path.to_string();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        // A sibling directory that merely shares the prefix
        // (`/Users/nicolas` against `HOME=/Users/nico`) is NOT under it.
        _ => path.to_string(),
    }
}

/// Split a display path into `(directory-with-trailing-slash, basename)`.
/// A bare filename has no directory half; a trailing slash makes the whole
/// thing the directory, so a folder still renders as itself.
pub fn split_display(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        None => (String::new(), path.to_string()),
        Some((_, "")) => (path.to_string(), String::new()),
        Some((dir, base)) => (format!("{dir}/"), base.to_string()),
    }
}

/// One path, laid out so the filename survives any width: the directory takes
/// the flexible slot and truncates, the basename is `flex_none`.
///
/// Returns the row's *contents* sized and colored — the caller owns the
/// surrounding height, padding and background, because a diff header, a chip
/// and a plan card each have their own.
pub fn path_line(path: &str, theme: &Theme) -> Div {
    let (dir, base) = split_display(&home_relative(path));
    div()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .font_family(theme.font_mono.clone())
        .text_size(px(PATH_TEXT))
        .text_color(theme.text_dim)
        .when(!dir.is_empty(), |el| {
            el.child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .child(SharedString::from(dir)),
            )
        })
        .when(!base.is_empty(), |el| {
            el.child(div().flex_none().child(SharedString::from(base)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_becomes_a_tilde_and_stays_one() {
        // SAFETY: single-threaded test process; no other thread reads HOME.
        unsafe { std::env::set_var("HOME", "/Users/nico") };
        assert_eq!(home_relative("/Users/nico/notes/a.md"), "~/notes/a.md");
        assert_eq!(home_relative("/Users/nico"), "~");
        // Idempotent: an already-shortened path is handed straight back.
        assert_eq!(home_relative("~/notes/a.md"), "~/notes/a.md");
        // Outside HOME, and a sibling that only shares the prefix.
        assert_eq!(home_relative("/tmp/x/a.md"), "/tmp/x/a.md");
        assert_eq!(home_relative("/Users/nicolas/a.md"), "/Users/nicolas/a.md");
        // A trailing-slash HOME must not leave a doubled separator.
        unsafe { std::env::set_var("HOME", "/Users/nico/") };
        assert_eq!(home_relative("/Users/nico/notes/a.md"), "~/notes/a.md");
    }

    #[test]
    fn split_keeps_the_basename_whole() {
        assert_eq!(
            split_display("/tmp/x/a.md"),
            ("/tmp/x/".to_string(), "a.md".to_string())
        );
        assert_eq!(
            split_display("a.md"),
            (String::new(), "a.md".to_string()),
            "a bare filename has no directory half"
        );
        assert_eq!(
            split_display("/tmp/x/"),
            ("/tmp/x/".to_string(), String::new()),
            "a directory renders as itself"
        );
        // The shape that motivates the whole module: everything identifying
        // is at the END, so the directory is what may be sacrificed.
        let long = format!("/var/sessions/{}/notes.md", "e".repeat(200));
        let (dir, base) = split_display(&long);
        assert_eq!(base, "notes.md");
        assert!(dir.len() > 200);
    }
}
