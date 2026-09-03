#!/bin/sh
# Fake comet cursor shim for comet-harness tests: speaks the shim's JSONL
# protocol (see crates/harness/src/cursor/shim.mjs) without node or the SDK.
# Driven by crates/harness/tests/cursor.rs.

emit() { printf '%s\n' "$1"; }

# Models mode (argv, no stdin protocol): one catalog frame, real 1.0.28
# shapes — parameterized Auto + its bare `default` alias twin (skipped by the
# harness) + a plain model.
if [ "$1" = "models" ]; then
  emit '{"ev":"models","items":[{"id":"auto-smart","displayName":"Auto","parameters":[{"id":"optimize_for","displayName":"Optimize For","values":[{"value":"intelligence","displayName":"Intelligence"},{"value":"balanced","displayName":"Balance"},{"value":"cost","displayName":"Cost"}]}],"variants":[{"params":[{"id":"optimize_for","value":"balanced"}],"displayName":"Auto","isDefault":true}]},{"id":"default","displayName":"Auto","aliases":["auto"]},{"id":"claude-fable-5","displayName":"Claude Fable 5","description":"Anthropic frontier","parameters":[{"id":"thinking","values":[{"value":"enabled"},{"value":"disabled"}]}]}]}'
  exit 0
fi

read -r first || exit 1
case "$first" in
*'"op":"run"'*) ;;
*) emit '{"ev":"fatal","message":"expected op run first"}'; exit 1 ;;
esac

case "$first" in

*scenario:happy*)
  emit '{"ev":"ready","agentId":"agent-1","model":"composer-2.5"}'
  emit '{"ev":"thinking","text":"planning"}'
  emit '{"ev":"text","text":"Hello from cursor"}'
  emit '{"ev":"tool","phase":"start","id":"c1","name":"shell","args":{"command":"ls -la"}}'
  emit '{"ev":"tool","phase":"end","id":"c1","name":"shell","args":{"command":"ls -la"},"error":false}'
  # A spawned subagent: the task chip on the parent feed, its interior tagged.
  emit '{"ev":"tool","phase":"start","id":"task1","name":"task","args":{"description":"scan repo"}}'
  emit '{"ev":"text","text":"sub scanning","parent":"task1"}'
  emit '{"ev":"tool","phase":"start","id":"s1","name":"grep","args":{"pattern":"todo"},"parent":"task1"}'
  emit '{"ev":"tool","phase":"end","id":"s1","name":"grep","args":{"pattern":"todo"},"error":false,"parent":"task1"}'
  emit '{"ev":"tool","phase":"end","id":"task1","name":"task","args":{"description":"scan repo"},"error":false}'
  # Unknown frame kinds must be tolerated.
  emit '{"ev":"someNewThing","x":1}'
  emit '{"ev":"usage","input":11,"output":5}'
  emit '{"ev":"turn","status":"finished"}'
  # Parked: wait for a follow-up or stdin EOF.
  read -r next || exit 0
  case "$next" in
  *'"op":"user"'*)
    emit '{"ev":"text","text":"second turn"}'
    emit '{"ev":"turn","status":"finished"}'
    ;;
  esac
  exit 0
  ;;

*scenario:plan*)
  # Plan mode (ARCHITECTURE.md §11.2, Cursor row): the client owns the mode,
  # so it must ride the run frame and every send. Append the raw stdin frames
  # to a log in the run's cwd; the test reads back the `mode` of each. The
  # `createPlan` call carries the plan the adapter turns into a plan part.
  printf '%s\n' "$first" >>plan-mode.jsonl
  emit '{"ev":"ready","agentId":"agent-plan","model":"auto"}'
  emit '{"ev":"tool","phase":"start","id":"p1","name":"createPlan","args":{"plan":"# Port the veil\n\n1. Move the fade into the row painter.\n"}}'
  emit '{"ev":"tool","phase":"end","id":"p1","name":"createPlan","args":{"plan":"# Port the veil\n\n1. Move the fade into the row painter.\n"},"error":false}'
  emit '{"ev":"turn","status":"finished"}'
  read -r steer || exit 0
  printf '%s\n' "$steer" >>plan-mode.jsonl
  emit '{"ev":"text","text":"building it"}'
  emit '{"ev":"turn","status":"finished"}'
  exit 0
  ;;

*scenario:interrupt*)
  emit '{"ev":"ready","agentId":"agent-int","model":"auto"}'
  emit '{"ev":"text","text":"working"}'
  read -r msg || exit 0
  case "$msg" in
  *'"op":"interrupt"'*)
    emit '{"ev":"turn","status":"cancelled"}'
    ;;
  esac
  exit 0
  ;;

*scenario:fatal*)
  emit '{"ev":"fatal","message":"Cursor SDK is not authenticated (its login is separate from `cursor-agent login`): set CURSOR_API_KEY from cursor.com/settings, then retry."}'
  exit 1
  ;;

*scenario:crash*)
  emit '{"ev":"ready","agentId":"agent-c","model":"auto"}'
  echo "shim exploded" >&2
  exit 3
  ;;

*'"prompt":"/'*)
  # Slash parity (ARCHITECTURE.md §10.5): Cursor's own surfaces send a skill
  # invocation as plain user text — its palette submits `/<skill> <args>` and
  # its ACP server passes an unmatched `/name` through untouched — so the
  # adapter must not touch it. Append the raw stdin frames (the run prompt,
  # then the steer) to a log in the run's cwd; the test compares them.
  printf '%s\n' "$first" >>slash-parity.jsonl
  emit '{"ev":"ready","agentId":"agent-slash","model":"auto"}'
  emit '{"ev":"turn","status":"finished"}'
  read -r steer || exit 0
  printf '%s\n' "$steer" >>slash-parity.jsonl
  emit '{"ev":"turn","status":"finished"}'
  exit 0
  ;;

*)
  emit '{"ev":"fatal","message":"unknown scenario"}'
  exit 1
  ;;
esac
