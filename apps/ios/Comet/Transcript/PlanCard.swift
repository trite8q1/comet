// Plan card (ARCHITECTURE.md §11.6) — the harness's own plan, rendered where
// the turn produced it. One card per plan-mode episode, refreshed in place:
// the doc's plan part IS the state, so this view owns nothing but its fold.
//
// Harness-agnostic by construction (§11.8 guard): the header's mark comes from
// the chat's configured harness through `BrandMark`, never from a language or
// file icon, and nothing here branches on a harness id.

import SwiftUI

/// The three answers the card can give a parked gate (§11.4), mirroring
/// `PlanDecision`'s three constructors: approve, keep planning, reject.
enum PlanAnswer {
    case approve
    case keepPlanning
    case reject
}

/// Whether the card opens by default (`transcript.rs plan_opens_by_default`):
/// a plan still being written, or waiting on the user, is the point of the
/// turn; an approved one is history — and so is a rejected one.
func planOpensByDefault(_ status: PlanStatus) -> Bool {
    status != .approved && status != .rejected
}

struct PlanCardView: View {
    /// The plan markdown, parsed like assistant prose (fenced code included).
    let blocks: [TopBlock]
    /// The plan's first `# ` heading, minus a repeated "Plan" genus — empty
    /// when that heading added nothing over the label beside it.
    let title: String
    let status: PlanStatus
    /// The parked gate, present only while `awaitingApproval`.
    let requestId: String?
    /// `ChatConfig.harness` — the mark in the header tile.
    let harness: String?
    /// Row id; seeds the markdown highlight cache.
    let cacheKey: String
    let open: Bool
    let toggle: () -> Void
    /// Approve / Keep planning / Reject — resolves `requestId`.
    let respond: (PlanAnswer) -> Void
    /// The store's answered-gate latch: the buttons go quiet the moment this
    /// gate is answered from this device — by these buttons OR by a composer
    /// send — and stay quiet until the doc flips the status.
    let answered: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if open {
                VStack(alignment: .leading, spacing: MD.blockGap) {
                    ForEach(Array(blocks.enumerated()), id: \.offset) { ix, top in
                        MarkdownBlockView(block: top.block, cacheKey: "\(cacheKey).\(ix)")
                    }
                    if status == .awaitingApproval, requestId != nil {
                        actions
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }
        }
        .background(whiteAlpha(0.03), in: RoundedRectangle(cornerRadius: 9))
        .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(whiteAlpha(0.05), lineWidth: 1))
    }

    private var header: some View {
        Button(action: toggle) {
            HStack(spacing: 8) {
                HarnessBadge(harness: harness ?? "", size: 11, neutral: Theme.textMuted)
                    .frame(width: 18, height: 18)
                    .background(whiteAlpha(0.08), in: RoundedRectangle(cornerRadius: 5))
                Text("Plan")
                    .font(Theme.sans(12, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize()
                // Empty when the heading was nothing but the genus: the
                // label carries the header alone rather than trailing an
                // 8pt gap for a slot with no name in it.
                if !title.isEmpty {
                    Text(title)
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.text.opacity(0.85))
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer(minLength: 8)
                statusPill
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .rotationEffect(.degrees(open ? 90 : 0))
            }
            .padding(.horizontal, 8)
            .frame(height: 34)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 9))
    }

    private var statusPill: some View {
        Text(status.label)
            .font(Theme.sans(10.5, weight: .medium))
            .foregroundStyle(statusTint.opacity(0.9))
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 6)
            .frame(height: 18)
            .background(statusTint.opacity(0.09), in: RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5)
                .strokeBorder(statusTint.opacity(0.13), lineWidth: 1))
    }

    /// Accent = "needs you", the same indigo the awaiting-input dot carries.
    private var statusTint: Color {
        switch status {
        case .drafting, .revising: return Theme.textMuted
        case .awaitingApproval: return Theme.accent
        case .approved: return Theme.statusCompleted
        case .rejected: return Theme.danger
        }
    }

    /// The pill padding all three answers share. A constant because the third
    /// button has to earn its place on one row: at 375pt the three labels plus
    /// this padding come to 278 of the 323pt the card's body has, and
    /// `PlanModeTests` pins that arithmetic so a longer label cannot wrap the
    /// row onto two lines unnoticed.
    static let answerHPadding: CGFloat = 14

    /// Approve is the primary (accent plate), Keep planning the quiet
    /// neutral, Reject the quietest of the three: danger-tinted TEXT on the
    /// faintest wash, never a red plate. Ending the turn is a real answer,
    /// not the one the card pushes you toward.
    private var actions: some View {
        HStack(spacing: 8) {
            Button { answer(.approve) } label: {
                Text("Approve")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.bg)
                    .padding(.horizontal, Self.answerHPadding)
                    .frame(height: 30)
                    .background(Theme.accent, in: Capsule())
            }
            .buttonStyle(ChipPressButtonStyle())
            Button { answer(.keepPlanning) } label: {
                Text("Keep planning")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, Self.answerHPadding)
                    .frame(height: 30)
                    .background(whiteAlpha(0.06), in: Capsule())
            }
            .buttonStyle(ChipPressButtonStyle())
            Button { answer(.reject) } label: {
                Text("Reject")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.danger.opacity(0.9))
                    .padding(.horizontal, Self.answerHPadding)
                    .frame(height: 30)
                    .background(whiteAlpha(0.03), in: Capsule())
            }
            .buttonStyle(ChipPressButtonStyle())
            Spacer(minLength: 0)
        }
        .opacity(answered ? 0.4 : 1)
        .disabled(answered)
    }

    private func answer(_ answer: PlanAnswer) {
        guard !answered else { return }
        respond(answer)
    }
}
