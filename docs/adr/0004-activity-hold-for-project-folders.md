# ADR 0004: Activity hold for project folders

- Status: Accepted
- Date: 2026-09-01

## Context

In the "By project" sidebar view a folder ranks by its newest chat. With
several projects running agents in parallel, every finished turn permutes the
top folders. Among simultaneously active projects that reorder carries almost
no information (they are all in play) while it costs spatial memory on every
turn. The ordering between active and dormant projects does carry
information, and the recency feel is part of Comet's sidebar language:
position means recency, the dot means urgency, reorders glide.

## Decision

Apply hysteresis at the folder level only, under Sort = Last updated:

- Folders with activity inside a hold window (a tunable constant, 4 hours to
  start) form an active block ordered by the moment each entered it. That
  entry time is frozen while the folder stays active, so turns inside active
  projects never reorder the block. Active projects behave like tabs.
- A dormant folder that becomes active is inserted at the top of the block.
  A folder whose activity ages past the window drops out of the block into
  the dormant tail, which stays a recency feed. "No project" is pinned last
  and never held.
- Activity is derived from data the sidebar already has: a persisted message
  (`last_message_at`, written for user prompts and agent messages alike) or
  a live Working / AwaitingInput session, which counts as activity now and so
  keeps a folder in the block for as long as it is live. Selecting a chat,
  picking a project in the composer, hovering, and collapsing do not touch
  these inputs and therefore never count.
- Chats inside a folder keep pure recency ordering, identical to main.
  Attention states (needs input, finished but unseen) are shown by the row's
  status word and the folder's aggregate dot, never by position: promoting
  such rows would reintroduce the "row jumps the moment you open it" problem
  that `sort_active` documents and avoids.
- The hold map is session-transient (like collapse state): after a restart
  every active folder enters at once and the block seeds in recency order.
  No settings, no persistence, no drag-and-drop, no visible tier separator
  for now.

One shared pure function computes the order for both the renderer (which
stores the next hold map) and the keyboard jump order (which recomputes and
discards it), so screen and keys cannot disagree.

## Consequences

- Parallel work across projects no longer shuffles folders per turn; the only
  folder moves are a project starting (to the top) or going quiet (below the
  block), both animated by the existing resort glide.
- The recency dynamics remain where they carry information: the flat
  "In one list" feed, chats inside folders, the dormant tail, and the
  archived shelf are unchanged.
- Time-based expiry surfaces on the next render rather than on its own
  timer; the once-per-second ticker only runs while the selected chat is live
  or connectivity is degraded. One small move at an attention boundary,
  accepted.
- A folder pinned by a Working session cannot stick forever: the 45s
  session-staleness gate turns a dead session Idle.
