// File mentions — the phone half of docs/composer-completions.md §2, §4, §5
// and §6. A port of composer.rs's `mention_token`, `local_file_link` /
// `file_mention_links` / `percent_encode_path` / `local_path_is_safe`,
// `mention_display_labels`, `TextProjection` and `FileMentionState`: same
// grammar, same link format, same labels, same popup states. Pure logic — no
// views, no editor types (those live in `MentionDraft.swift`), and no harness
// identity: the search root comes from the chat or space row.
//
// This file is the ONE place the mention link scheme is spelled on the phone
// (§1 boundary rule); every producer and consumer goes through `fileMentionLink`
// and `fileMentionLinks`.

import Foundation

// MARK: - Chip identity

/// One file mention (§5): a checkout-relative, `/`-separated path without a
/// trailing slash, directories flagged. Everything a chip shows is derived.
struct FileMention: Codable, Hashable {
    let path: String
    let isDir: Bool

    init(path: String, isDir: Bool) {
        var path = path
        while path.hasSuffix("/") { path.removeLast() }
        self.path = path
        self.isDir = isDir
    }

    /// Default chip label and the label the link serializes (§6).
    var basename: String {
        guard let cut = path.lastIndex(of: "/") else { return path }
        let tail = String(path[path.index(after: cut)...])
        return tail.isEmpty ? path : tail
    }
}

// MARK: - Link format (§6)

/// A private URI scheme keeps file mentions distinguishable from ordinary
/// Markdown links pasted into the composer (`FILE_MENTION_SCHEME`).
let fileMentionScheme = "comet-file:"

/// `percent_encode_path`: byte-wise, `A–Z a–z 0–9 - . _ ~ /` pass through and
/// every other byte becomes `%XX` with uppercase hex.
func percentEncodePath(_ path: String) -> String {
    var out = ""
    for byte in Array(path.utf8) {
        let passes = (byte >= 0x41 && byte <= 0x5A) || (byte >= 0x61 && byte <= 0x7A)
            || (byte >= 0x30 && byte <= 0x39)
            || byte == UInt8(ascii: "-") || byte == UInt8(ascii: ".")
            || byte == UInt8(ascii: "_") || byte == UInt8(ascii: "~")
            || byte == UInt8(ascii: "/")
        if passes {
            out.append(Character(UnicodeScalar(byte)))
        } else {
            out += String(format: "%%%02X", byte)
        }
    }
    return out
}

/// `percent_decode_path`: nil when a triplet is truncated or the bytes are not
/// valid UTF-8.
func percentDecodePath(_ encoded: String) -> String? {
    let raw = Array(encoded.utf8)
    var bytes: [UInt8] = []
    bytes.reserveCapacity(raw.count)
    var at = 0
    while at < raw.count {
        if raw[at] == UInt8(ascii: "%") {
            guard at + 3 <= raw.count,
                  let hex = String(bytes: raw[(at + 1)..<(at + 3)], encoding: .utf8),
                  let byte = UInt8(hex, radix: 16)
            else { return nil }
            bytes.append(byte)
            at += 3
        } else {
            bytes.append(raw[at])
            at += 1
        }
    }
    return String(bytes: bytes, encoding: .utf8)
}

/// `escape_mention_label`: the label half of the Markdown link.
func escapeMentionLabel(_ label: String) -> String {
    label
        .replacingOccurrences(of: "\\", with: "\\\\")
        .replacingOccurrences(of: "[", with: "\\[")
        .replacingOccurrences(of: "]", with: "\\]")
}

/// `local_file_link`: `[<escaped basename>](comet-file:<encoded path>[/])`.
/// The label is always the basename, never the deduped display label (§6).
func fileMentionLink(_ mention: FileMention) -> String {
    let target = mention.path + (mention.isDir ? "/" : "")
    return "[\(escapeMentionLabel(mention.basename))](\(fileMentionScheme)\(percentEncodePath(target)))"
}

/// `local_path_is_safe`: relative, non-empty, no `\`, no control character,
/// no empty / `.` / `..` component.
func localPathIsSafe(_ path: String) -> Bool {
    guard !path.isEmpty, !path.hasPrefix("/"), !path.contains("\\") else { return false }
    if path.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) {
        return false
    }
    return !path.split(separator: "/", omittingEmptySubsequences: false)
        .contains { $0.isEmpty || $0 == "." || $0 == ".." }
}

/// One valid mention link found in a text, by character offsets.
struct FileMentionLinkMatch: Equatable {
    let range: Range<Int>
    let mention: FileMention
}

/// `label_close`: the `]` that closes a label, honoring backslash escapes and
/// requiring the `(` of the target right after it.
private func labelClose(_ chars: [Character], from start: Int) -> Int? {
    var escaped = false
    var at = start
    while at < chars.count {
        let ch = chars[at]
        if escaped {
            escaped = false
        } else if ch == "\\" {
            escaped = true
        } else if ch == "]", at + 1 < chars.count, chars[at + 1] == "(" {
            return at
        }
        at += 1
    }
    return nil
}

/// `file_mention_links`: the strict validator (§6) and the only way a link
/// becomes a chip. Non-canonical encodings, external schemes, unsafe paths and
/// labels that are not the escaped basename all stay ordinary text.
func fileMentionLinks(_ text: String) -> [FileMentionLinkMatch] {
    let chars = Array(text)
    var links: [FileMentionLinkMatch] = []
    var search = 0
    while search < chars.count, let start = chars[search...].firstIndex(of: "[") {
        guard let labelEnd = labelClose(chars, from: start + 1) else {
            search = start + 1
            continue
        }
        let targetStart = labelEnd + 2
        guard let close = chars[targetStart...].firstIndex(of: ")") else {
            search = start + 1
            continue
        }
        let end = close + 1
        search = end
        let target = String(chars[targetStart..<(end - 1)])
        guard target.hasPrefix(fileMentionScheme) else { continue }
        let encoded = String(target.dropFirst(fileMentionScheme.count))
        guard let decoded = percentDecodePath(encoded), percentEncodePath(decoded) == encoded
        else { continue }
        let isDir = decoded.hasSuffix("/")
        let mention = FileMention(path: isDir ? String(decoded.dropLast()) : decoded, isDir: isDir)
        guard localPathIsSafe(mention.path),
              escapeMentionLabel(mention.basename) == String(chars[(start + 1)..<labelEnd])
        else { continue }
        links.append(FileMentionLinkMatch(range: start..<end, mention: mention))
    }
    return links
}

// MARK: - Labels and display text (§5.1, §5.2)

/// `mention_display_labels`: the basename, or — when a draft holds two chips
/// with the same basename — the shortest path suffix that is unique among the
/// draft's chips. Suffixes are compared by whole path components, never by
/// substring (`foo/mod.rs` vs `bar/oomod.rs` stay `mod.rs` / `oomod.rs`).
func mentionDisplayLabels(_ mentions: [FileMention]) -> [String] {
    mentions.enumerated().map { ix, mention in
        guard mentions.filter({ $0.basename == mention.basename }).count > 1 else {
            return mention.basename
        }
        let parts = mention.path.split(separator: "/").map(String.init)
        for count in 1...max(parts.count, 1) {
            let suffix = Array(parts.suffix(count))
            let unique = mentions.enumerated().allSatisfy { otherIx, other in
                otherIx == ix
                    || Array(other.path.split(separator: "/").map(String.init).suffix(count)) != suffix
            }
            if unique { return suffix.joined(separator: "/") }
        }
        return mention.path
    }
}

/// The side bearings that keep a chip one word for both token grammars and
/// give the wash room (`MENTION_SIDE_PAD` / `MENTION_PREFIX`).
let mentionSidePad = "\u{00A0}"
let mentionPrefix: Character = "@"

/// A chip's display text (§5.2): `\u{00A0}@label\u{00A0}`, with spaces inside
/// the label replaced by non-breaking spaces.
func mentionDisplayText(label: String) -> String {
    var out = mentionSidePad
    out.append(mentionPrefix)
    for ch in label { out.append(ch == " " ? "\u{00A0}" : ch) }
    out += mentionSidePad
    return out
}

// MARK: - Token grammar (§2.1)

/// The `@token` under the caret: its character range in the draft (extending
/// past the caret to the next whitespace) and the query typed so far.
struct MentionToken: Equatable {
    let range: Range<Int>
    let query: String
}

/// `mention_token`: the `@` must begin a token — at offset 0, or preceded by
/// whitespace or one of `(`, `[`, `{`. This excludes `mail@example.com`,
/// `word@file` and `path/@file` while allowing `(@src`.
func mentionToken(_ text: String, cursor: Int) -> MentionToken? {
    let chars = Array(text)
    guard cursor >= 0, cursor <= chars.count else { return nil }
    let tokenStart = chars[..<cursor].lastIndex(where: \.isWhitespace).map { $0 + 1 } ?? 0
    guard let at = chars[tokenStart..<cursor].lastIndex(of: mentionPrefix) else { return nil }
    let validBoundary = at == 0
        || chars[at - 1].isWhitespace
        || chars[at - 1] == "(" || chars[at - 1] == "[" || chars[at - 1] == "{"
    guard validBoundary, !chars[(at + 1)..<cursor].contains(mentionPrefix) else { return nil }
    let end = chars[cursor...].firstIndex(where: \.isWhitespace) ?? chars.count
    return MentionToken(range: at..<end, query: String(chars[(at + 1)..<cursor]))
}

/// The token's own text, as it currently reads in the draft (the dismissal key).
func mentionTokenText(_ text: String, _ token: MentionToken) -> String {
    let chars = Array(text)
    guard token.range.upperBound <= chars.count else { return "" }
    return String(chars[token.range])
}

// MARK: - Transcript projection (§6.1)

/// One chip in a *sent* message: its character range over the projected
/// display string. Directories are flagged on the mention, never by a trailing
/// slash on a display path (§8).
struct SentMentionSpan: Equatable {
    let range: Range<Int>
    let mention: FileMention
}

/// `sent_mention_display`: project a sent message's raw Markdown for the
/// transcript — mention links collapse to the chip display text (§5.2) with
/// labels deduped across the message, everything else passes through. nil when
/// the text has no valid mention; the substring probe keeps ordinary prompts on
/// the zero-cost path, so this is safe to call for every user row.
func sentMentionDisplay(_ raw: String) -> (display: String, spans: [SentMentionSpan])? {
    guard raw.contains(fileMentionScheme) else { return nil }
    let links = fileMentionLinks(raw)
    guard !links.isEmpty else { return nil }
    let labels = mentionDisplayLabels(links.map(\.mention))
    let chars = Array(raw)
    var display = ""
    var spans: [SentMentionSpan] = []
    var rawAt = 0
    var displayAt = 0
    for (link, label) in zip(links, labels) {
        display += String(chars[rawAt..<link.range.lowerBound])
        displayAt += link.range.lowerBound - rawAt
        let chip = mentionDisplayText(label: label)
        display += chip
        let end = displayAt + chip.count
        spans.append(SentMentionSpan(range: displayAt..<end, mention: link.mention))
        displayAt = end
        rawAt = link.range.upperBound
    }
    display += String(chars[rawAt...])
    return (display, spans)
}

// MARK: - Popup state (§4.2, §4.3)

/// What the `@` card renders (`render_file_mention_popup`'s branches, in its
/// order). A failure never renders as "no matches" — cross-device failures are
/// actionable and the empty state hid them.
enum MentionPopup: Equatable {
    case hidden
    case loading
    case failed(String)
    /// Empty query, nothing to show: "No files available".
    case noFiles
    /// A query with no hits: "No matching files".
    case noMatches
    case matches([FileSearchMatch])
}

/// A search the surface must send after the 80 ms debounce, tagged with the
/// generation its reply has to carry back.
struct MentionSearchRequest: Equatable {
    let generation: UInt64
    let query: String
}

/// The device + checkout a search runs against. When it moves under a live `@`
/// token (the new-session composer switching worktree / checkout / space) the
/// popup must re-issue against the new root — slash's `SlashCatalogKey` twin
/// (§4.1: the search root follows the checkout plan).
struct MentionSearchScope: Equatable {
    let deviceId: String
    let scope: FileSearchScope
}

/// The composer's mention state: the live token, the last result set, and the
/// request generation that decides which replies still count (`FileMentionState`
/// + `mention_response_is_current`).
struct FileMentionsModel {
    private(set) var token: MentionToken?
    private(set) var results: [FileSearchMatch] = []
    /// The desktop's keyboard cursor. A tap chooses on the phone, so nothing
    /// renders it — the model keeps it for parity.
    private(set) var active: Int?
    private(set) var loading = false
    private(set) var error: String?
    private(set) var generation: UInt64 = 0
    /// Token text hidden by an explicit dismiss — until the token changes.
    private var dismissed: String?
    /// The search root the live token was last issued against; a move under an
    /// open token re-issues, the way slash re-probes on a key change (§4.1).
    private var scope: MentionSearchScope?

    /// Track the token on every edit / caret move / checkout switch. Returns the
    /// search to send after the debounce, or nil when nothing must be (re)issued.
    mutating func update(text: String, cursor: Int,
                         scope: MentionSearchScope? = nil) -> MentionSearchRequest? {
        let token = mentionToken(text, cursor: cursor)
        // The search root can move while the popup is open (the new-session
        // checkout / space switching): re-issue the same query then, so the
        // rows track the picked worktree. A dismissed or absent token never does.
        let scopeMoved = token != nil && scope != self.scope
        self.scope = scope
        if let token, let dismissed, mentionTokenText(text, token) == dismissed {
            self.token = nil
            return nil
        }
        dismissed = nil
        if token == self.token, !scopeMoved { return nil }
        // Every token change bumps the generation, so a reply already queued
        // for the previous one is dropped on arrival.
        generation &+= 1
        // Refining an open menu keeps the stale rows visible until the new
        // reply lands — clearing here bounced the popup through the skeleton
        // (and a different height) on every keystroke.
        let refining = self.token != nil && token != nil
        self.token = token
        if !refining {
            results = []
            active = nil
        }
        error = nil
        loading = token != nil
        guard let token else { return nil }
        return MentionSearchRequest(generation: generation, query: token.query)
    }

    /// `mention_response_is_current`, inverted: a reply counts only when its
    /// generation is the current one AND a token is still live.
    func isStale(_ generation: UInt64) -> Bool {
        generation != self.generation || token == nil
    }

    mutating func received(_ matches: [FileSearchMatch], generation: UInt64) {
        guard !isStale(generation) else { return }
        loading = false
        error = nil
        results = matches
        active = matches.isEmpty ? nil : 0
    }

    mutating func failed(_ message: String, generation: UInt64) {
        guard !isStale(generation) else { return }
        loading = false
        results = []
        active = nil
        error = message
    }

    /// No chat and no space: the popup renders "No files available" without
    /// ever sending a `SearchFiles` (§4.1).
    mutating func searchUnavailable() {
        loading = false
        results = []
        active = nil
    }

    /// Hide the popup for this exact token; any edit to it opens again.
    mutating func dismiss(in text: String) {
        guard let token else { return }
        dismissed = mentionTokenText(text, token)
        self.token = nil
        results = []
        active = nil
        loading = false
        error = nil
        generation &+= 1
    }

    /// Replace the token with the chip's display text (§2.2), closing the
    /// popup. The view turns the replacement into a chip run through
    /// `MentionDraft.insertMention`.
    mutating func accept(_ match: FileSearchMatch,
                         in text: String) -> (TokenReplacement, FileMention)? {
        guard let token else { return nil }
        let mention = FileMention(path: match.path, isDir: match.isDir)
        let replacement = tokenReplacement(text: text, range: token.range,
                                           inserted: mentionDisplayText(label: mention.basename))
        self.token = nil
        dismissed = nil
        results = []
        active = nil
        loading = false
        error = nil
        generation &+= 1
        return (replacement, mention)
    }

    var popup: MentionPopup {
        guard let token else { return .hidden }
        if loading, results.isEmpty { return .loading }
        if let error { return .failed(error) }
        if results.isEmpty { return token.query.isEmpty ? .noFiles : .noMatches }
        return .matches(results)
    }
}
