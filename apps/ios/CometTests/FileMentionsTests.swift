// File-mention rules parity — the phone must behave exactly like
// crates/ui/src/composer.rs: the `@` token grammar, the strict `comet-file:`
// link format and its validator, the deduped chip labels, the transcript
// projection, and the search popup's states, freshness and dismissal
// (docs/composer-completions.md §2, §4, §5, §6). Every vector below is the
// desktop test's vector.

import XCTest
@testable import Comet

private func hit(_ path: String, dir: Bool = false) -> FileSearchMatch {
    FileSearchMatch(path: path, isDir: dir)
}

private func mention(_ path: String, dir: Bool = false) -> FileMention {
    FileMention(path: path, isDir: dir)
}

/// The chip display of a label, as characters, for slicing a projection.
private func chip(_ label: String) -> String {
    mentionDisplayText(label: label)
}

// One class: `scripts/verify-skills.sh` runs the suite as
// `-only-testing:CometTests/FileMentionsTests`.
final class FileMentionsTests: XCTestCase {
    // MARK: - Token grammar (§2.1)

    func testMentionTokenRequiresATokenBoundaryAndTracksFullToken() {
        XCTAssertEqual(mentionToken("Fix @src/com", cursor: 12),
                       MentionToken(range: 4..<12, query: "src/com"))
        // The `@` must begin a token.
        XCTAssertNil(mentionToken("mail@example.com", cursor: 16))
        XCTAssertNil(mentionToken("word@file", cursor: 9))
        XCTAssertNil(mentionToken("path/@file", cursor: 10))
        // Punctuation before the `@` is a boundary.
        XCTAssertEqual(mentionToken("See (@lib", cursor: 9)?.range, 5..<9)
        XCTAssertEqual(mentionToken("[@lib", cursor: 5)?.range, 1..<5)
        XCTAssertEqual(mentionToken("{@lib", cursor: 5)?.range, 1..<5)
        // The range runs past the caret to the next whitespace, so accepting
        // replaces the whole word being completed.
        XCTAssertEqual(mentionToken("Fix @src/com now", cursor: 8),
                       MentionToken(range: 4..<12, query: "src"))
        // A second `@` between the `@` and the caret closes the popup.
        XCTAssertNil(mentionToken("@src@lib", cursor: 8))
        // No token under the caret at all.
        XCTAssertNil(mentionToken("plain words", cursor: 5))
        XCTAssertNil(mentionToken("@src", cursor: 5))
    }

    func testMentionTokenTextIsTheDismissalKey() {
        let token = mentionToken("Fix @src now", cursor: 8)!
        XCTAssertEqual(mentionTokenText("Fix @src now", token), "@src")
    }

    // MARK: - Link format (§6)

    func testFileMentionsSerializeToStrictLocalMarkdown() {
        let file = mention("src/a file#[x].rs")
        XCTAssertEqual(fileMentionLink(file),
                       "[a file#\\[x\\].rs](comet-file:src/a%20file%23%5Bx%5D.rs)")
        let links = fileMentionLinks(fileMentionLink(file))
        XCTAssertEqual(links.count, 1)
        XCTAssertEqual(links[0].mention, file)
        XCTAssertEqual(links[0].mention.basename, "a file#[x].rs")
        XCTAssertFalse(links[0].mention.isDir)

        let folder = mention("src/components", dir: true)
        XCTAssertEqual(fileMentionLink(folder), "[components](comet-file:src/components/)")
        let folderLinks = fileMentionLinks(fileMentionLink(folder))
        XCTAssertEqual(folderLinks.count, 1)
        XCTAssertEqual(folderLinks[0].mention, folder)
        XCTAssertTrue(folderLinks[0].mention.isDir)

        // The link's range covers exactly the link, prose either side untouched.
        let inline = "check \(fileMentionLink(file)) now"
        XCTAssertEqual(fileMentionLinks(inline).first?.range,
                       6..<(6 + fileMentionLink(file).count))
    }

    func testFileMentionsRejectExternalOrNoncanonicalMarkdown() {
        XCTAssertTrue(fileMentionLinks("[site](https://example.com/a)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](../a.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](src/a file.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[other](src/a.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](src/a.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](src%5Cfake%5Ca.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](src/a%0A.rs)").isEmpty)
        // The scheme is required, and the rest of the validator holds under it.
        XCTAssertTrue(fileMentionLinks("[a.rs](comet-file:../a.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](comet-file:/abs/a.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[other](comet-file:src/a.rs)").isEmpty)
        // A non-canonical encoding is not a mention.
        XCTAssertTrue(fileMentionLinks("[a.rs](comet-file:src%2Fa.rs)").isEmpty)
        XCTAssertTrue(fileMentionLinks("[a.rs](comet-file:src/a%2ers)").isEmpty)
    }

    func testUnsafePathsAreRejected() {
        XCTAssertTrue(localPathIsSafe("src/a.rs"))
        XCTAssertFalse(localPathIsSafe(""))
        XCTAssertFalse(localPathIsSafe("/etc/passwd"))
        XCTAssertFalse(localPathIsSafe("src\\a.rs"))
        XCTAssertFalse(localPathIsSafe("src/../a.rs"))
        XCTAssertFalse(localPathIsSafe("src/./a.rs"))
        XCTAssertFalse(localPathIsSafe("src//a.rs"))
        XCTAssertFalse(localPathIsSafe("src/a\u{0A}.rs"))
    }

    // MARK: - Labels (§5.1)

    func testDuplicateMentionBasenamesUseUniqueSuffixes() {
        let labels = mentionDisplayLabels([mention("src/one/mod.rs"), mention("src/two/mod.rs")])
        XCTAssertEqual(labels, ["one/mod.rs", "two/mod.rs"])
        // A basename that appears once keeps it.
        XCTAssertEqual(mentionDisplayLabels([mention("src/one/mod.rs"), mention("src/lib.rs")]),
                       ["mod.rs", "lib.rs"])
    }

    func testMentionSuffixesComparePathComponents() {
        // `foo/mod.rs` and `bar/oomod.rs` share a substring, not a component.
        XCTAssertEqual(mentionDisplayLabels([mention("foo/mod.rs"), mention("bar/oomod.rs")]),
                       ["mod.rs", "oomod.rs"])
    }

    func testChipDisplayTextPadsAndReplacesSpaces() {
        XCTAssertEqual(mentionDisplayText(label: "a.rs"), "\u{00A0}@a.rs\u{00A0}")
        XCTAssertEqual(mentionDisplayText(label: "a file.rs"), "\u{00A0}@a\u{00A0}file.rs\u{00A0}")
    }

    // MARK: - Transcript projection (§6.1)

    func testSentMentionDisplayProjectsChipsForTheTranscript() throws {
        let file = mention("src/composer.rs")
        let folder = mention("src/components", dir: true)
        let raw = "check \(fileMentionLink(file)) and \(fileMentionLink(folder))"
        let projected = sentMentionDisplay(raw)
        let (display, spans) = try XCTUnwrap(projected)
        XCTAssertFalse(display.contains("comet-file:"))
        XCTAssertEqual(display, "check \(chip("composer.rs")) and \(chip("components"))")
        XCTAssertEqual(spans.count, 2)
        let chars = Array(display)
        XCTAssertEqual(String(chars[spans[0].range]), "\u{00A0}@composer.rs\u{00A0}")
        XCTAssertEqual(spans[0].mention, file)
        XCTAssertFalse(spans[0].mention.isDir)
        XCTAssertEqual(String(chars[spans[1].range]), "\u{00A0}@components\u{00A0}")
        XCTAssertEqual(spans[1].mention, folder)
        XCTAssertTrue(spans[1].mention.isDir)
    }

    func testSentMentionDisplayDedupesLabelsAcrossTheMessage() throws {
        let raw = "\(fileMentionLink(mention("src/one/mod.rs"))) "
            + "\(fileMentionLink(mention("src/two/mod.rs")))"
        let (display, spans) = try XCTUnwrap(sentMentionDisplay(raw))
        XCTAssertEqual(display, "\(chip("one/mod.rs")) \(chip("two/mod.rs"))")
        XCTAssertEqual(spans.map(\.mention.path), ["src/one/mod.rs", "src/two/mod.rs"])
    }

    /// Ordinary prompts stay on the zero-cost path, including ones that merely
    /// talk about the scheme without carrying a valid mention.
    func testSentMentionDisplayLeavesPlainPromptsUntouched() {
        XCTAssertNil(sentMentionDisplay("fix the composer"))
        XCTAssertNil(sentMentionDisplay("what is a comet-file: link?"))
        XCTAssertNil(sentMentionDisplay("[a.rs](comet-file:../a.rs)"))
    }

    // MARK: - Search state (§4.2, §4.3)

    func testAFreshOpenLoadsAndRefiningKeepsTheRows() throws {
        var model = FileMentionsModel()
        XCTAssertEqual(model.popup, .hidden)
        let open = try XCTUnwrap(model.update(text: "@", cursor: 1))
        XCTAssertEqual(open.query, "")
        XCTAssertEqual(model.popup, .loading)
        model.received([hit("src/a.rs")], generation: open.generation)
        XCTAssertEqual(model.popup, .matches([hit("src/a.rs")]))
        XCTAssertEqual(model.active, 0)

        // Refining: the previous rows stay up while the new search runs.
        let refined = try XCTUnwrap(model.update(text: "@a", cursor: 2))
        XCTAssertEqual(refined.query, "a")
        XCTAssertNotEqual(refined.generation, open.generation)
        XCTAssertEqual(model.popup, .matches([hit("src/a.rs")]))
        model.received([hit("src/ab.rs")], generation: refined.generation)
        XCTAssertEqual(model.popup, .matches([hit("src/ab.rs")]))

        // The caret leaving the token resets everything.
        XCTAssertNil(model.update(text: "@a b", cursor: 4))
        XCTAssertEqual(model.popup, .hidden)
        XCTAssertTrue(model.results.isEmpty)
        XCTAssertNil(model.active)
        XCTAssertFalse(model.loading)
    }

    /// `mention_response_is_current`: a reply lands only when its generation is
    /// the current one AND a token is still live.
    func testStaleRepliesAreDropped() throws {
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "@src", cursor: 4))
        XCTAssertFalse(model.isStale(open.generation))
        let refined = try XCTUnwrap(model.update(text: "@srcs", cursor: 5))
        XCTAssertTrue(model.isStale(open.generation))
        model.received([hit("src/stale.rs")], generation: open.generation)
        XCTAssertEqual(model.popup, .loading)
        model.failed("File search failed", generation: open.generation)
        XCTAssertEqual(model.popup, .loading)

        // The token disappearing invalidates even the current generation.
        XCTAssertNil(model.update(text: "srcs", cursor: 4))
        XCTAssertTrue(model.isStale(refined.generation))
        model.received([hit("src/late.rs")], generation: refined.generation)
        XCTAssertEqual(model.popup, .hidden)
    }

    /// A failure MUST NOT render as "no matching files" — cross-device failures
    /// are actionable and the empty state hid them (§4.3).
    func testAFailureRendersAsItsMessage() throws {
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "@z", cursor: 2))
        model.failed("The session's device is unreachable", generation: open.generation)
        XCTAssertEqual(model.popup, .failed("The session's device is unreachable"))
        XCTAssertTrue(model.results.isEmpty)
        // The next token change clears it and searches again.
        let next = try XCTUnwrap(model.update(text: "@zz", cursor: 3))
        XCTAssertEqual(model.popup, .loading)
        model.received([], generation: next.generation)
        XCTAssertEqual(model.popup, .noMatches)
    }

    func testEmptyResultsDistinguishNoFilesFromNoMatches() throws {
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "@", cursor: 1))
        model.received([], generation: open.generation)
        XCTAssertEqual(model.popup, .noFiles)
        let refined = try XCTUnwrap(model.update(text: "@zz", cursor: 3))
        model.received([], generation: refined.generation)
        XCTAssertEqual(model.popup, .noMatches)
    }

    /// No chat and no space: the popup renders "No files available" and no
    /// `SearchFiles` is ever sent (§4.1).
    func testAScopelessSurfaceRendersNoFilesWithoutASearch() {
        var model = FileMentionsModel()
        XCTAssertNotNil(model.update(text: "@", cursor: 1))
        model.searchUnavailable()
        XCTAssertEqual(model.popup, .noFiles)
    }

    /// The search root moving under a live token re-issues the same query so
    /// the rows track the picked worktree — slash's key-revalidate twin (§4.1).
    /// The new-session checkout / space `onChange`s ride on this.
    func testAScopeChangeReissuesUnderALiveToken() throws {
        let here = MentionSearchScope(deviceId: "d", scope: .space(spaceId: "s", path: "/here"))
        let there = MentionSearchScope(deviceId: "d", scope: .space(spaceId: "s", path: "/there"))
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "@src", cursor: 4, scope: here))
        model.received([hit("here/src.rs")], generation: open.generation)

        // Same token text, moved checkout: a fresh request goes out (same
        // query, newer generation), and the prior rows stay up until it lands.
        let moved = try XCTUnwrap(model.update(text: "@src", cursor: 4, scope: there))
        XCTAssertEqual(moved.query, "src")
        XCTAssertNotEqual(moved.generation, open.generation)
        XCTAssertEqual(model.popup, .matches([hit("here/src.rs")]))
        model.received([hit("there/src.rs")], generation: moved.generation)
        XCTAssertEqual(model.popup, .matches([hit("there/src.rs")]))

        // Same token AND same scope: still a no-op, so keystroke-free redraws
        // do not re-probe.
        XCTAssertNil(model.update(text: "@src", cursor: 4, scope: there))

        // The scope falling away under a live token still re-issues, so the
        // surface can drop to the no-files path.
        XCTAssertNotNil(model.update(text: "@src", cursor: 4, scope: nil))
    }

    func testDismissHidesUntilTheTokenChanges() throws {
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "Fix @src", cursor: 8))
        model.received([hit("src/a.rs")], generation: open.generation)
        model.dismiss(in: "Fix @src")
        XCTAssertEqual(model.popup, .hidden)
        // Same token, caret moved: still dismissed.
        XCTAssertNil(model.update(text: "Fix @src", cursor: 6))
        XCTAssertEqual(model.popup, .hidden)
        // Editing the token reopens it as a fresh open.
        let reopened = try XCTUnwrap(model.update(text: "Fix @srcs", cursor: 9))
        XCTAssertEqual(reopened.query, "srcs")
        XCTAssertEqual(model.popup, .loading)
    }

    // MARK: - Accept (§2.2)

    func testAcceptReplacesTheWholeTokenAndAppendsASeparator() throws {
        var model = FileMentionsModel()
        let open = try XCTUnwrap(model.update(text: "Fix @src", cursor: 8))
        model.received([hit("src/composer.rs")], generation: open.generation)
        let accepted = model.accept(hit("src/composer.rs"), in: "Fix @src")
        let (replacement, inserted) = try XCTUnwrap(accepted)
        XCTAssertEqual(replacement, TokenReplacement(range: 4..<8,
                                                     inserted: "\u{00A0}@composer.rs\u{00A0} ",
                                                     cursor: 19))
        XCTAssertEqual(inserted, mention("src/composer.rs"))
        XCTAssertEqual(replacement.applied(to: "Fix @src"), "Fix \u{00A0}@composer.rs\u{00A0} ")
        XCTAssertEqual(model.popup, .hidden)
        // Nothing open: accept is a no-op.
        XCTAssertNil(model.accept(hit("src/composer.rs"), in: "Fix @src"))
    }

    func testAcceptKeepsAnExistingSeparatorAndLandsAfterIt() throws {
        var model = FileMentionsModel()
        _ = model.update(text: "Fix @src now", cursor: 8)
        let (replacement, inserted) = try XCTUnwrap(
            model.accept(hit("src/components", dir: true), in: "Fix @src now"))
        XCTAssertEqual(replacement, TokenReplacement(range: 4..<8,
                                                     inserted: "\u{00A0}@components\u{00A0}",
                                                     cursor: 18))
        XCTAssertEqual(replacement.applied(to: "Fix @src now"),
                       "Fix \u{00A0}@components\u{00A0} now")
        XCTAssertTrue(inserted.isDir)
        XCTAssertEqual(inserted.path, "src/components")
    }
}
