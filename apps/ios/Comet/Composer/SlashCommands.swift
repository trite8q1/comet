// Slash-command completion — the phone half of ARCHITECTURE.md §10.2. A port
// of composer.rs's `slash_token` / `SlashState` / `refilter_slash` /
// `accept_slash` / `dismiss_slash` and popover.rs's `match_rank` +
// `filter_indices`: same token grammar, same ranking, same accept text, same
// popup states. Pure logic — no views, and no harness identity: the catalog is
// whatever `ListCommands {harness, cwd}` answered for the resolved
// (device, harness, cwd) key, never something this file decides.

import Foundation

// MARK: - Wire

/// One invocable the agent advertises (`comet_proto::SlashCommand`, camelCase
/// on the wire). `description` and `aliases` are serde-defaulted host-side, so
/// they decode as absent too.
struct SlashCommand: Decodable, Equatable, Identifiable {
    let name: String
    let description: String
    let inputHint: String?
    let aliases: [String]

    var id: String { name }

    init(name: String, description: String = "", inputHint: String? = nil,
         aliases: [String] = []) {
        self.name = name
        self.description = description
        self.inputHint = inputHint
        self.aliases = aliases
    }

    private enum CodingKeys: String, CodingKey {
        case name, description, inputHint, aliases
    }

    init(from decoder: Decoder) throws {
        let row = try decoder.container(keyedBy: CodingKeys.self)
        name = try row.decode(String.self, forKey: .name)
        description = try row.decodeIfPresent(String.self, forKey: .description) ?? ""
        inputHint = try row.decodeIfPresent(String.self, forKey: .inputHint)
        aliases = try row.decodeIfPresent([String].self, forKey: .aliases) ?? []
    }

    /// Row title: commands render as `/name` (render_slash_popup).
    var title: String { "/\(name)" }

    /// Row subtitle: the description, with the argument hint appended as
    /// `<hint>` — ` · ` separated when a description exists.
    var detail: String {
        guard let inputHint else { return description }
        return description.isEmpty ? "<\(inputHint)>" : "\(description) · <\(inputHint)>"
    }
}

/// A catalog probe's outcome: the harness's list, or why it couldn't be read
/// (the popup's error row).
enum SlashCatalog: Equatable {
    case commands([SlashCommand])
    case failure(String)
}

// MARK: - Token grammar

/// The `/token` under the caret: its character range in the draft and the
/// query typed so far (the text between `/` and the caret).
struct SlashToken: Equatable {
    let range: Range<Int>
    let query: String
}

/// `slash_token`: slash commands are whole-prompt prefixes (`/compact`,
/// `/goal ship it`), so the popup opens only when `/` is the draft's FIRST
/// character and the caret sits inside that first token. A token carrying a
/// second `/` (a typed path) never opens.
func slashToken(_ text: String, cursor: Int) -> SlashToken? {
    let chars = Array(text)
    guard cursor > 0, cursor <= chars.count, chars.first == "/" else { return nil }
    let end = chars.firstIndex(where: \.isWhitespace) ?? chars.count
    // Caret outside the command token (typing the argument): popup closed.
    guard cursor <= end else { return nil }
    let query = String(chars[1..<cursor])
    guard !query.contains("/") else { return nil }
    return SlashToken(range: 0..<end, query: query)
}

/// The token's own text, as it currently reads in the draft (the dismissal key).
func slashTokenText(_ text: String, _ token: SlashToken) -> String {
    let chars = Array(text)
    guard token.range.upperBound <= chars.count else { return "" }
    return String(chars[token.range])
}

// MARK: - Filtering (popover.rs)

/// `popover::match_rank`: 0 = prefix, 1 = substring, nil = no match.
/// Case-insensitive; an empty query matches everything at rank 1.
func slashMatchRank(query: String, label: String) -> Int? {
    let query = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    if query.isEmpty { return 1 }
    let label = label.lowercased()
    if label.hasPrefix(query) { return 0 }
    return label.contains(query) ? 1 : nil
}

/// `popover::filter_indices`, ranked over each command's name AND its aliases
/// (the agents' own popups match aliases too — best rank wins). Prefix matches
/// first, then substrings, catalog order within each rank.
func slashFilterIndices(query: String, commands: [SlashCommand]) -> [Int] {
    commands.enumerated()
        .compactMap { ix, command -> (rank: Int, ix: Int)? in
            let ranks = ([command.name] + command.aliases)
                .compactMap { slashMatchRank(query: query, label: $0) }
            guard let best = ranks.min() else { return nil }
            return (best, ix)
        }
        .sorted { ($0.rank, $0.ix) < ($1.rank, $1.ix) }
        .map(\.ix)
}

// MARK: - Accept

/// The draft after accepting a command, and where the caret lands in it
/// (character offsets).
struct SlashAccept: Equatable {
    let text: String
    let cursor: Int
}

/// `accept_slash` + `replace_plain_token`: the token becomes `/name` followed
/// by a space (unless a separator already sits there), and the caret lands
/// after that separator so arguments follow.
func slashAccept(text: String, token: SlashToken, command: SlashCommand) -> SlashAccept {
    let chars = Array(text)
    let next = token.range.upperBound < chars.count ? chars[token.range.upperBound] : nil
    let existingSeparator = next.map { $0.isWhitespace && $0 != "\n" && $0 != "\r" } ?? false
    let inserted = existingSeparator ? "/\(command.name)" : "/\(command.name) "
    let updated = String(chars[..<token.range.lowerBound])
        + inserted
        + String(chars[token.range.upperBound...])
    let cursor = token.range.lowerBound + inserted.count + (existingSeparator ? 1 : 0)
    return SlashAccept(text: updated, cursor: cursor)
}

// MARK: - Catalog cache + popup state

/// One catalog per `(device, harness, cwd)` — §10.4/§10.6. Discovery is
/// cwd-scoped (project skills resolve relative to the run's directory), so the
/// cwd is part of the key: switching any component swaps the list, and the
/// previous key's entries are never shown under the new one.
struct SlashCatalogKey: Hashable {
    let deviceId: String
    let harness: String
    /// The chat's cwd for an existing chat, the picked space's path on the
    /// new-session canvas; nil probes from the engine's own directory.
    let cwd: String?
}

/// `ListCommands` params for one key: the harness always, `cwd` only when the
/// surface has one (§10.4 — an omitted `cwd` is the old caller's
/// engine-directory probe).
func slashListCommandsParams(harness: String, cwd: String?) -> [String: String] {
    var params = ["harness": harness]
    if let cwd { params["cwd"] = cwd }
    return params
}

/// What the popup renders (render_slash_popup's branches, in its order).
enum SlashPopup: Equatable {
    case hidden
    case loading
    case failed(String)
    /// The agent answered with an empty catalog.
    case noCommands
    /// The catalog has entries, none match the query.
    case noMatches
    case commands([SlashCommand])
}

/// The composer's slash state: the live token, the per-(device, harness, cwd)
/// catalogs, and which of them is loading or failed. `update` is the only
/// entry point on an edit; it returns the key whose catalog must be fetched.
struct SlashCommandsModel {
    private var catalogs: [SlashCatalogKey: [SlashCommand]] = [:]
    private var errors: [SlashCatalogKey: String] = [:]
    private var inFlight: Set<SlashCatalogKey> = []
    private var key: SlashCatalogKey?
    /// Token text hidden by an explicit dismiss — until the token changes.
    private var dismissed: String?

    private(set) var token: SlashToken?

    /// Track the token on every edit / caret move / harness or folder switch.
    /// Returns the key needing a `ListCommands` (nil = cached, in flight, or
    /// nothing open).
    mutating func update(text: String, cursor: Int, key: SlashCatalogKey?) -> SlashCatalogKey? {
        let token = slashToken(text, cursor: cursor)
        if let token, let dismissed, slashTokenText(text, token) == dismissed {
            self.token = nil
            return nil
        }
        dismissed = nil
        let keyChanged = self.key != key
        self.key = key
        if token == self.token, !keyChanged { return nil }
        self.token = token
        guard token != nil, let key else { return nil }
        if catalogs[key] != nil || inFlight.contains(key) { return nil }
        // First open for this key: one probe, and a retry on the next open
        // after a failure (the desktop clears its error the same way).
        errors[key] = nil
        inFlight.insert(key)
        return key
    }

    mutating func received(_ commands: [SlashCommand], for key: SlashCatalogKey) {
        inFlight.remove(key)
        catalogs[key] = commands
    }

    mutating func failed(_ message: String, for key: SlashCatalogKey) {
        inFlight.remove(key)
        errors[key] = message
    }

    /// Hide the popup for this exact token; any edit to it opens again.
    mutating func dismiss(in text: String) {
        guard let token else { return }
        dismissed = slashTokenText(text, token)
        self.token = nil
    }

    /// Replace the token with `/name `, closing the popup.
    mutating func accept(_ command: SlashCommand, in text: String) -> SlashAccept? {
        guard let token else { return nil }
        self.token = nil
        dismissed = nil
        return slashAccept(text: text, token: token, command: command)
    }

    var popup: SlashPopup {
        guard let token else { return .hidden }
        let commands = key.flatMap { catalogs[$0] } ?? []
        if let key, inFlight.contains(key), commands.isEmpty { return .loading }
        if let key, let message = errors[key] { return .failed(message) }
        let filtered = slashFilterIndices(query: token.query, commands: commands)
        if filtered.isEmpty { return commands.isEmpty ? .noCommands : .noMatches }
        return .commands(filtered.map { commands[$0] })
    }
}
