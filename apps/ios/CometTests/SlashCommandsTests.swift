// Slash-command completion parity — the phone must behave exactly like
// crates/ui/src/composer.rs (token grammar, accept replacement, dismissal)
// and crates/ui/src/popover.rs (match_rank / filter_indices ranking), with
// one catalog per (device, harness, cwd) per ARCHITECTURE.md §10.4/§10.6.

import XCTest
@testable import Comet

private func command(_ name: String, _ description: String = "",
                     hint: String? = nil, aliases: [String] = []) -> SlashCommand {
    SlashCommand(name: name, description: description, inputHint: hint, aliases: aliases)
}

private let claudeKey = SlashCatalogKey(deviceId: "dev-a", harness: "harness-one",
                                        cwd: "/work/repo")
private let otherKey = SlashCatalogKey(deviceId: "dev-a", harness: "harness-two",
                                       cwd: "/work/repo")

// One class: `scripts/verify-skills.sh` runs the suite as
// `-only-testing:CometTests/SlashCommandsTests`.
final class SlashCommandsTests: XCTestCase {
    func testOpensOnlyOnALeadingSlashToken() {
        // Bare `/` opens with an empty query.
        XCTAssertEqual(slashToken("/", cursor: 1), SlashToken(range: 0..<1, query: ""))
        XCTAssertEqual(slashToken("/comp", cursor: 5), SlashToken(range: 0..<5, query: "comp"))
        // Caret inside the token of a command that already has arguments.
        XCTAssertEqual(slashToken("/compact now", cursor: 5),
                       SlashToken(range: 0..<8, query: "comp"))
        // Caret in the argument: closed.
        XCTAssertNil(slashToken("/compact now", cursor: 12))
        XCTAssertNil(slashToken("/compact now", cursor: 9))
        // Caret still inside the token at its end.
        XCTAssertEqual(slashToken("/compact now", cursor: 8),
                       SlashToken(range: 0..<8, query: "compact"))
        // Not the first character.
        XCTAssertNil(slashToken("run /compact", cursor: 12))
        XCTAssertNil(slashToken(" /compact", cursor: 5))
        // A typed path is never a command.
        XCTAssertNil(slashToken("/usr/bin", cursor: 8))
        XCTAssertNil(slashToken("/usr/bin", cursor: 5))
        // Caret before the slash, and out-of-range carets.
        XCTAssertNil(slashToken("/comp", cursor: 0))
        XCTAssertNil(slashToken("/comp", cursor: 6))
        XCTAssertNil(slashToken("", cursor: 0))
        XCTAssertNil(slashToken("hello", cursor: 3))
    }

    func testRanksPrefixesBeforeSubstringsLikeFilterIndices() {
        // popover.rs's own vector: prefix first, then substrings in input order.
        let labels = ["main", "feature/main-sync", "master", "dev"].map { command($0) }
        XCTAssertEqual(slashFilterIndices(query: "ma", commands: labels), [0, 2, 1])
        XCTAssertEqual(slashFilterIndices(query: "MA", commands: labels), [0, 2, 1])
        XCTAssertTrue(slashFilterIndices(query: "zzz", commands: labels).isEmpty)
        // Empty / whitespace query keeps catalog order.
        XCTAssertEqual(slashFilterIndices(query: "", commands: labels), [0, 1, 2, 3])
        XCTAssertEqual(slashFilterIndices(query: "   ", commands: labels), [0, 1, 2, 3])
    }

    func testMatchesAliasesAtTheirOwnRank() {
        let commands = [
            command("review", aliases: ["pr"]),
            command("compact", aliases: ["squash", "prune"]),
            command("promote"),
        ]
        // "pr" is an exact alias prefix on row 0 and row 1 ("prune"), and a
        // name prefix on row 2 — all rank 0, so catalog order decides.
        XCTAssertEqual(slashFilterIndices(query: "pr", commands: commands), [0, 1, 2])
        // Only the alias matches: the row still shows.
        XCTAssertEqual(slashFilterIndices(query: "squa", commands: commands), [1])
        // The best rank across name + aliases wins: "om" is a substring of
        // "promote" (rank 1) but a prefix of nothing, while "ompac" is a
        // substring of "compact" only.
        XCTAssertEqual(slashFilterIndices(query: "ompac", commands: commands), [1])
        XCTAssertEqual(slashFilterIndices(query: "om", commands: commands), [1, 2])
    }

    func testRowDetailAppendsTheInputHint() {
        XCTAssertEqual(command("compact", "Compact the thread", hint: "instructions").detail,
                       "Compact the thread · <instructions>")
        XCTAssertEqual(command("imagegen", hint: "prompt").detail, "<prompt>")
        XCTAssertEqual(command("architect", "Plan the work").detail, "Plan the work")
        XCTAssertEqual(command("architect").title, "/architect")
    }

    func testDecodesTheListCommandsReply() throws {
        // The exact wire shape of crates/engine/tests/skills_isolation.rs.
        let json = Data("""
        [{"name":"architect","description":"architect description"},
         {"name":"vercel:deploy","description":"","inputHint":"[prod]","aliases":["deploy"]}]
        """.utf8)
        let decoded = try JSONDecoder().decode([SlashCommand].self, from: json)
        XCTAssertEqual(decoded, [
            command("architect", "architect description"),
            command("vercel:deploy", hint: "[prod]", aliases: ["deploy"]),
        ])
    }

    func testReplacesTheTokenWithNameAndASpace() {
        let token = slashToken("/comp", cursor: 5)!
        let accepted = slashAccept(text: "/comp", token: token, command: command("compact"))
        XCTAssertEqual(accepted.text, "/compact ")
        XCTAssertEqual(accepted.cursor, 9)
    }

    func testKeepsAnExistingSeparatorAndLandsAfterIt() {
        // Arguments already typed: no second space, caret past the existing one.
        let token = slashToken("/comp now", cursor: 5)!
        let accepted = slashAccept(text: "/comp now", token: token, command: command("compact"))
        XCTAssertEqual(accepted.text, "/compact now")
        XCTAssertEqual(accepted.cursor, 9)
        // A newline is not a separator — the space is inserted.
        let multiline = slashToken("/comp\nnow", cursor: 5)!
        let wrapped = slashAccept(text: "/comp\nnow", token: multiline,
                                  command: command("compact"))
        XCTAssertEqual(wrapped.text, "/compact \nnow")
        XCTAssertEqual(wrapped.cursor, 9)
    }

    func testFetchesSeparatelyPerDeviceAndHarness() {
        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        // Still loading: no second probe, and no rows from anywhere else.
        XCTAssertNil(model.update(text: "/c", cursor: 2, key: claudeKey))
        XCTAssertEqual(model.popup, .loading)
        model.received([command("compact"), command("clear")], for: claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact"), command("clear")]))
        // A keystroke inside the same open never probes again.
        XCTAssertNil(model.update(text: "/co", cursor: 3, key: claudeKey))
        XCTAssertEqual(model.popup, .commands([command("compact")]))

        // A harness switch swaps the list immediately — never the previous
        // harness's entries while the new catalog loads (§10.6).
        XCTAssertEqual(model.update(text: "/co", cursor: 3, key: otherKey), otherKey)
        XCTAssertEqual(model.popup, .loading)
        model.received([command("code")], for: otherKey)
        XCTAssertEqual(model.popup, .commands([command("code")]))

        // Another device running the same harness is its own catalog.
        let remote = SlashCatalogKey(deviceId: "dev-b", harness: otherKey.harness,
                                     cwd: otherKey.cwd)
        XCTAssertEqual(model.update(text: "/co", cursor: 3, key: remote), remote)
        XCTAssertEqual(model.popup, .loading)
    }

    /// §10.4 "Freshness": every open revalidates the key's list — the token
    /// appearing, or the key changing while the popup is open — and nothing
    /// else does. Never one probe per keystroke, never a second one while the
    /// key's probe is in flight; the reply replaces the cached rows.
    func testRevalidatesOnEveryOpenAndNeverPerKeystroke() {
        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        // Keystrokes inside one open never probe — while it loads...
        XCTAssertNil(model.update(text: "/c", cursor: 2, key: claudeKey))
        model.received([command("compact"), command("clear")], for: claudeKey)
        // ...and with the rows on screen.
        XCTAssertNil(model.update(text: "/cl", cursor: 3, key: claudeKey))
        XCTAssertEqual(model.popup, .commands([command("clear")]))

        // Closing (the caret leaves the token) and opening again revalidates:
        // the cached rows show immediately, with no loading state.
        XCTAssertNil(model.update(text: "/clear now", cursor: 10, key: claudeKey))
        XCTAssertEqual(model.popup, .hidden)
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact"), command("clear")]))

        // Another open while that probe is still in flight sends nothing, and
        // the reply replaces the rows it revalidated.
        XCTAssertNil(model.update(text: "", cursor: 0, key: claudeKey))
        XCTAssertNil(model.update(text: "/", cursor: 1, key: claudeKey))
        model.received([command("compact")], for: claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact")]))
    }

    /// A key change while a probe is out opens the new key: it probes now, and
    /// the reply still owed by the old key is keyed to it, never rendered here.
    func testKeyChangeWhileInFlightProbesTheNewKeyAndDropsTheStaleReply() {
        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/c", cursor: 2, key: claudeKey), claudeKey)
        XCTAssertEqual(model.update(text: "/c", cursor: 2, key: otherKey), otherKey)
        XCTAssertEqual(model.popup, .loading)
        // The first harness answers late: its rows belong to its own key.
        model.received([command("compact")], for: claudeKey)
        XCTAssertEqual(model.popup, .loading)
        model.received([command("code")], for: otherKey)
        XCTAssertEqual(model.popup, .commands([command("code")]))
        // Switching back is another open: those rows show at once, revalidated.
        XCTAssertEqual(model.update(text: "/c", cursor: 2, key: claudeKey), claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact")]))
    }

    /// A transient probe failure never blanks a list that was fine a moment
    /// ago; the error row shows only when the key has no rows at all (§10.4).
    func testAFailedRevalidationKeepsTheRows() {
        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        model.received([command("compact")], for: claudeKey)
        XCTAssertNil(model.update(text: "", cursor: 0, key: claudeKey))
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact")]))
        model.failed("The device is offline", for: claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact")]))

        // No rows for the key: the same failure is the popup's error row.
        var cold = SlashCommandsModel()
        XCTAssertEqual(cold.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        cold.failed("The device is offline", for: claudeKey)
        XCTAssertEqual(cold.popup, .failed("The device is offline"))
    }

    /// Discovery is cwd-scoped (§10.4): the same harness on the same device in
    /// two folders is two catalogs, and switching folders on the new-session
    /// canvas swaps the rows immediately — never the previous folder's list.
    func testFetchesSeparatelyPerCwdAndSwapsRowsOnAFolderSwitch() {
        let repo = SlashCatalogKey(deviceId: "dev-a", harness: "harness-one", cwd: "/work/repo")
        let other = SlashCatalogKey(deviceId: "dev-a", harness: "harness-one", cwd: "/work/other")
        let engineDir = SlashCatalogKey(deviceId: "dev-a", harness: "harness-one", cwd: nil)
        XCTAssertNotEqual(repo, other)
        XCTAssertNotEqual(repo, engineDir)

        var model = SlashCommandsModel()
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: repo), repo)
        model.received([command("repo-only")], for: repo)
        XCTAssertEqual(model.popup, .commands([command("repo-only")]))

        // Same device, same harness, another folder: its own probe, and the
        // repo's entries are gone the instant the folder changes.
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: other), other)
        XCTAssertEqual(model.popup, .loading)
        model.received([command("other-only")], for: other)
        XCTAssertEqual(model.popup, .commands([command("other-only")]))

        // Back to the first folder: its cached list shows at once (no loading
        // state) while the open's own probe revalidates it (§10.4).
        XCTAssertEqual(model.update(text: "/r", cursor: 2, key: repo), repo)
        XCTAssertEqual(model.popup, .commands([command("repo-only")]))

        // A cwd-less key (no space picked yet) is a third catalog.
        XCTAssertEqual(model.update(text: "/", cursor: 1, key: engineDir), engineDir)
        XCTAssertEqual(model.popup, .loading)
    }

    /// `cwd` rides the request only when the surface has one — an omitted
    /// `cwd` is the engine-directory probe (§10.4).
    func testListCommandsParamsCarryCwdOnlyWhenKnown() {
        XCTAssertEqual(slashListCommandsParams(harness: "harness-one", cwd: "/work/repo"),
                       ["harness": "harness-one", "cwd": "/work/repo"])
        XCTAssertEqual(slashListCommandsParams(harness: "harness-one", cwd: nil),
                       ["harness": "harness-one"])
    }

    func testEmptyErrorAndNoMatchStates() {
        var model = SlashCommandsModel()
        _ = model.update(text: "/", cursor: 1, key: claudeKey)
        model.received([], for: claudeKey)
        XCTAssertEqual(model.popup, .noCommands)

        var failing = SlashCommandsModel()
        _ = failing.update(text: "/", cursor: 1, key: claudeKey)
        failing.failed("The device is offline", for: claudeKey)
        XCTAssertEqual(failing.popup, .failed("The device is offline"))
        // A keystroke inside the same open is not an open: no retry.
        XCTAssertNil(failing.update(text: "/c", cursor: 2, key: claudeKey))
        XCTAssertEqual(failing.popup, .failed("The device is offline"))
        // The next open retries (the desktop clears its error the same way).
        XCTAssertNil(failing.update(text: "", cursor: 0, key: claudeKey))
        XCTAssertEqual(failing.update(text: "/c", cursor: 2, key: claudeKey), claudeKey)
        XCTAssertEqual(failing.popup, .loading)
        failing.received([command("compact")], for: claudeKey)
        XCTAssertEqual(failing.popup, .commands([command("compact")]))
        _ = failing.update(text: "/zz", cursor: 3, key: claudeKey)
        XCTAssertEqual(failing.popup, .noMatches)

        // No resolved harness: empty popup, no probe.
        var unresolved = SlashCommandsModel()
        XCTAssertNil(unresolved.update(text: "/", cursor: 1, key: nil))
        XCTAssertEqual(unresolved.popup, .noCommands)
    }

    func testDismissHidesUntilTheTokenChanges() {
        var model = SlashCommandsModel()
        _ = model.update(text: "/co", cursor: 3, key: claudeKey)
        model.received([command("compact")], for: claudeKey)
        model.dismiss(in: "/co")
        XCTAssertEqual(model.popup, .hidden)
        // Same token, caret moved: still dismissed.
        XCTAssertNil(model.update(text: "/co", cursor: 2, key: claudeKey))
        XCTAssertEqual(model.popup, .hidden)
        // Editing the token reopens it — an open, so it revalidates the list
        // while the cached rows stay on screen (§10.4 "Freshness").
        XCTAssertEqual(model.update(text: "/com", cursor: 4, key: claudeKey), claudeKey)
        XCTAssertEqual(model.popup, .commands([command("compact")]))
    }

    func testAcceptClosesThePopupAndRewritesTheDraft() {
        var model = SlashCommandsModel()
        _ = model.update(text: "/co", cursor: 3, key: claudeKey)
        model.received([command("compact")], for: claudeKey)
        let accepted = model.accept(command("compact"), in: "/co")
        XCTAssertEqual(accepted, SlashAccept(text: "/compact ", cursor: 9))
        XCTAssertEqual(model.popup, .hidden)
        // The accepted draft leaves the caret in the argument, so the popup
        // stays closed on the next edit.
        XCTAssertNil(model.update(text: "/compact ", cursor: 9, key: claudeKey))
        XCTAssertEqual(model.popup, .hidden)
        // Nothing open: accept is a no-op.
        XCTAssertNil(model.accept(command("compact"), in: "/compact "))
    }

    /// The composer's own `/plan` row is a row for the LOADING state only
    /// (`self.slash.loading && commands.is_empty()` on the desktop): a probe
    /// that failed with no cached catalog for the key still shows its message,
    /// because `slash_failure_error` latches on `slash_cache.contains_key`,
    /// never on the merged rows.
    func testThePlanRowHidesTheSkeletonButNeverTheFailure() {
        // (a) No catalog for the key: the error row, not `/plan` alone.
        var cold = SlashCommandsModel()
        XCTAssertEqual(cold.update(text: "/", cursor: 1, key: claudeKey, planOffered: true),
                       claudeKey)
        cold.failed("The session's device is unreachable", for: claudeKey)
        XCTAssertEqual(cold.popup, .failed("The session's device is unreachable"))

        // (b) A failed revalidation over a cached catalog keeps the rows,
        // `/plan` first (§10.4).
        var warm = SlashCommandsModel()
        XCTAssertEqual(warm.update(text: "/", cursor: 1, key: claudeKey, planOffered: true),
                       claudeKey)
        warm.received([command("compact")], for: claudeKey)
        XCTAssertNil(warm.update(text: "", cursor: 0, key: claudeKey, planOffered: true))
        XCTAssertEqual(warm.update(text: "/", cursor: 1, key: claudeKey, planOffered: true),
                       claudeKey)
        warm.failed("The session's device is unreachable", for: claudeKey)
        XCTAssertEqual(warm.popup, .commands([planSlashCommand, command("compact")]))

        // (c) In flight with no catalog: `/plan` shows instead of the skeleton.
        var probing = SlashCommandsModel()
        XCTAssertEqual(probing.update(text: "/", cursor: 1, key: claudeKey, planOffered: true),
                       claudeKey)
        XCTAssertEqual(probing.popup, .commands([planSlashCommand]))

        // (d) No plan mode on this harness: both states read as before.
        var plain = SlashCommandsModel()
        XCTAssertEqual(plain.update(text: "/", cursor: 1, key: claudeKey), claudeKey)
        XCTAssertEqual(plain.popup, .loading)
        plain.failed("The session's device is unreachable", for: claudeKey)
        XCTAssertEqual(plain.popup, .failed("The session's device is unreachable"))
    }
}
