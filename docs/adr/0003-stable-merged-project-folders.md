# ADR 0003: Stable merged project folders in the sidebar

- Status: Accepted
- Date: 2026-08-31

## Context

The sidebar's "All projects" view groups chats into collapsible project
folders. Two forces conflict: spatial stability (fixed folder positions build
muscle memory; nothing jumps mid-work) and activity surfacing (the chat that
just finished should be easy to find). Ranking folders by their newest chat's
`last_message_at` made a single completed run yank an entire folder across
the sidebar. A short-lived per-device folder mode (one folder per Space)
additionally rendered the same repo as multiple identically named folders in
unrelated positions.

## Decision

There is one folder mode, "By project" (`SidebarOrganization::ByProjectMerged`,
the default): one folder per project name, merged across devices. The
per-device mode is removed from the menu and its persisted value normalizes
to the merged mode on load. "By device" and "In one list" remain; the flat
list is the recency-driven activity feed.

Folder positions are manual and stable. The order lives in the
`sidebar_folder_order` setting (merged name keys, topmost first): seeded from
recency when folders first materialize, new projects prepend, drag-and-drop
rewrites it, and the "No project" bucket is pinned last and never persisted.
Keys of vanished folders are kept so a returning project regains a slot. The
Sort setting (Last updated / Created) orders chats inside folders only.

Activity never reorders folders. Each folder header carries an attention dot
colored by its most urgent nested status (`attention_rank`); a collapsed
folder keeps its count and its dot.

Folder identity is the lowercased display name. A rename re-buckets the
folder, which re-enters at the top as a new project while the old key
lingers harmlessly in the stored order; two same-named repos merge into one
folder. Both are accepted as rare and self-healing.

## Consequences

- Folders hold still during and after agent runs; the dot carries urgency,
  and the global recency feed is one Organize switch away.
- Drag-and-drop reuses the surface-tab strip's drag recipe (payload, ghost
  chip, drop-slot math over the rendered section heights, FLIP glide on
  commit), keeping one drag idiom across the app.
- The device is row-level metadata, not structure: chats wear a device tag
  only when their folder spans machines.
- Manual order is device-local (settings file), matching tabs and other
  viewport state; it does not sync across devices.
