// Native plan mode, phone half (ARCHITECTURE.md §11.8 step 5): the doc decode
// of the `plan` part, the transcript row it builds, and the two pure decisions
// the composer makes — when to offer the toggle, and when a send is feedback
// on a parked plan-exit gate rather than a new turn.

import Loro
import UIKit
import XCTest
@testable import Comet

final class PlanModeTests: XCTestCase {
    // MARK: partFrom (through decodeEntries, its module seam)

    func testDecodesAPlanPartFromTheDoc() throws {
        let doc = LoroDoc()
        try doc.getList(id: "messages").push(v: LoroValue.fromJSON([
            "id": "m1",
            "role": "assistant",
            "createdAt": 1_700_000_000_000,
            "deviceId": "dev-mac",
            "status": "complete",
            "parts": [
                [
                    "id": "plan",
                    "kind": "plan",
                    "plan": "# Ship it\n\n1. step",
                    "planStatus": "awaitingApproval",
                    "requestId": "req-7",
                    "path": "/tmp/a.md",
                ],
            ],
        ]))
        doc.commit()

        let entries = try XCTUnwrap(SessionStore.decodeEntries(from: doc))
        XCTAssertEqual(entries.count, 1)
        guard case .plan(let id, let plan, let status, let requestId, let path) =
            try XCTUnwrap(entries.first?.parts.first) else {
            return XCTFail("expected a plan part")
        }
        XCTAssertEqual(id, "plan")
        XCTAssertEqual(plan, "# Ship it\n\n1. step")
        XCTAssertEqual(status, .awaitingApproval)
        XCTAssertEqual(requestId, "req-7")
        XCTAssertEqual(path, "/tmp/a.md")
    }

    /// The body never rides `text`, and an unknown status degrades to the
    /// lifecycle's first state rather than dropping the part.
    func testPlanStatusDefaultsToDraftingAndTheBodyIsNeverText() throws {
        let doc = LoroDoc()
        try doc.getList(id: "messages").push(v: LoroValue.fromJSON([
            "id": "m1", "role": "assistant", "createdAt": 1, "deviceId": "d",
            "parts": [["id": "plan", "kind": "plan", "plan": "# P",
                       "planStatus": "fromTheFuture"]],
        ]))
        doc.commit()

        let parts = try XCTUnwrap(SessionStore.decodeEntries(from: doc)?.first?.parts)
        guard case .plan(_, let plan, let status, let requestId, _) = try XCTUnwrap(parts.first) else {
            return XCTFail("expected a plan part")
        }
        XCTAssertEqual(plan, "# P")
        XCTAssertEqual(status, .drafting)
        XCTAssertNil(requestId)
    }

    // MARK: Row building

    func testBuildsAPlanCardRowWithTitleAndStatus() {
        let rows = rowsFor(plan: "# Harden the deploy path\n\nStep one.",
                           status: .awaitingApproval, requestId: "req-7")
        XCTAssertEqual(rows.count, 1)
        let row = rows[0]
        XCTAssertEqual(row.id, "m1#plan")
        guard case .planCard(let blocks, let title, let status, let requestId, _) = row.kind else {
            return XCTFail("expected a plan card row")
        }
        XCTAssertEqual(title, "Harden the deploy path")
        XCTAssertEqual(status, .awaitingApproval)
        XCTAssertEqual(requestId, "req-7")
        // The body parses like assistant prose: heading + paragraph.
        XCTAssertEqual(blocks.count, 2)
    }

    func testAPlanWithoutAHeadingFallsBackToPlan() {
        XCTAssertEqual(TranscriptRowBuilder.planTitle("Just prose.\n\n## Not h1"), "Plan")
        XCTAssertEqual(TranscriptRowBuilder.planTitle("## Sub\n\n# Real title\n"), "Real title")
    }

    /// The part is refreshed IN PLACE, so the row id never moves — the version
    /// is the only thing that can tell SwiftUI a draft changed.
    func testVersionChangesAcrossDraftsAndStatusFlips() {
        let draft = rowsFor(plan: "# Plan\n\none", status: .drafting, requestId: nil)[0]
        let longer = rowsFor(plan: "# Plan\n\none two", status: .drafting, requestId: nil)[0]
        let parked = rowsFor(plan: "# Plan\n\none two", status: .awaitingApproval,
                             requestId: "req-7")[0]
        let approved = rowsFor(plan: "# Plan\n\none two", status: .approved, requestId: "req-7")[0]

        XCTAssertEqual(draft.id, longer.id)
        XCTAssertEqual(longer.id, parked.id)
        XCTAssertNotEqual(draft.version, longer.version)
        XCTAssertNotEqual(longer.version, parked.version)
        XCTAssertNotEqual(parked.version, approved.version)
    }

    /// A redraft that keeps the plan's LENGTH (a reordered step, a swapped
    /// word) still has to re-render: the version hashes the body, not `count`.
    func testASameLengthRedraftStillMovesTheVersion() {
        let before = "# Plan\n\n1. read\n2. write\n"
        let after = "# Plan\n\n1. scan\n2. write\n"
        XCTAssertEqual(before.count, after.count, "the point of the test")
        XCTAssertNotEqual(
            TranscriptRowBuilder.planVersion(plan: before, status: .drafting, requestId: nil,
                                             path: nil),
            TranscriptRowBuilder.planVersion(plan: after, status: .drafting, requestId: nil,
                                             path: nil))
    }

    /// The gate on the LAST assistant entry is the one the composer serves
    /// (the desktop rule): a newer assistant entry supersedes an unanswered
    /// card, so the send is never bound to a stale request id.
    func testANewerAssistantEntrySupersedesAnUnansweredGate() {
        let gate = MessageEntry(id: "m1", role: .assistant, parts: [
            .plan(id: "plan", plan: "# P", status: .awaitingApproval,
                  requestId: "req-7", path: nil),
        ], createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil)
        XCTAssertEqual(SessionStore.pendingPlanExit(in: [gate])?.requestId, "req-7")
        let newer = MessageEntry(id: "m2", role: .assistant, parts: [
            .text(id: "t", text: "moving on"),
        ], createdAt: 2, deviceId: "d", status: .complete, continuationOf: nil)
        XCTAssertNil(SessionStore.pendingPlanExit(in: [gate, newer]))
        // A user message in between is not a new assistant turn.
        let typed = MessageEntry(id: "u1", role: .user, parts: [
            .text(id: "t", text: "hi"),
        ], createdAt: 2, deviceId: "d", status: .complete, continuationOf: nil)
        XCTAssertEqual(SessionStore.pendingPlanExit(in: [gate, typed])?.requestId, "req-7")
    }

    // MARK: Composer decisions (pure)

    func testTheToggleIsOfferedOnlyWhereTheDescriptorHasAPlanMode() {
        let catalog = [
            HarnessInfo(id: "with-plan", label: "With plan", planMode: true),
            HarnessInfo(id: "without-plan", label: "Without plan", planMode: false),
            HarnessInfo(id: "old-engine", label: "Old engine"),  // field absent ⇒ false
        ]
        XCTAssertTrue(planModeAvailable(harness: "with-plan", catalog: catalog))
        XCTAssertFalse(planModeAvailable(harness: "without-plan", catalog: catalog))
        XCTAssertFalse(planModeAvailable(harness: "old-engine", catalog: catalog))
        // A harness the catalog doesn't list can't be assumed to have one.
        XCTAssertFalse(planModeAvailable(harness: "unlisted", catalog: catalog))
        XCTAssertFalse(planModeAvailable(harness: "with-plan", catalog: []))
    }

    func testAParkedGateTurnsTheSendIntoPlanFeedback() {
        let entries = [
            MessageEntry(id: "m1", role: .assistant, parts: [
                .plan(id: "plan", plan: "# P", status: .awaitingApproval,
                      requestId: "req-7", path: nil),
            ], createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil),
        ]
        let pending = SessionStore.pendingPlanExit(in: entries)
        XCTAssertEqual(pending?.requestId, "req-7")
        XCTAssertEqual(pending?.entryId, "m1")
        XCTAssertEqual(composerSendAction(pendingPlanExit: pending?.requestId,
                                          prompt: "smaller steps please"),
                       .planFeedback(requestId: "req-7"))
        // An empty draft can't answer the gate — it stays parked.
        XCTAssertEqual(composerSendAction(pendingPlanExit: pending?.requestId, prompt: ""),
                       .message)
    }

    // MARK: `/plan` — the one composer-owned slash command (§11.9)

    func testResolvesSlashPlanAndOnlySlashPlan() {
        // Bare `/plan` enters the mode and sends nothing.
        XCTAssertEqual(composerBuiltin(text: "/plan", planOffered: true), .plan(description: nil))
        // Trailing whitespace is not a description.
        XCTAssertEqual(composerBuiltin(text: "/plan   ", planOffered: true),
                       .plan(description: nil))
        // Arguments are trimmed, exactly like `split_invocation`.
        XCTAssertEqual(composerBuiltin(text: "/plan  add a readme", planOffered: true),
                       .plan(description: "add a readme"))
        XCTAssertEqual(composerBuiltin(text: "/plan\nadd a readme", planOffered: true),
                       .plan(description: "add a readme"))
        // Exact name only: a longer name is the harness's command, not ours.
        XCTAssertNil(composerBuiltin(text: "/plans", planOffered: true))
        XCTAssertNil(composerBuiltin(text: "/planning the work", planOffered: true))
        XCTAssertNil(composerBuiltin(text: "/", planOffered: true))
        // Not a leading invocation at all.
        XCTAssertNil(composerBuiltin(text: "plan the work", planOffered: true))
        XCTAssertNil(composerBuiltin(text: " /plan", planOffered: true))
        XCTAssertNil(composerBuiltin(text: "run /plan", planOffered: true))
        // Where the descriptor has no plan mode it is ordinary prompt text.
        XCTAssertNil(composerBuiltin(text: "/plan", planOffered: false))
        XCTAssertNil(composerBuiltin(text: "/plan add a readme", planOffered: false))
    }

    func testPlanRowIsPrependedAndShadowsTheCatalogsOwn() {
        let catalog = [
            SlashCommand(name: "compact", description: "Compact the thread"),
            SlashCommand(name: "plan", description: "Enable plan mode or view the plan",
                         inputHint: "[open|share|<description>]"),
            SlashCommand(name: "planner", description: "A skill of the same family"),
            SlashCommand(name: "todo", description: "Aliased", aliases: ["plan"]),
        ]
        let rows = slashRowsWithBuiltins(catalog: catalog, planOffered: true)
        // Listed first, with comet's own description and hint.
        XCTAssertEqual(rows.first, planSlashCommand)
        XCTAssertEqual(rows.first?.description, "Enter plan mode")
        XCTAssertEqual(rows.first?.inputHint, "[description]")
        // The harness's own `plan` — by name or by alias — is shadowed; a
        // merely similar name is not.
        XCTAssertEqual(rows.map(\.name), ["plan", "compact", "planner"])
        // Untouched where the harness has no plan mode.
        XCTAssertEqual(slashRowsWithBuiltins(catalog: catalog, planOffered: false), catalog)
        XCTAssertEqual(slashRowsWithBuiltins(catalog: [], planOffered: true), [planSlashCommand])
        XCTAssertEqual(slashRowsWithBuiltins(catalog: [], planOffered: false), [])
    }

    /// The merge happens where the popup's rows are produced, so the typed
    /// prefix filters `/plan` like any other row and accept rewrites the token.
    func testThePopupFiltersAndAcceptsThePlanRow() {
        let key = SlashCatalogKey(deviceId: "dev-a", harness: "harness-one", cwd: "/work/repo")
        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: key, planOffered: true), key)
        // The composer's own row shows before the probe answers.
        XCTAssertEqual(model.popup, .commands([planSlashCommand]))
        model.received([SlashCommand(name: "compact"), SlashCommand(name: "plan")], for: key)
        XCTAssertEqual(model.popup, .commands([planSlashCommand, SlashCommand(name: "compact")]))
        _ = model.update(text: "/pl", cursor: 3, key: key, planOffered: true)
        XCTAssertEqual(model.popup, .commands([planSlashCommand]))
        let accepted = model.accept(planSlashCommand, in: "/pl")
        XCTAssertEqual(accepted?.text, "/plan ")
        XCTAssertEqual(accepted?.cursor, 6)
    }

    /// A parked gate owns the send on every harness — the composer resolves
    /// `/plan` only after that decision, so feedback that happens to read as
    /// an invocation still answers the gate.
    func testAParkedGateStillWinsOverSlashPlan() {
        let entries = [
            MessageEntry(id: "m1", role: .assistant, parts: [
                .plan(id: "plan", plan: "# P", status: .awaitingApproval,
                      requestId: "req-7", path: nil),
            ], createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil),
        ]
        let pending = SessionStore.pendingPlanExit(in: entries)
        XCTAssertEqual(composerSendAction(pendingPlanExit: pending?.requestId, prompt: "/plan"),
                       .planFeedback(requestId: "req-7"))
        XCTAssertEqual(composerSendAction(pendingPlanExit: pending?.requestId,
                                          prompt: "/plan smaller steps"),
                       .planFeedback(requestId: "req-7"))
        // With no gate parked the same text is the builtin's to answer.
        XCTAssertEqual(composerSendAction(pendingPlanExit: nil, prompt: "/plan"), .message)
        XCTAssertEqual(composerBuiltin(text: "/plan", planOffered: true), .plan(description: nil))
    }

    // MARK: Question panel (pure)

    func testTheLastPageNeverAutoSubmits() {
        // Between pages a single-select pick advances itself…
        XCTAssertTrue(questionPanelAutoAdvances(page: 0, count: 3, multiSelect: false))
        XCTAssertTrue(questionPanelAutoAdvances(page: 1, count: 3, multiSelect: false))
        // …but the last page's Submit is the user's own tap.
        XCTAssertFalse(questionPanelAutoAdvances(page: 2, count: 3, multiSelect: false))
        XCTAssertFalse(questionPanelAutoAdvances(page: 0, count: 1, multiSelect: false))
        // Multi-select picks are toggles — never an advance.
        XCTAssertFalse(questionPanelAutoAdvances(page: 0, count: 3, multiSelect: true))
    }

    func testAnAnsweredOrDraftingPlanLeavesTheSendAlone() {
        for status: PlanStatus in [.drafting, .approved, .revising, .rejected] {
            let entries = [
                MessageEntry(id: "m1", role: .assistant, parts: [
                    .plan(id: "plan", plan: "# P", status: status,
                          requestId: "req-7", path: nil),
                ], createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil),
            ]
            XCTAssertNil(SessionStore.pendingPlanExit(in: entries), "\(status)")
        }
        XCTAssertEqual(composerSendAction(pendingPlanExit: nil, prompt: "hello"), .message)
    }

    // MARK: Reject — the third answer (§11.4)

    /// The fifth status is a doc string like the other four, and the one an
    /// older device cannot know still degrades to `drafting` rather than
    /// dropping the part.
    func testARejectedPlanDecodesAndAnUnknownStatusStillDegrades() throws {
        XCTAssertEqual(PlanStatus(rawValue: "rejected"), .rejected)
        XCTAssertEqual(PlanStatus.rejected.rawValue, "rejected")
        XCTAssertEqual(PlanStatus.rejected.label, "Rejected")

        let doc = LoroDoc()
        try doc.getList(id: "messages").push(v: LoroValue.fromJSON([
            "id": "m1", "role": "assistant", "createdAt": 1, "deviceId": "d",
            "parts": [["id": "plan", "kind": "plan", "plan": "# P",
                       "planStatus": "rejected", "requestId": "req-7"]],
        ]))
        doc.commit()
        let parts = try XCTUnwrap(SessionStore.decodeEntries(from: doc)?.first?.parts)
        guard case .plan(_, _, let status, _, _) = try XCTUnwrap(parts.first) else {
            return XCTFail("expected a plan part")
        }
        XCTAssertEqual(status, .rejected)
    }

    /// A rejected plan is history, exactly like an approved one: the card
    /// folds by default and the fold is the only thing the view owns.
    func testARejectedPlanCardCollapsesByDefault() {
        XCTAssertTrue(planOpensByDefault(.drafting))
        XCTAssertTrue(planOpensByDefault(.awaitingApproval))
        XCTAssertTrue(planOpensByDefault(.revising))
        XCTAssertFalse(planOpensByDefault(.approved))
        XCTAssertFalse(planOpensByDefault(.rejected))
    }

    /// Approve / Keep planning / Reject stay on ONE row on the narrowest
    /// phone comet ships to (375pt), inside the transcript's 16pt gutters and
    /// the card's 10pt body padding. If a label or the padding grows, this is
    /// the test that says so before the row wraps on a device.
    func testTheThreePlanAnswersFitOneRowOnA375ptPhone() {
        let font = Theme.sansUI(13, weight: .medium)
        let padding = PlanCardView.answerHPadding
        let buttons = ["Approve", "Keep planning", "Reject"].reduce(CGFloat(0)) {
            $0 + ceil($1.size(withAttributes: [.font: font]).width) + 2 * padding
        }
        let available = CGFloat(375) - 2 * 16 - 2 * 10
        XCTAssertLessThanOrEqual(buttons + 2 * 8, available, "the answers would wrap")
    }

    // MARK: The plan file on the card (§11.6)

    /// Claude learns the plan file from its own gate: the path lands on a
    /// LATER `PlanUpdated` whose text is byte-identical to the one before it.
    /// Without the path in the version the card would keep a stale (empty)
    /// path row through that update.
    func testPlanVersionMovesWhenOnlyThePathChanges() {
        let plan = "# Plan\n\n1. step\n"
        let before = TranscriptRowBuilder.planVersion(plan: plan, status: .awaitingApproval,
                                                     requestId: "req-7", path: nil)
        let landed = TranscriptRowBuilder.planVersion(plan: plan, status: .awaitingApproval,
                                                     requestId: "req-7", path: "/tmp/x/notes/a.md")
        let moved = TranscriptRowBuilder.planVersion(plan: plan, status: .awaitingApproval,
                                                     requestId: "req-7", path: "/tmp/x/notes/b.md")
        XCTAssertNotEqual(before, landed)
        XCTAssertNotEqual(landed, moved)
        // And it reaches the row the card reads.
        let row = rowsFor(plan: plan, status: .awaitingApproval, requestId: "req-7",
                          path: "/tmp/x/notes/a.md")[0]
        guard case .planCard(_, _, _, _, let path) = row.kind else {
            return XCTFail("expected a plan card row")
        }
        XCTAssertEqual(path, "/tmp/x/notes/a.md")
    }

    /// The card's path line, semantics for semantics with the desktop's
    /// `file_path::home_relative` (its unit tests pin the same cases).
    func testThePlanPathIsShownHomeRelative() {
        let home = "/Users/nico"
        XCTAssertEqual(planPathDisplay("/Users/nico/notes/a.md", home: home), "~/notes/a.md")
        XCTAssertEqual(planPathDisplay("/Users/nico", home: home), "~")
        // Idempotent: an already-shortened path is handed straight back.
        XCTAssertEqual(planPathDisplay("~/notes/a.md", home: home), "~/notes/a.md")
        XCTAssertEqual(planPathDisplay("~", home: home), "~")
        // Outside HOME, and a sibling that only shares the prefix.
        XCTAssertEqual(planPathDisplay("/tmp/x/a.md", home: home), "/tmp/x/a.md")
        XCTAssertEqual(planPathDisplay("/Users/nicolas/a.md", home: home), "/Users/nicolas/a.md")
        // A trailing-slash HOME must not leave a doubled separator.
        XCTAssertEqual(planPathDisplay("/Users/nico/notes/a.md", home: "/Users/nico/"),
                       "~/notes/a.md")
        // No home to speak of shortens nothing.
        XCTAssertEqual(planPathDisplay("/Users/nico/a.md", home: ""), "/Users/nico/a.md")
        // Nothing is percent-decoded: the encoded segment IS the name on disk.
        XCTAssertEqual(planPathDisplay("/Users/nico/.state/%2FUsers%2Fnico%2Frepo/a.md",
                                       home: home),
                       "~/.state/%2FUsers%2Fnico%2Frepo/a.md")
    }

    /// Truncation may eat the directory; it may never eat the filename, so the
    /// two halves are split before they reach the view.
    func testThePlanPathSplitKeepsTheBasenameWhole() {
        var split = splitDisplayPath("/tmp/x/notes/a.md")
        XCTAssertEqual(split.directory, "/tmp/x/notes/")
        XCTAssertEqual(split.name, "a.md")
        // A bare filename has no directory half.
        split = splitDisplayPath("plan.md")
        XCTAssertEqual(split.directory, "")
        XCTAssertEqual(split.name, "plan.md")
        // A trailing slash is all directory — a folder renders as itself.
        split = splitDisplayPath("/tmp/x/")
        XCTAssertEqual(split.directory, "/tmp/x/")
        XCTAssertEqual(split.name, "")
        // The shape that motivates the whole helper: everything identifying
        // the file is at the END of a 200-plus-character path.
        split = splitDisplayPath("/var/sessions/\(String(repeating: "e", count: 200))/notes.md")
        XCTAssertEqual(split.name, "notes.md")
        XCTAssertGreaterThan(split.directory.count, 200)
    }

    // MARK: Helpers

    private func rowsFor(plan: String, status: PlanStatus, requestId: String?,
                         path: String? = nil) -> [TranscriptRow] {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        let entries = [
            MessageEntry(id: "m1", role: .assistant, parts: [
                .plan(id: "plan", plan: plan, status: status, requestId: requestId, path: path),
            ], createdAt: 1, deviceId: "dev-mac", status: .complete, continuationOf: nil),
        ]
        return TranscriptRowBuilder.rows(entries: entries, pendingSends: [],
                                         parsers: &parsers, completed: &completed)
    }
}
