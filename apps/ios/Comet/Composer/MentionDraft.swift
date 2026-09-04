// The composer's editor model — docs/composer-completions.md §2 and §5.3. The
// desktop keeps raw Markdown as the source of truth and projects chips for
// display (composer.rs `TextProjection`); the phone inverts it, because on
// iOS 26 only `TextEditor` binds an `AttributedString` with a selection and
// there is no paint hook under a native editor. So the attributed draft IS the
// display, a chip is one run carrying `FileMentionAttribute`, and the raw
// prompt is derived at send time by `serialized()`.
//
// The rules this model obeys (labels, display text, link format) all live in
// `FileMentions.swift`; nothing here re-decides them.

import SwiftUI

// MARK: - The chip attribute

/// What a chip run remembers: the mention, and the label it was rendered with.
/// The label is stored so `reconcile` can tell an intact chip from one an edit
/// broke, and can rewrite chips whose deduped label changed (§5.1). `id` keeps
/// every chip's attribute value distinct: `AttributedString` coalesces adjacent
/// runs with equal attributes, and two chips for the same file that come to
/// touch (the space between them deleted) must stay two chips.
struct FileMentionChip: Codable, Hashable {
    let mention: FileMention
    let label: String
    let id: UUID

    init(mention: FileMention, label: String, id: UUID = UUID()) {
        self.mention = mention
        self.label = label
        self.id = id
    }
}

/// `inheritedByAddedText = false` is the phone's half of chip atomicity
/// (§5.3): text typed against a chip's edge is never part of it.
struct FileMentionAttribute: CodableAttributedStringKey {
    typealias Value = FileMentionChip
    static let name = "comet.fileMention"
    static let inheritedByAddedText = false
}

extension AttributeScopes {
    struct CometAttributes: AttributeScope {
        let fileMention: FileMentionAttribute
        let swiftUI: SwiftUIAttributes
    }

    var comet: CometAttributes.Type { CometAttributes.self }
}

extension AttributeDynamicLookup {
    subscript<T: AttributedStringKey>(
        dynamicMember keyPath: KeyPath<AttributeScopes.CometAttributes, T>
    ) -> T {
        self[T.self]
    }
}

// MARK: - Chip styling (§5.2)

/// Chip style is derived from the attribute's presence, never applied by hand:
/// typed text can never inherit it and a chip can never lose it (desktop
/// `theme.font_mono` / `code_text` / `code_wash`).
struct MentionFormatting: AttributedTextFormattingDefinition {
    struct Scope: AttributeScope {
        let fileMention: FileMentionAttribute
        let font: AttributeScopes.SwiftUIAttributes.FontAttribute
        let foregroundColor: AttributeScopes.SwiftUIAttributes.ForegroundColorAttribute
        let backgroundColor: AttributeScopes.SwiftUIAttributes.BackgroundColorAttribute
    }

    var body: some AttributedTextFormattingDefinition<Scope> {
        ChipFont()
        ChipForeground()
        ChipWash()
    }
}

struct ChipFont: AttributedTextValueConstraint {
    typealias Scope = MentionFormatting.Scope
    typealias AttributeKey = AttributeScopes.SwiftUIAttributes.FontAttribute

    func constrain(_ container: inout Attributes) {
        let isChip = container.fileMention != nil
        container.font = isChip ? Theme.mono(16) : nil
    }
}

struct ChipForeground: AttributedTextValueConstraint {
    typealias Scope = MentionFormatting.Scope
    typealias AttributeKey = AttributeScopes.SwiftUIAttributes.ForegroundColorAttribute

    func constrain(_ container: inout Attributes) {
        let isChip = container.fileMention != nil
        container.foregroundColor = isChip ? Theme.inlineCodeText : nil
    }
}

struct ChipWash: AttributedTextValueConstraint {
    typealias Scope = MentionFormatting.Scope
    typealias AttributeKey = AttributeScopes.SwiftUIAttributes.BackgroundColorAttribute

    func constrain(_ container: inout Attributes) {
        let isChip = container.fileMention != nil
        container.backgroundColor = isChip ? Theme.inlineCodeWash : nil
    }
}

// MARK: - The draft

/// The composer's text: an `AttributedString` bound to the editor, plus the
/// operations the completion rules need over it. Offsets are character offsets
/// into the display text, matching both token grammars.
struct MentionDraft: Equatable {
    var text: AttributedString

    init() {
        text = AttributedString()
    }

    init(plain: String) {
        text = AttributedString(plain)
    }

    /// The display text both grammars run over (`String(draft.text.characters)`).
    var display: String { String(text.characters) }

    var isEmpty: Bool { text.characters.isEmpty }

    /// The chips in document order.
    var chips: [FileMention] { chipSpans.map(\.chip.mention) }

    // MARK: Selection

    /// The caret as a character offset — the lower bound of a range selection,
    /// and the end of the draft when the selection carries no live indices
    /// (a programmatic rewrite can lag the editor by one pass).
    func caret(of selection: AttributedTextSelection) -> Int {
        switch selection.indices(in: text) {
        case .insertionPoint(let index):
            return offset(of: index)
        case .ranges(let ranges):
            guard let first = ranges.ranges.first else { return display.count }
            return offset(of: first.lowerBound)
        }
    }

    /// Chip atomicity for the selection (§5.3): an insertion point inside a
    /// chip moves to the nearer boundary (desktop `normalize_range`'s midpoint
    /// rule) and a range overlapping chips expands to cover them. nil when the
    /// selection already sits where it should.
    func snapped(_ selection: AttributedTextSelection) -> AttributedTextSelection? {
        let spans = chipSpans.map(\.range)
        switch selection.indices(in: text) {
        case .insertionPoint(let index):
            let at = offset(of: index)
            guard let chip = spans.first(where: { $0.lowerBound < at && at < $0.upperBound })
            else { return nil }
            let midpoint = chip.lowerBound + chip.count / 2
            let snapped = at < midpoint ? chip.lowerBound : chip.upperBound
            return AttributedTextSelection(insertionPoint: self.index(at: snapped))
        case .ranges(let ranges):
            var changed = false
            var expanded: [Range<AttributedString.Index>] = []
            for range in ranges.ranges {
                var lower = offset(of: range.lowerBound)
                var upper = offset(of: range.upperBound)
                for chip in spans where lower < chip.upperBound && upper > chip.lowerBound {
                    if chip.lowerBound < lower {
                        lower = chip.lowerBound
                        changed = true
                    }
                    if chip.upperBound > upper {
                        upper = chip.upperBound
                        changed = true
                    }
                }
                expanded.append(index(at: lower)..<index(at: upper))
            }
            guard changed else { return nil }
            return AttributedTextSelection(ranges: RangeSet(expanded))
        }
    }

    // MARK: Edits

    /// Apply a token replacement as plain, attribute-free text (§2.2) and
    /// return the selection the editor must adopt.
    mutating func apply(_ replacement: TokenReplacement) -> AttributedTextSelection {
        text.replaceSubrange(range(of: replacement.range),
                             with: AttributedString(replacement.inserted))
        return AttributedTextSelection(insertionPoint: index(at: replacement.cursor))
    }

    /// Like `apply`, but the inserted token text becomes one chip run carrying
    /// the mention; the appended separator stays attribute-free so prose typed
    /// after it is ordinary text.
    mutating func insertMention(_ replacement: TokenReplacement,
                                chip mention: FileMention) -> AttributedTextSelection {
        let label = mention.basename
        var inserted = chipRun(mention: mention, label: label)
        inserted.append(AttributedString(
            String(replacement.inserted.dropFirst(mentionDisplayText(label: label).count))
        ))
        text.replaceSubrange(range(of: replacement.range), with: inserted)
        var cursor = replacement.cursor
        _ = reconcile(adjusting: &cursor)
        return AttributedTextSelection(insertionPoint: index(at: cursor))
    }

    /// The §5.3 reconcile rule, run after every text change: a chip run is
    /// valid iff its characters equal the canonical display of its stored
    /// label, so any run an edit touched (a backspace ate its trailing bearing,
    /// a character was typed into it) is removed whole. Labels are then
    /// recomputed over what remains and changed runs rewritten. Returns whether
    /// anything moved — the view writes back only then, so `onChange` settles.
    @discardableResult
    mutating func reconcile() -> Bool {
        var cursor = 0
        return reconcile(adjusting: &cursor)
    }

    /// The raw Markdown prompt (§6): chip runs emit their link, everything
    /// else its characters verbatim.
    func serialized() -> String {
        let chars = Array(text.characters)
        var out = ""
        var at = 0
        for span in chipSpans {
            out += String(chars[at..<span.range.lowerBound])
            out += fileMentionLink(span.chip.mention)
            at = span.range.upperBound
        }
        return out + String(chars[at...])
    }

    // MARK: - Internals

    private func chipRun(mention: FileMention, label: String) -> AttributedString {
        var run = AttributedString(mentionDisplayText(label: label))
        run[FileMentionAttribute.self] = FileMentionChip(mention: mention, label: label)
        return run
    }

    /// The chip runs, by character offsets (stable across the mutations below,
    /// which walk them back to front).
    private var chipSpans: [(chip: FileMentionChip, range: Range<Int>)] {
        text.runs[FileMentionAttribute.self].compactMap { chip, range in
            guard let chip else { return nil }
            return (chip, offset(of: range.lowerBound)..<offset(of: range.upperBound))
        }
    }

    private func offset(of index: AttributedString.Index) -> Int {
        text.characters.distance(from: text.characters.startIndex, to: index)
    }

    private func index(at offset: Int) -> AttributedString.Index {
        text.characters.index(text.characters.startIndex, offsetBy: offset)
    }

    private func range(of offsets: Range<Int>) -> Range<AttributedString.Index> {
        index(at: offsets.lowerBound)..<index(at: offsets.upperBound)
    }

    /// Keep a caret offset pointing at the same place across one edit.
    private func adjust(_ cursor: inout Int, replacing range: Range<Int>, with length: Int) {
        if cursor >= range.upperBound {
            cursor += length - range.count
        } else if cursor > range.lowerBound {
            cursor = range.lowerBound + min(length, cursor - range.lowerBound)
        }
    }

    private mutating func reconcile(adjusting cursor: inout Int) -> Bool {
        var changed = false
        let chars = Array(text.characters)
        for span in chipSpans.reversed()
        where String(chars[span.range]) != mentionDisplayText(label: span.chip.label) {
            text.removeSubrange(range(of: span.range))
            adjust(&cursor, replacing: span.range, with: 0)
            changed = true
        }
        let spans = chipSpans
        let labels = mentionDisplayLabels(spans.map(\.chip.mention))
        for (ix, span) in spans.enumerated().reversed() where labels[ix] != span.chip.label {
            var run = chipRun(mention: span.chip.mention, label: labels[ix])
            run[FileMentionAttribute.self] = FileMentionChip(
                mention: span.chip.mention, label: labels[ix], id: span.chip.id)
            text.replaceSubrange(range(of: span.range), with: run)
            adjust(&cursor, replacing: span.range, with: run.characters.count)
            changed = true
        }
        return changed
    }
}
