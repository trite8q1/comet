#!/usr/bin/env bash
# Verification loop for Agent Skills as slash commands (ARCHITECTURE.md §10.7).
#
# Proves, offline and deterministically:
#   1. correct catalog per harness      (adapter tests against fixture CLIs)
#   2. no cross-harness leakage         (engine ListCommands isolation test,
#                                        adapter "unknown /name stays text" tests)
#   3. slash parity                     (adapter tests asserting the native wire
#                                        frame produced for `/name args`)
#   4. architecture + code quality      (fmt, clippy with no new diagnostics on
#                                        added lines, guard greps)
#
# Usage: scripts/verify-skills.sh [--live] [--ios]
#   --live  additionally runs the #[ignore] tests that probe the REAL agent CLIs
#           installed on this machine (no model turns; discovery only).
#   --ios   additionally runs the phone composer's unit tests (needs Xcode).

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
# 4a. Architecture guards — the invariants as executable checks.
# ---------------------------------------------------------------------------
step "architecture guards"

# Harness-specific branching belongs in adapters only. The slash paths of the
# desktop composer, the engine RPC layer, and the phone composer must not
# match on harness identity.
if grep -n 'HarnessId::' crates/ui/src/composer.rs >/dev/null; then
  fail "crates/ui/src/composer.rs matches on HarnessId (slash routing must stay harness-agnostic)"
else
  ok "desktop composer is harness-agnostic"
fi

# rpc.rs: production code (everything before the test module) must not match
# on HarnessId variants — ListCommands resolves one registry slot by id.
if awk '/#\[cfg\(test\)\]/{exit} /HarnessId::/{found=1} END{exit !found}' crates/engine/src/rpc.rs; then
  fail "crates/engine/src/rpc.rs matches on HarnessId outside tests"
else
  ok "engine rpc layer is harness-agnostic"
fi

# Phone: the completion rules live in their own files and know no harness
# names (docs/composer-completions.md §1). The views may still carry the
# model picker's default harness; the completion path never does.
IOS_RULE_FILES=""
for name in SlashCommands.swift FileSearch.swift FileMentions.swift MentionDraft.swift; do
  if [[ ! -f "apps/ios/Comet/Composer/$name" ]]; then
    fail "apps/ios/Comet/Composer/$name missing — the phone surface has not landed"
  else
    IOS_RULE_FILES="$IOS_RULE_FILES apps/ios/Comet/Composer/$name"
  fi
done
IOS_HARNESS_HITS="$(grep -lE '"(claude-code|codex|cursor|grok|hermes|pi|opencode|mock)"' \
  $IOS_RULE_FILES || true)"
if [[ -n "$IOS_HARNESS_HITS" ]]; then
  fail "phone composer hardcodes harness ids: $(echo "$IOS_HARNESS_HITS" | tr '\n' ' ')"
else
  ok "phone composer is harness-agnostic"
fi

# The mention link scheme is written down once per surface (§6): the desktop
# composer's rules and the phone's. Spellings inside a test module are
# vectors, not definitions.
RUST_MENTION_FILES=""
for f in $(find crates/ui/src -name '*.rs' | sort); do
  if awk '/^#\[cfg\(test\)\]/{exit} /comet-file:/{found=1} END{exit !found}' "$f"; then
    RUST_MENTION_FILES="$RUST_MENTION_FILES $f"
  fi
done
SWIFT_MENTION_FILES="$(grep -rl 'comet-file:' apps/ios/Comet --include='*.swift' || true)"
RUST_MENTION="$(echo $RUST_MENTION_FILES | wc -w | tr -d ' ')"
SWIFT_MENTION="$(echo $SWIFT_MENTION_FILES | wc -w | tr -d ' ')"
if [[ "$RUST_MENTION" == 1 && "$SWIFT_MENTION" == 1 ]]; then
  ok "comet-file: link format defined once per surface"
else
  fail "comet-file: defined in$RUST_MENTION_FILES (expected crates/ui/src/composer.rs) and $SWIFT_MENTION_FILES (expected apps/ios/Comet/Composer/FileMentions.swift)"
fi

# comet never parses SKILL.md itself outside the harness crate (and there only
# for a harness whose wire has no listing).
if grep -rn 'SKILL\.md' crates/ui crates/engine crates/proto crates/rpc apps/ios/Comet --include='*.rs' --include='*.swift' >/dev/null; then
  fail "SKILL.md handling found outside crates/harness"
else
  ok "no SKILL.md parsing outside crates/harness"
fi

# One invocation grammar: adapters split `/name args` through the shared
# helper, never with their own strip_prefix('/') copies.
if grep -rn "strip_prefix('/')" crates/harness/src --include='*.rs' | grep -v 'src/commands.rs' >/dev/null; then
  fail "an adapter re-implements the invocation grammar (use comet_harness::commands)"
else
  ok "single invocation grammar"
fi

# ---------------------------------------------------------------------------
# 4b. Formatting + lints on the crates this feature touches.
# ---------------------------------------------------------------------------
step "rustfmt --check on files changed vs main"
# The workspace is not fmt-clean at HEAD; the gate is "every file this branch
# touches is formatted", so unrelated files are never reformatted.
CHANGED_RS="$( { git diff --name-only "${VERIFY_BASE:-main}" -- '*.rs'; \
                 git ls-files --others --exclude-standard -- '*.rs'; } | sort -u)"
# `cargo fmt --check` reports every unformatted file (it follows `mod`
# declarations into untouched children); only diffs in changed files count.
cargo fmt --all -- --check >/tmp/verify-skills-fmt.log 2>&1 || true
FMT_OFFENDERS="$(grep -oE '^Diff in [^:]+' /tmp/verify-skills-fmt.log | sed "s#^Diff in ${ROOT}/##" | sort -u)"
FMT_HITS="$(comm -12 <(echo "$CHANGED_RS") <(echo "$FMT_OFFENDERS") | sed '/^$/d')"
if [[ -z "$FMT_HITS" ]]; then
  ok "fmt"
else
  fail "fmt — unformatted changed files: $(echo "$FMT_HITS" | tr '\n' ' ')"
fi

step "cargo clippy — no new diagnostics on lines added vs main (proto, harness, engine)"
# The workspace carries pre-existing clippy warnings outside this feature;
# the gate is "this branch adds none" (scripts/clippy-new-warnings.py).
if python3 scripts/clippy-new-warnings.py --base "${VERIFY_BASE:-main}" -- \
     -q -p comet-proto -p comet-harness -p comet-engine --tests; then
  ok "clippy (no new diagnostics)"
else
  fail "clippy (new diagnostics on added lines)"
fi

# ---------------------------------------------------------------------------
# 1–3. Deterministic tests.
# ---------------------------------------------------------------------------
step "shared invocation grammar"
if cargo test -q -p comet-harness --lib commands:: >/dev/null; then ok "commands::"; else fail "commands::"; fi

step "adapter catalogs + parity (fixture CLIs)"
# Every adapter's command/skill tests, by name convention: `commands`, `skill`,
# `slash`, `invocation`, `parity` in the test name.
ADAPTER_FAIL=0
for suite in claude codex acp opencode cursor; do
  if [[ ! -f "crates/harness/tests/$suite.rs" ]]; then
    continue
  fi
  for pattern in commands skill slash invocation parity; do
    if ! cargo test -q -p comet-harness --test "$suite" "$pattern" >/tmp/verify-skills-$suite-$pattern.log 2>&1; then
      ADAPTER_FAIL=1
      fail "comet-harness --test $suite $pattern (see /tmp/verify-skills-$suite-$pattern.log)"
    fi
  done
done
[[ $ADAPTER_FAIL -eq 0 ]] && ok "adapter suites"

step "engine ListCommands isolation"
if cargo test -q -p comet-engine --test skills_isolation >/dev/null; then ok "skills_isolation"; else fail "skills_isolation"; fi

step "desktop composer slash unit tests"
if cargo test -q -p comet-ui --lib slash >/tmp/verify-skills-ui.log 2>&1; then
  ok "comet-ui slash"
else
  fail "comet-ui slash (see /tmp/verify-skills-ui.log)"
fi

# ---------------------------------------------------------------------------
# Optional: live discovery against the real CLIs installed here.
# ---------------------------------------------------------------------------
if [[ $LIVE -eq 1 ]]; then
  step "live discovery (installed CLIs only)"
  for suite in claude codex acp opencode cursor; do
    [[ -f "crates/harness/tests/$suite.rs" ]] || continue
    if cargo test -q -p comet-harness --test "$suite" -- --ignored live_commands >/tmp/verify-skills-live-$suite.log 2>&1; then
      ok "live $suite"
    else
      fail "live $suite (see /tmp/verify-skills-live-$suite.log)"
    fi
  done
fi

if [[ $IOS -eq 1 ]]; then
  step "phone composer unit tests"
  # First available iPhone simulator unless VERIFY_IOS_DEVICE names one.
  IOS_DEVICE="${VERIFY_IOS_DEVICE:-$(xcrun simctl list devices available 2>/dev/null \
    | grep -m1 -oE 'iPhone [^(]+' | sed 's/ *$//')}"
  if [[ -z "$IOS_DEVICE" ]]; then
    fail "no available iPhone simulator (set VERIFY_IOS_DEVICE)"
  elif (cd apps/ios && xcodebuild -quiet -project Comet.xcodeproj -scheme Comet \
        -destination "platform=iOS Simulator,name=$IOS_DEVICE" \
        -only-testing:CometTests/SlashCommandsTests \
        -only-testing:CometTests/FileMentionsTests \
        -only-testing:CometTests/MentionDraftTests test >/tmp/verify-skills-ios.log 2>&1); then
    ok "ios composer tests"
  else
    fail "ios composer tests (see /tmp/verify-skills-ios.log)"
  fi
fi

echo
if [[ $FAILED -eq 0 ]]; then
  echo "verify-skills: all checks passed"
else
  echo "verify-skills: FAILED" >&2
  exit 1
fi
