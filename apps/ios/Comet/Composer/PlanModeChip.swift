// Composer plan-mode toggle (ARCHITECTURE.md §11.6) — the phone's twin of the
// desktop composer's chip. It carries one bit, `ChatConfig.plan_mode`: the
// mode the user ASKED for, which the host reconciles to whatever the harness
// reports.
//
// Harness-agnostic by construction (§11.8 guard): visibility is decided by the
// catalog row's `planMode`, never by a harness id.

import SwiftUI

/// Whether the composer offers the toggle at all: the resolved harness's
/// descriptor says its CLI has a plan mode comet can drive.
func planModeAvailable(harness: String, catalog: [HarnessInfo]) -> Bool {
    catalog.first { $0.id == harness }?.planMode ?? false
}

/// What a send does. While the harness's plan-exit gate is parked, the
/// composer's send is the "keep planning" answer carrying the typed feedback
/// (§11.4 step 4) — never a run and never a steer. It reads the same on every
/// harness: the engine delivers the feedback the way that CLI does it (a
/// mid-turn message on a step-boundary steerer, cancel-then-next-message on a
/// turn-boundary agent).
enum ComposerSendAction: Equatable {
    case planFeedback(requestId: String)
    case message
}

func composerSendAction(pendingPlanExit requestId: String?, prompt: String) -> ComposerSendAction {
    guard let requestId, !prompt.isEmpty else { return .message }
    return .planFeedback(requestId: requestId)
}

/// The composer's placeholder while a gate is parked — typing IS the feedback.
let planFeedbackPlaceholder = "Describe what to change in the plan…"

// MARK: - `/plan`, the one composer-owned slash command (§11.9)

/// `/plan`'s popup row, with comet's own description and hint. It is listed
/// FIRST and it SHADOWS a catalog entry of the same name (Claude and Grok both
/// list `plan`, and on both the CLI's own command is TUI-local): one behavior,
/// one report path, on every harness.
let planSlashCommand = SlashCommand(name: "plan", description: "Enter plan mode",
                                    inputHint: "[description]")

/// A command the composer answers ITSELF instead of sending as prompt text —
/// the deliberate exception to §10's "comet invents no commands", because the
/// command IS the chip (§11.9).
enum ComposerBuiltin: Equatable {
    /// `/plan` enters plan mode for the chat; `/plan <description>` enters it
    /// and sends the description as the prompt. There is no leaving: that is
    /// the chip, exactly as no CLI has a `/plan off`.
    case plan(description: String?)
}

/// `composer_builtin`'s phone twin: the invocation this prompt makes, when the
/// composer owns it. One grammar with `slashToken` and the host's
/// `split_invocation` — leading `/`, name to the first whitespace, arguments
/// trimmed — and an exact name match, so `/plans …` is never it.
///
/// `planOffered` is `planModeAvailable` for the resolved harness: where the
/// descriptor has no plan mode, `/plan …` stays ordinary prompt text and the
/// harness reacts to it as its CLI would (§10.5). No harness is named here.
func composerBuiltin(text: String, planOffered: Bool) -> ComposerBuiltin? {
    guard planOffered else { return nil }
    let chars = Array(text)
    guard chars.first == "/" else { return nil }
    let end = chars.firstIndex(where: \.isWhitespace) ?? chars.count
    guard String(chars[1..<end]) == planSlashCommand.name else { return nil }
    let arguments = String(chars[end...]).trimmingCharacters(in: .whitespacesAndNewlines)
    return .plan(description: arguments.isEmpty ? nil : arguments)
}

/// `ComposerChip`'s toggle sibling: no picker, no chevron; filled with the
/// accent while plan mode is on.
struct PlanModeChip: View {
    let active: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 6) {
                Image(systemName: "list.bullet.rectangle")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(active ? Theme.bg : Theme.textFaint)
                Text("Plan")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(active ? Theme.bg : Theme.text.opacity(0.9))
                    .lineLimit(1)
            }
            .padding(.horizontal, 13)
            .frame(height: 40)
            .background(active ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(whiteAlpha(0.08)),
                        in: Capsule())
            .overlay(Capsule().strokeBorder(active ? .clear : whiteAlpha(0.08), lineWidth: 1))
        }
        .buttonStyle(ChipPressButtonStyle())
        .accessibilityLabel("Plan mode")
        .accessibilityValue(active ? "On" : "Off")
    }
}
