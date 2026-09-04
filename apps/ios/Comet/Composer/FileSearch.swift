// File-search wire contract — the phone half of docs/composer-completions.md
// §4. Pure types and one error mapper; no views, no harness identity. The
// engine owns the walk, the ranking, and the root check (`SearchFiles`).

import Foundation

// MARK: - Wire

/// One hit of `SearchFiles` (`comet_proto::FileSearchMatch`, camelCase on the
/// wire): a checkout-relative `/`-separated path, directories flagged.
struct FileSearchMatch: Decodable, Hashable, Identifiable {
    let path: String
    let isDir: Bool

    var id: String { isDir ? path + "/" : path }

    init(path: String, isDir: Bool) {
        self.path = path
        self.isDir = isDir
    }

    private enum CodingKeys: String, CodingKey {
        case path, isDir
    }

    init(from decoder: Decoder) throws {
        let row = try decoder.container(keyedBy: CodingKeys.self)
        path = try row.decode(String.self, forKey: .path)
        isDir = try row.decodeIfPresent(Bool.self, forKey: .isDir) ?? false
    }

    /// Popup row halves (`render_file_mention_popup`): the directory and the
    /// file name, with a top-level entry having an empty directory.
    var directory: String {
        guard let cut = path.lastIndex(of: "/") else { return "" }
        return String(path[..<cut])
    }

    var name: String {
        guard let cut = path.lastIndex(of: "/") else { return path }
        return String(path[path.index(after: cut)...])
    }
}

/// Where a search walks (§4.1): an existing chat's checkout, or a picked
/// space's folder — optionally an existing linked worktree the new chat will
/// reuse. The engine verifies both against rows it hosts.
enum FileSearchScope: Hashable {
    case chat(chatId: String)
    case space(spaceId: String, path: String?)
}

/// `SearchFiles` params for one scope. The relay already addresses one
/// device, so no `targetDeviceId` rides along (same as `ListCommands`).
func fileSearchParams(query: String, scope: FileSearchScope) -> [String: String] {
    var params = ["query": query]
    switch scope {
    case .chat(let chatId):
        params["chatId"] = chatId
    case .space(let spaceId, let path):
        params["spaceId"] = spaceId
        if let path { params["path"] = path }
    }
    return params
}

/// A search's outcome: the engine's ranked hits, or why it couldn't answer
/// (the popup's error row, already mapped by `completionErrorMessage`).
enum FileSearchResult: Equatable {
    case matches([FileSearchMatch])
    case failure(String)
}

// MARK: - Error mapping (§4.5)

/// Which popup a failure is reported for — the copy differs, the cases don't.
enum CompletionAction {
    case searchFiles
    case listCommands
}

/// composer.rs `mention_error_message` / `slash_error_message` over the relay's
/// error shape. "unknown method" is the version-skew case: the host daemon
/// predates the RPC while the same feature works against a newer device.
func completionErrorMessage(_ error: Error, action: CompletionAction) -> String {
    let unreachable = "The session's device is unreachable"
    let outdated: String
    let failed: String
    switch action {
    case .searchFiles:
        outdated = "The session's device runs an older comet — update it to search its files"
        failed = "File search failed"
    case .listCommands:
        outdated = "The session's device runs an older comet — update it to list commands"
        failed = "Couldn't load this agent's commands"
    }
    guard let relay = error as? RelayError else { return failed }
    switch relay {
    case .hostOffline, .notConnected, .timeout:
        return unreachable
    case .rpc(let message):
        return message.lowercased().hasPrefix("unknown method") ? outdated : failed
    }
}
