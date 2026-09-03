#!/usr/bin/env bash
# Verification loop for native plan mode (ARCHITECTURE.md §11.8).
#
# Proves, offline and deterministically:
#   1. architecture guards            (harness-agnostic UI/RPC plan paths; plan
#                                      files, tool names and prompts live only in
#                                      crates/harness)
#   2. shared substrate               (proto wire compat, doc part + fold, ledger,
#                                      engine bridge/watch/reconcile)
#   3. adapters against fixture CLIs  (per-harness `plan` tests)
#   4. UI row building + phone tests  (desktop `plan` unit tests; --ios)
#   5. fmt + clippy with no new diagnostics on lines added vs main
#
# Usage: scripts/verify-plan-mode.sh [--live] [--ios]
#   --live  additionally runs the #[ignore] `live_plan` tests against the REAL
#           agent CLIs installed here (one model turn each where a gate needs it).
#   --ios   additionally runs the phone plan-card/composer unit tests (needs Xcode).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"

LIVE=0
IOS=0
for arg in "$@"; do
  case "$arg" in
    --live) LIVE=1 ;;
    --ios) IOS=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

FAILED=0
step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf '\033[31mFAIL\033[0m %s\n' "$*"; FAILED=1; }
ok() { printf '\033[32mok\033[0m   %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. Architecture guards — the invariants as executable checks.
# ---------------------------------------------------------------------------
step "architecture guards"

# The transcript's plan card and the composer's toggle branch on the plan
# part and the harness DESCRIPTOR, never on harness identity.
for f in crates/ui/src/transcript.rs crates/ui/src/composer.rs; do
  if grep -n 'HarnessId::' "$f" >/dev/null; then
    fail "$f matches on HarnessId (plan UI must stay harness-agnostic)"
  else
    ok "$(basename "$f") is harness-agnostic"
  fi
done

# rpc.rs: production code must not match on HarnessId variants.
if awk '/#\[cfg\(test\)\]/{exit} /HarnessId::/{found=1} END{exit !found}' crates/engine/src/rpc.rs; then
  fail "crates/engine/src/rpc.rs matches on HarnessId outside tests"
else
  ok "engine rpc layer is harness-agnostic"
fi

# Plan files, plan-exit tool names, and mode ids are adapter knowledge.
# Matched as STRING LITERALS (quoted): identifiers such as the engine's
# `pending_plan_exits` are comet's own vocabulary, not CLI knowledge.
if grep -rnE '"[^"]*(\.claude/plans|\.opencode/plans|plans/\*\.md|ExitPlanMode|plan_exit|exit_plan_mode|set_permission_mode|collaborationMode|createPlan)[^"]*"' \
     crates/ui crates/engine crates/proto crates/doc crates/rpc apps/comet apps/ios/Comet --include='*.rs' --include='*.swift' >/dev/null; then
  fail "plan-file paths / plan-exit tool names / mode ids found outside crates/harness"
else
  ok "no plan-file or plan-tool knowledge outside crates/harness"
fi

# comet never synthesizes a plan or an implement prompt: no such prompt text
# outside the harness crate (adapters may forward the CLI's own wording).
if grep -rniE '"[^"]*(implement the plan|implement this plan)[^"]*"' \
     crates/ui crates/engine crates/proto crates/doc crates/rpc apps/comet apps/ios/Comet --include='*.rs' --include='*.swift' >/dev/null; then
  fail "a synthesized plan/implement prompt lives outside crates/harness"
else
  ok "no synthesized plan prompts outside crates/harness"
fi

# Phone: the plan card and chip know no harness ids.
for f in apps/ios/Comet/Transcript/PlanCard.swift apps/ios/Comet/Composer/PlanModeChip.swift; do
  if [[ -f "$f" ]]; then
    if grep -nE '"(claude-code|codex|cursor|grok|hermes|pi|opencode|mock)"' "$f" >/dev/null; then
      fail "$f hardcodes harness ids"
    else
      ok "$(basename "$f") is harness-agnostic"
    fi
  else
    fail "$f missing — the phone surface has not landed"
  fi
done

# ONE optimistic answered-gate latch per surface: the plan card's buttons and
# the composer's send answer the SAME request, so a private set on either side
# lets a send right after an Approve take the gate again as "keep planning".
if grep -nE 'answered_plan_gates: *(std::collections::)?HashSet' \
     crates/ui/src/transcript.rs crates/ui/src/composer.rs >/dev/null; then
  fail "the desktop plan card keeps a private answered-gate set (share AppState's)"
else
  ok "desktop shares one answered-gate latch"
fi
if grep -rn 'answeredPlanGates' apps/ios/Comet/Sync/SessionStore.swift >/dev/null; then
  ok "phone shares one answered-gate latch"
else
  fail "apps/ios/Comet/Sync/SessionStore.swift lacks the shared answered-gate latch"
fi

# ONE path treatment per surface (§11.6): the plan card's file row and the
# diff file header are the same object, so they must not drift apart again.
if grep -q 'file_path::path_line' crates/ui/src/changes.rs crates/ui/src/transcript.rs; then
  ok "desktop shares one file-path treatment"
else
  fail "the plan card / diff header no longer share ui::file_path::path_line"
fi
if grep -rq 'planPathDisplay' apps/ios/Comet; then
  ok "phone shares one file-path treatment"
else
  fail "apps/ios/Comet lacks the shared plan-path helper"
fi

# The plan file is a STRING the harness handed us: the UI may render it, never
# interpret it (§11.8). Guarded by the plan-path literal check above; this one
# keeps the shortener lexical — no cwd math, which would only buy `../../..`
# for the plan files that live outside the project.
if grep -nE 'strip_prefix\(&?cwd|relative_to|components\(\)' crates/ui/src/file_path.rs >/dev/null; then
  fail "crates/ui/src/file_path.rs relativizes paths (must stay lexical)"
else
  ok "file-path shortener stays lexical"
fi

# `/plan` is the one composer-owned slash command (§11.9): one resolver per
# surface, both harness-agnostic, and no adapter re-implements it.
if grep -q 'fn composer_builtin' crates/ui/src/composer.rs; then
  ok "desktop /plan resolver present"
else
  fail "crates/ui/src/composer.rs lacks composer_builtin (§11.9)"
fi
if grep -rq 'func composerBuiltin' apps/ios/Comet/Composer; then
  ok "phone /plan resolver present"
else
  fail "apps/ios/Comet/Composer lacks composerBuiltin (§11.9)"
fi
if grep -rnE 'name == "plan"|matches\("plan"\)' crates/harness/src --include='*.rs' >/dev/null; then
  fail "an adapter matches a /plan invocation (the composer owns /plan)"
else
  ok "no adapter-side /plan handling"
fi

# ---------------------------------------------------------------------------
# 5. Formatting + lints on the crates this feature touches.
# ---------------------------------------------------------------------------
step "rustfmt --check on files changed vs main"
CHANGED_RS="$( { git diff --name-only "${VERIFY_BASE:-main}" -- '*.rs'; \
                 git ls-files --others --exclude-standard -- '*.rs'; } | sort -u)"
cargo fmt --all -- --check >/tmp/verify-plan-fmt.log 2>&1 || true
FMT_OFFENDERS="$(grep -oE '^Diff in [^:]+' /tmp/verify-plan-fmt.log | sed "s#^Diff in ${ROOT}/##" | sort -u)"
FMT_HITS="$(comm -12 <(echo "$CHANGED_RS") <(echo "$FMT_OFFENDERS") | sed '/^$/d')"
if [[ -z "$FMT_HITS" ]]; then
  ok "fmt"
else
  fail "fmt — unformatted changed files: $(echo "$FMT_HITS" | tr '\n' ' ')"
fi

step "cargo clippy — no new diagnostics on lines added vs main"
if python3 scripts/clippy-new-warnings.py --base "${VERIFY_BASE:-main}" -- \
     -q -p comet-proto -p comet-doc -p comet-harness -p comet-engine -p comet-ui --tests; then
  ok "clippy (no new diagnostics)"
else
  fail "clippy (new diagnostics on added lines)"
fi

# ---------------------------------------------------------------------------
# 2. Shared substrate.
# ---------------------------------------------------------------------------
step "proto + doc: wire compat, plan part, fold, ledger"
if cargo test -q -p comet-proto -p comet-doc plan >/tmp/verify-plan-doc.log 2>&1; then
  ok "comet-proto/comet-doc plan tests"
else
  fail "comet-proto/comet-doc plan tests (see /tmp/verify-plan-doc.log)"
fi

step "engine: exit bridge, mode watch, status, reconcile"
if cargo test -q -p comet-engine --test plan_mode >/tmp/verify-plan-engine.log 2>&1; then
  ok "comet-engine plan_mode"
else
  fail "comet-engine plan_mode (see /tmp/verify-plan-engine.log)"
fi

# ---------------------------------------------------------------------------
# 3. Adapters against fixture CLIs.
# ---------------------------------------------------------------------------
step "adapter plan-mode parity (fixture CLIs)"
ADAPTER_FAIL=0
for suite in claude codex acp opencode cursor; do
  [[ -f "crates/harness/tests/$suite.rs" ]] || continue
  if ! cargo test -q -p comet-harness --test "$suite" plan >/tmp/verify-plan-$suite.log 2>&1; then
    ADAPTER_FAIL=1
    fail "comet-harness --test $suite plan (see /tmp/verify-plan-$suite.log)"
  fi
done
[[ $ADAPTER_FAIL -eq 0 ]] && ok "adapter suites"

# ---------------------------------------------------------------------------
# 4. UI.
# ---------------------------------------------------------------------------
step "desktop transcript/composer plan unit tests"
if cargo test -q -p comet-ui --lib plan >/tmp/verify-plan-ui.log 2>&1; then
  ok "comet-ui plan"
else
  fail "comet-ui plan (see /tmp/verify-plan-ui.log)"
fi

if [[ $LIVE -eq 1 ]]; then
  step "live plan mode (installed CLIs only)"
  for suite in claude codex acp opencode cursor; do
    [[ -f "crates/harness/tests/$suite.rs" ]] || continue
    if cargo test -q -p comet-harness --test "$suite" -- --ignored live_plan >/tmp/verify-plan-live-$suite.log 2>&1; then
      ok "live $suite"
    else
      fail "live $suite (see /tmp/verify-plan-live-$suite.log)"
    fi
  done
fi

if [[ $IOS -eq 1 ]]; then
  step "phone plan unit tests"
  IOS_DEVICE="${VERIFY_IOS_DEVICE:-$(xcrun simctl list devices available 2>/dev/null \
    | grep -m1 -oE 'iPhone [^(]+' | sed 's/ *$//')}"
  if [[ -z "$IOS_DEVICE" ]]; then
    fail "no available iPhone simulator (set VERIFY_IOS_DEVICE)"
  elif (cd apps/ios && xcodebuild -quiet -project Comet.xcodeproj -scheme Comet \
        -destination "platform=iOS Simulator,name=$IOS_DEVICE" \
        -only-testing:CometTests/PlanModeTests test >/tmp/verify-plan-ios.log 2>&1); then
    ok "ios PlanModeTests"
  else
    fail "ios PlanModeTests (see /tmp/verify-plan-ios.log)"
  fi
fi

echo
if [[ $FAILED -eq 0 ]]; then
  echo "verify-plan-mode: all checks passed"
else
  echo "verify-plan-mode: FAILED" >&2
  exit 1
fi
