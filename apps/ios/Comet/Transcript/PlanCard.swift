// Plan card (ARCHITECTURE.md §11.6) — the harness's own plan, rendered where
// the turn produced it. One card per plan-mode episode, refreshed in place:
// the doc's plan part IS the state, so this view owns nothing but its fold.
//
// Harness-agnostic by construction (§11.8 guard): the header's mark comes from
// the chat's configured harness through `BrandMark`, never from a language or
// file icon, and nothing here branches on a harness id.

import SwiftUI

struct PlanCardView: View {
    /// The plan markdown, parsed like assistant prose (fenced code included).
    let blocks: [TopBlock]
    /// The plan's first `# ` heading, else "Plan".
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
    /// Approve (true) / Keep planning (false) — resolves `requestId`.
    let respond: (Bool) -> Void

    /// Local latch: the buttons go quiet the moment they're tapped and stay
    /// quiet until the doc flips the status out of `awaitingApproval`.
    @State private var answered = false

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
        .onChange(of: status) { answered = false }
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
                Text(title)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.text.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.tail)
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
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Button { answer(true) } label: {
                Text("Approve")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.bg)
                    .padding(.horizontal, 14)
                    .frame(height: 30)
                    .background(Theme.accent, in: Capsule())
            }
            .buttonStyle(ChipPressButtonStyle())
            Button { answer(false) } label: {
                Text("Keep planning")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 14)
                    .frame(height: 30)
                    .background(whiteAlpha(0.06), in: Capsule())
            }
            .buttonStyle(ChipPressButtonStyle())
            Spacer(minLength: 0)
        }
        .opacity(answered ? 0.4 : 1)
        .disabled(answered)
    }

    private func answer(_ approved: Bool) {
        guard !answered else { return }
        answered = true
        respond(approved)
    }
}
