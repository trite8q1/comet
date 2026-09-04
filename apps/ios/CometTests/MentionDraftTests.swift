// Editor-model parity — the attributed draft must round-trip to the raw
// prompt the desktop composer sends, and must keep chips atomic the way
// composer.rs's `TextProjection` does (docs/composer-completions.md §2, §5.3,
// §6). The chip run is the phone's stand-in for the desktop's link range.

import SwiftUI
import XCTest
@testable import Comet

private let composer = FileMention(path: "src/composer.rs", isDir: false)
private let one = FileMention(path: "src/one/mod.rs", isDir: false)
private let two = FileMention(path: "src/two/mod.rs", isDir: false)

/// Type an `@`, accept `mention`: exactly what the composer does on a tap.
@discardableResult
private func insert(_ mention: FileMention, into draft: inout MentionDraft)
    -> AttributedTextSelection {
    draft.text.append(AttributedString("@"))
    let token = mentionToken(draft.display, cursor: draft.display.count)!
    let replacement = tokenReplacement(text: draft.display, range: token.range,
                                       inserted: mentionDisplayText(label: mention.basename))
    return draft.insertMention(replacement, chip: mention)
}

/// Delete one character, as a backspace in the editor would.
private func deleteCharacter(at offset: Int, in draft: inout MentionDraft) {
    let start = draft.text.characters.index(draft.text.startIndex, offsetBy: offset)
    draft.text.removeSubrange(start..<draft.text.characters.index(after: start))
}

private func offsets(_ selection: AttributedTextSelection,
                     in draft: MentionDraft) -> [Range<Int>] {
    func at(_ index: AttributedString.Index) -> Int {
        draft.text.characters.distance(from: draft.text.startIndex, to: index)
    }
    switch selection.indices(in: draft.text) {
    case .insertionPoint(let index):
        return [at(index)..<at(index)]
    case .ranges(let ranges):
        return ranges.ranges.map { at($0.lowerBound)..<at($0.upperBound) }
    }
}

// One class: `scripts/verify-skills.sh` runs the suite as
// `-only-testing:CometTests/MentionDraftTests`.
final class MentionDraftTests: XCTestCase {
    // MARK: - Insert and serialize (§2, §6)

    func testInsertingAChipSerializesToTheDesktopLink() {
        var draft = MentionDraft(plain: "Fix ")
        let selection = insert(composer, into: &draft)
        XCTAssertEqual(draft.display, "Fix \u{00A0}@composer.rs\u{00A0} ")
        XCTAssertEqual(draft.serialized(), "Fix [composer.rs](comet-file:src/composer.rs) ")
        XCTAssertEqual(draft.chips, [composer])
        XCTAssertEqual(draft.caret(of: selection), draft.display.count)
        // The prompt round-trips through the strict validator.
        XCTAssertEqual(fileMentionLinks(draft.serialized()).map(\.mention), [composer])
        XCTAssertFalse(draft.isEmpty)
        XCTAssertTrue(MentionDraft().isEmpty)
    }

    func testTheDisplayNeverCarriesTheLinkFormat() {
        var draft = MentionDraft()
        insert(composer, into: &draft)
        XCTAssertFalse(draft.display.contains("comet-file:"))
        XCTAssertFalse(draft.display.contains("]("))
        XCTAssertTrue(draft.serialized().contains("comet-file:"))
    }

    /// §5.3: text typed against a chip's edge is never part of it.
    func testTypedTextAfterAChipIsAttributeFree() {
        XCTAssertFalse(FileMentionAttribute.inheritedByAddedText)
        var draft = MentionDraft()
        insert(composer, into: &draft)
        draft.text.append(AttributedString("now"))
        XCTAssertFalse(draft.reconcile())
        XCTAssertEqual(draft.chips, [composer])
        XCTAssertEqual(draft.serialized(), "[composer.rs](comet-file:src/composer.rs) now")
    }

    // MARK: - Reconcile (§5.3)

    func testReconcileRemovesAChipWhoseBearingWasDeleted() {
        var draft = MentionDraft(plain: "Fix ")
        insert(composer, into: &draft)
        // A backspace eats the chip's trailing bearing: the whole chip goes.
        deleteCharacter(at: draft.display.count - 2, in: &draft)
        XCTAssertTrue(draft.reconcile())
        XCTAssertEqual(draft.display, "Fix  ")
        XCTAssertTrue(draft.chips.isEmpty)
        XCTAssertEqual(draft.serialized(), "Fix  ")
        // A settled draft reports no change, so the view stops writing back.
        XCTAssertFalse(draft.reconcile())
    }

    func testReconcileRelabelsChipsSharingABasename() {
        var draft = MentionDraft()
        insert(one, into: &draft)
        XCTAssertEqual(draft.display, "\u{00A0}@mod.rs\u{00A0} ")
        insert(two, into: &draft)
        // The second chip's arrival relabels both to unique suffixes.
        XCTAssertEqual(draft.display,
                       "\u{00A0}@one/mod.rs\u{00A0} \u{00A0}@two/mod.rs\u{00A0} ")
        XCTAssertEqual(draft.chips, [one, two])
        // The link's label is always the basename, never the display label.
        XCTAssertEqual(draft.serialized(),
                       "[mod.rs](comet-file:src/one/mod.rs) [mod.rs](comet-file:src/two/mod.rs) ")

        // Deleting the second chip relabels the survivor back to its basename.
        deleteCharacter(at: draft.display.count - 2, in: &draft)
        XCTAssertTrue(draft.reconcile())
        XCTAssertEqual(draft.display, "\u{00A0}@mod.rs\u{00A0}  ")
        XCTAssertEqual(draft.chips, [one])
    }

    /// Two chips for the same file that come to touch stay two chips: their
    /// attribute values differ by id, so the runs never coalesce into one run
    /// the reconcile rule would then drop whole.
    func testTouchingChipsForTheSameFileStayDistinct() {
        var draft = MentionDraft()
        insert(composer, into: &draft)
        insert(composer, into: &draft)
        // Same path twice: no suffix is unique, so both carry the full path.
        XCTAssertEqual(draft.display,
                       "\u{00A0}@src/composer.rs\u{00A0} \u{00A0}@src/composer.rs\u{00A0} ")
        // Delete the separator between the two chips.
        let gap = draft.display.distance(from: draft.display.startIndex,
                                         to: draft.display.firstIndex(of: " ")!)
        deleteCharacter(at: gap, in: &draft)
        XCTAssertFalse(draft.reconcile())
        XCTAssertEqual(draft.chips, [composer, composer])
        XCTAssertEqual(draft.serialized(),
                       "[composer.rs](comet-file:src/composer.rs)"
                           + "[composer.rs](comet-file:src/composer.rs) ")
    }

    // MARK: - Atomicity (§5.3)

    func testSnappingMovesAnInsertionPointOutOfAChip() throws {
        var draft = MentionDraft()
        insert(composer, into: &draft)
        // The chip spans 0..<14; its midpoint is 7.
        let inside = AttributedTextSelection(
            insertionPoint: draft.text.characters.index(draft.text.startIndex, offsetBy: 3))
        XCTAssertEqual(offsets(try XCTUnwrap(draft.snapped(inside)), in: draft), [0..<0])
        let late = AttributedTextSelection(
            insertionPoint: draft.text.characters.index(draft.text.startIndex, offsetBy: 10))
        XCTAssertEqual(offsets(try XCTUnwrap(draft.snapped(late)), in: draft), [14..<14])
        // Already on a boundary, or outside every chip: nothing to do.
        let boundary = AttributedTextSelection(
            insertionPoint: draft.text.characters.index(draft.text.startIndex, offsetBy: 14))
        XCTAssertNil(draft.snapped(boundary))
    }

    func testSnappingExpandsARangeOverAChip() throws {
        var draft = MentionDraft(plain: "Fix ")
        insert(composer, into: &draft)
        let characters = draft.text.characters
        func index(_ offset: Int) -> AttributedString.Index {
            characters.index(draft.text.startIndex, offsetBy: offset)
        }
        // A range biting into the chip covers the whole chip (4..<18).
        let partial = AttributedTextSelection(range: index(2)..<index(10))
        XCTAssertEqual(offsets(try XCTUnwrap(draft.snapped(partial)), in: draft), [2..<18])
        // A range clear of the chip is left alone.
        XCTAssertNil(draft.snapped(AttributedTextSelection(range: index(0)..<index(3))))
    }

    // MARK: - Plain replacements

    /// A slash accept rewrites the first token as plain text; the chips that
    /// follow it are untouched.
    func testApplyingASlashReplacementLeavesChipsIntact() {
        var draft = MentionDraft(plain: "/comp ")
        insert(composer, into: &draft)
        let token = slashToken(draft.display, cursor: 5)!
        let replacement = slashReplacement(text: draft.display, token: token,
                                           command: SlashCommand(name: "compact"))
        let selection = draft.apply(replacement)
        XCTAssertEqual(draft.display, "/compact \u{00A0}@composer.rs\u{00A0} ")
        XCTAssertEqual(draft.caret(of: selection), 9)
        XCTAssertEqual(draft.chips, [composer])
        XCTAssertEqual(draft.serialized(),
                       "/compact [composer.rs](comet-file:src/composer.rs) ")
        // The replacement itself carries no chip styling.
        XCTAssertFalse(draft.reconcile())
    }
}
