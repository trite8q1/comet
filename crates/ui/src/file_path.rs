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

use std::time::Duration;

use gpui::prelude::*;
use gpui::{ClipboardItem, Div, ElementId, SharedString, Stateful, Task, div, px};

use crate::theme::Theme;

/// The diff file header's size, where the path IS the row's content.
pub const PATH_TEXT: f32 = 12.0;
/// A path that is metadata rather than the point of its row — the plan card's
/// file line, one notch under the 12px title above it so it reads as "where
/// this lives", not as a second heading.
pub const PATH_TEXT_META: f32 = 11.0;

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
/// and a plan card each have their own. `size` is the ONE thing a caller
/// chooses ([`PATH_TEXT`] or [`PATH_TEXT_META`]): the font, the tone, the
/// `~` shortening and the basename-pinned truncation are the shared part, and
/// a path that is a row's content wants more weight than one that annotates
/// the row above it.
pub fn path_line(path: &str, theme: &Theme, size: f32) -> Div {
    let (dir, base) = split_display(&home_relative(path));
    div()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .font_family(theme.font_mono.clone())
        .text_size(px(size))
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

    /// The tick is keyed on what was COPIED — the raw path — not on the
    /// shortened line the reader sees. The two differ for anything under
    /// `$HOME`, which is where most plan files live.
    #[test]
    fn the_copy_latch_tracks_the_raw_path() {
        let mut latch = CopyLatch::default();
        assert!(!latch.shows("/Users/nico/notes/a.md"));
        latch.path = Some("/Users/nico/notes/a.md".into());
        assert!(latch.shows("/Users/nico/notes/a.md"));
        assert!(!latch.shows("/Users/nico/notes/b.md"));
        assert!(
            !latch.shows("~/notes/a.md"),
            "the displayed form must not light the tick"
        );
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

// ---------------------------------------------------------------------------
// Copying a path
// ---------------------------------------------------------------------------

/// How long the copied tick stays up — the house figure, shared by
/// `copy_sha`, `copy_message` and the code block's "Copied".
const COPIED_MS: u64 = 1_200;

/// The "just copied" flash for a path, held by whichever entity renders it.
#[derive(Default)]
pub struct CopyLatch {
    path: Option<SharedString>,
    clear: Option<Task<()>>,
}

impl CopyLatch {
    /// Whether `path` is the one showing its tick right now.
    pub fn shows(&self, path: &str) -> bool {
        self.path.as_deref() == Some(path)
    }
}

/// An entity that can copy a path and flash it.
///
/// Two entities render paths — the transcript's plan card and the changes
/// pane's file header — and the flash is per-entity state, so the BEHAVIOR is
/// shared rather than the state: one clipboard write, one duration, one way to
/// ask "is this the path I just copied?". Every other copy site in this crate
/// hand-rolled that trio and they have already drifted (1200ms here, 1500ms
/// there, three different confirmations); the two path sites will not.
pub trait PathCopy: Sized + 'static {
    fn copy_latch(&mut self) -> &mut CopyLatch;

    /// Put `path` on the clipboard and raise its tick.
    ///
    /// The RAW path goes to the clipboard, never the `~`-shortened line the
    /// user sees: a path is copied to be USED — pasted into a shell, a
    /// message, another machine — and `~` only resolves against a home
    /// directory this path may not even belong to.
    fn copy_path(&mut self, path: SharedString, cx: &mut gpui::Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
        let clear = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPIED_MS))
                .await;
            this.update(cx, |entity: &mut Self, cx| {
                entity.copy_latch().path = None;
                cx.notify();
            })
            .ok();
        });
        let latch = self.copy_latch();
        latch.path = Some(path);
        latch.clear = Some(clear);
    }
}

/// [`path_line`], plus the click that copies it and the tick that says it
/// happened. The caller owns the click (the two call sites need different
/// propagation) and supplies `copied` from its own [`CopyLatch`].
///
/// Click-to-copy rather than text selection, deliberately: gpui has no
/// selectable-text primitive, the crate's selection machinery is built for
/// the transcript's markdown and is registry-scoped to it, and a selectable
/// path would have to give up the two-part layout that keeps the FILENAME on
/// screen. Clicking a path is also fewer gestures than drag-then-copy, and it
/// is what this crate already does for a commit sha and a device id.
pub fn copyable_path_line(
    path: &str,
    id: impl Into<ElementId>,
    copied: bool,
    theme: &Theme,
    size: f32,
) -> Stateful<Div> {
    path_line(path, theme, size)
        .id(id)
        .cursor_pointer()
        // The tick rides AFTER the basename in the same flex row, so it never
        // moves the path and survives the directory truncating away. The
        // other copy sites swap their label to the word "Copied" — here that
        // would hide the very thing the user just asked to see.
        .when(copied, |el| {
            el.child(
                div().flex_none().pl(px(6.0)).child(
                    crate::icons::icon(crate::icons::CHECK)
                        .size(px(size - 1.0))
                        .text_color(theme.success_muted),
                ),
            )
        })
        // Brightening the TEXT is the hover cue, not a background wash: the
        // diff header's own row already washes on hover, and a second wash
        // inside it would read as one target, which is the opposite of what
        // a nested click needs to say.
        .when(!copied, |el| el.hover(|s| s.text_color(theme.text)))
}
