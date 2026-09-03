# ADR 0005: Plan mode is a projection of the harness's own plan mode

- Status: Accepted
- Date: 2026-09-02

## Context

Every agent CLI Comet drives has a plan mode with its own enter/exit path, plan
storage, and approval gate (ARCHITECTURE.md §11.2). Comet exposed none of them
and auto-approved every permission request, so a harness's plan-exit gate would
have been approved unread. Users expect the same plan/approve cycle they get in
the CLI, from the desktop and the phone, with the plan visible while it changes.

## Decision

- Comet carries one bit, `plan_mode`, requested per chat (`ChatConfig`) and
  reported by the harness (`PlanModeChanged`). The host reconciles requested to
  reported on every report.
- The plan is whatever the harness produced; it is folded into the session doc as
  `MessagePart::Plan`, one per segment, refreshed in place. The UI renders that
  part and nothing else. No comet-side plan store, no synthesized plan prompts.
- The exit gate is the harness's own request, bridged through the engine like a
  user question (`request_plan_exit` mirrors `request_input`; `RespondPlanExit`
  mirrors `RespondInput`). Where the CLI has no agent-initiated gate the user
  leaves plan mode with the toggle, as in that CLI.
- A harness whose wire has no plan-mode entry point (Codex app-server 0.151–0.152)
  reports `plan_mode() == false` and carries a tripwire test that fails the moment
  the wire grows one.
- Adapters are the only place that knows plan files, tool names, or mode ids.

## Consequences

- Adding a harness is an adapter plus an icon; the doc, engine, and both UIs are
  untouched.
- Old clients degrade safely: the plan part's body field is `plan`, so a pre-plan
  desktop renders nothing and iOS drops the unknown kind; the new config/request
  fields are serde-defaulted.
- The plan card is history: each plan-mode episode leaves its card at the turn
  that produced it, like the CLIs' own transcripts.
