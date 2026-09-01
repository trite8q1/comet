# ADR 0003: Merged project folders in the sidebar

- Status: Accepted (amended 2026-08-31: the manual-ordering package was
  removed after a simplification pass; folders order by recency)
- Date: 2026-08-31

## Context

The sidebar's "All projects" view groups chats into collapsible project
folders. A short-lived per-device folder mode (one folder per Space) rendered
the same repo as multiple identically named folders in unrelated positions.
A manual-ordering design (frozen positions plus drag-and-drop) was built and
then removed: it added a settings field, drag machinery, and a second
ordering model for marginal benefit over the recency model the rest of the
sidebar already uses.

## Decision

There is one folder mode, "By project" (`SidebarOrganization::ByProjectMerged`,
the default): one folder per project name, merged across devices. The
per-device mode is removed from the menu and its persisted value normalizes
to the merged mode on load. "By device" and "In one list" remain unchanged.

Folders are purely a UI layer over main's behavior. Chats sort exactly as
before (the Sort setting: Last updated / Created); folders form by first
appearance in that sorted walk, so a folder ranks by its newest chat and
floats on activity, exactly as flat rows do. The "No project" bucket is
pinned last. The archived shelf nests its chats in the same folders, dimmed,
with independent collapse state (`archived-folder:` keys). Nothing about
folder order is persisted and nothing drags.

One signal is added: a folder header carries an attention dot colored by its
most urgent nested status (`attention_rank`). This is not extra polish — a
collapsed folder hides its chats' status rows, which are always visible on
main, and the dot restores that information.

Folder identity is the lowercased display name. A rename re-buckets the
folder; two same-named repos merge into one folder. Both are accepted as
rare and self-healing. The device is row-level metadata, not structure:
chats wear a device tag only when their folder spans machines.

## Consequences

- The sidebar behaves like main with a folder layer on top: same sorts, same
  recency dynamics (a completed run floats its folder), same archived-shelf
  semantics, plus grouping, per-folder paging, and the collapse affordance.
- Collapsed folders stay informative via the attention dot and the "(count)"
  label.
- No new persisted state: collapse and paging are session-transient, like
  the device groups before them.
