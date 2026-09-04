// Composer — the floating glass shell in the t3 mobile composer's shape: a
// collapsed capsule (editor + send circle) that morphs into an expanded card
// with a toolbar ROW below it (attach circle · scrolling chips · pinned send)
// when the editor takes focus. Carries the desktop's Send→Steer→Stop
// semantics: live run + text = steer (same up-arrow), live run + empty = stop.
//
// Expansion is focus-driven like t3's, with the old deterministic content
// triggers kept as a floor (attachments, newline, >26 chars) — content-size
// measurement oscillates at the boundary, so it is never measured.

import PhotosUI
import SwiftUI

/// Shared glass shell + input + action row. `chips` render in the expanded
/// toolbar row between the attach and send circles.
struct ComposerShell<Chips: View>: View {
    @Binding var draft: AttributedString
    var placeholder = "Message"
    var sendEnabled: Bool
    var showStop: Bool
    var busy = false
    /// New-session composers stay expanded — the picker chips ARE the page.
    var alwaysExpanded = false
    /// Hold the expanded layout while a picker sheet is up: presenting the
    /// sheet blurs the editor, and collapsing on that blur flaps the
    /// transcript's bottom inset mid-presentation (t3 derives expansion from
    /// focus OR sheet-active for exactly this reason).
    var keepExpanded = false
    /// Caret and range: both completion grammars read it as a character offset
    /// and chip atomicity snaps it (composer.rs reads the same cursor —
    /// docs/composer-completions.md §2, §5.3).
    var selection: Binding<AttributedTextSelection>
    var onSend: () -> Void
    var onStop: () -> Void = {}
    /// Staged image attachments (attachment-ui.tsx AttachmentStrip inside the
    /// pill). Non-empty forces the expanded layout, like focus.
    var attachments: [StagedAttachment] = []
    /// Present the photo picker; nil hides the attach button.
    var onAttach: (() -> Void)? = nil
    var onRemoveAttachment: (String) -> Void = { _ in }
    /// Screenshot rig (-focuscomposer): take keyboard focus shortly after
    /// appearing, so the keyboard-up transcript states can be driven headless.
    var autoFocus = false
    @ViewBuilder var chips: Chips

    @FocusState private var focused: Bool

    private var expanded: Bool {
        alwaysExpanded || keepExpanded || focused || !attachments.isEmpty
            || draft.characters.contains("\n") || draft.characters.count > 26
    }

    /// One animatable shape for background/glass/hairline: capsule-radius
    /// collapsed (46pt tall pill), 20pt card expanded (t3's 999↔20 morph).
    private var surfaceShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: expanded ? 20 : 24)
    }

    // Switching between VStack/HStack via AnyLayout (rather than an if/else
    // that swaps container types) keeps `input`'s view identity stable across
    // the compact↔expanded flip — an if/else here would tear down and rebuild
    // the TextField, dropping keyboard focus mid-type.
    private var shellLayout: AnyLayout {
        expanded
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 0))
            : AnyLayout(HStackLayout(alignment: .center, spacing: 12))
    }

    var body: some View {
        surface
            // Focus-widen: margins pull in slightly while typing (chat-session.tsx).
            .padding(.horizontal, focused ? 10 : 16)
            .motionAnimation(Motion.resize, value: focused)
            .motionAnimation(Motion.collapse, value: expanded)
            .onAppear {
                guard autoFocus else { return }
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    focused = true
                }
            }
    }

    /// The glass surface: collapsed = editor + send in one capsule row;
    /// expanded = attachment strip over a tall editor, with the control row
    /// (attach circle · scrolling chips · pinned send) along the card's
    /// bottom edge — everything stays inside the glass.
    private var surface: some View {
        shellLayout {
            if expanded, !attachments.isEmpty {
                AttachmentStripView(attachments: attachments, remove: onRemoveAttachment)
                    .padding(.bottom, 10)
            }
            input
                .padding(.leading, expanded ? 4 : 13)
                .padding(.trailing, expanded ? 4 : 0)
                .padding(.vertical, expanded ? 4 : 5)
                .frame(minHeight: expanded ? 64 : nil, alignment: .topLeading)
            if expanded {
                HStack(spacing: 8) {
                    if onAttach != nil {
                        attachButton
                    }
                    // Chips scroll; the send button stays pinned.
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            chips
                        }
                    }
                    .scrollClipDisabled(false)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    actionButton
                }
                .padding(.top, 8)
            } else {
                actionButton
            }
        }
        .padding(.horizontal, expanded ? 12 : 5)
        .padding(.vertical, expanded ? 12 : 5)
        .background(whiteAlpha(0.04), in: surfaceShape)
        .glassEffect(.regular.interactive(), in: surfaceShape)
        .overlay(surfaceShape.strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        // The whole glass surface focuses the editor, not just the TextField's
        // own text box: the collapsed pill is mostly padding, and a tap that
        // misses the text box falls through to the transcript underneath —
        // whose tap-to-blur then RESIGNS the keyboard. That's the "have to
        // press a few times" miss. Buttons and chips still win their taps.
        // Masked to .subviews while focused so cursor-placement taps inside
        // the editor stay fully native.
        .contentShape(surfaceShape)
        .gesture(TapGesture().onEnded { focused = true },
                 including: focused ? .subviews : .all)
    }

    private var input: some View {
        ComposerEditor(text: $draft, selection: selection, placeholder: placeholder,
                       focus: $focused)
    }

    private var attachButton: some View {
        Button {
            onAttach?()
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 40, height: 40)
                .background(whiteAlpha(0.06), in: Circle())
                .overlay(Circle().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(busy)
    }

    /// Attachments count as content: an image-only send is a send, never a stop.
    private var hasContent: Bool {
        !String(draft.characters).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !attachments.isEmpty
    }

    private var actionButton: some View {
        Button {
            if showStop, !hasContent {
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                onStop()
            } else {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onSend()
            }
        } label: {
            Group {
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(Theme.bg)
                } else if showStop, !hasContent {
                    RoundedRectangle(cornerRadius: 3.5)
                        .fill(Theme.bg)
                        .frame(width: 12, height: 12)
                } else {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(buttonActive ? Theme.bg : Theme.textFaint)
                }
            }
            .frame(width: 40, height: 40)
            .background(buttonActive ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.10)),
                        in: Circle())
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!buttonActive)
        .motionAnimation(Motion.fadeQuick, value: showStop)
    }

    private var buttonActive: Bool {
        if showStop, !hasContent { return true }
        return sendEnabled && hasContent && !busy
    }
}

// MARK: - Editor (docs/composer-completions.md §9)

/// The shell's text view: a `TextEditor` over the attributed draft, because on
/// iOS 26 only `TextEditor` binds an `AttributedString` together with a
/// selection — which is what chips need (§2). Chip paint comes from
/// `MentionFormatting`, never from here.
private struct ComposerEditor: View {
    @Binding var text: AttributedString
    @Binding var selection: AttributedTextSelection
    let placeholder: String
    let focus: FocusState<Bool>.Binding

    /// UITextView's own chrome, which the `TextField` this replaced did not
    /// have: 8pt of vertical `textContainerInset` and 5pt of
    /// `lineFragmentPadding` per side. The editor keeps them (the caret is
    /// drawn inside them) and the layout takes them straight back out, so the
    /// collapsed pill and the expanded card keep the geometry they had around
    /// the field. Residual difference: the glyphs sit ~1pt lower than the
    /// field's did, because a text view centers a line in its line height.
    private static let insetX: CGFloat = 5
    private static let insetY: CGFloat = 8
    /// Grow with the text to 7 lines, then scroll inside — the cap the old
    /// field's `lineLimit(1...7)` had (the desktop stops at 260px and scrolls).
    private static let maxLines: CGFloat = 7

    /// `TextEditor` is greedy vertically, so the height is measured off hidden
    /// mirrors laid out at the editor's own text width. `Text` and TextKit can
    /// disagree by a line on a pathological wrap; the editor scrolls, so the
    /// caret stays reachable either way.
    @State private var contentHeight: CGFloat = 0
    @State private var lineHeight: CGFloat = 0

    private var height: CGFloat {
        let capped = lineHeight > 0 ? min(contentHeight, lineHeight * Self.maxLines) : contentHeight
        return capped.rounded(.up) + Self.insetY * 2
    }

    var body: some View {
        TextEditor(text: $text, selection: $selection)
            .attributedTextFormattingDefinition(MentionFormatting())
            .font(Theme.sans(16))
            .foregroundStyle(Theme.text)
            .tint(Theme.text)
            .scrollContentBackground(.hidden)
            .frame(height: height)
            .padding(.horizontal, -Self.insetX)
            .padding(.vertical, -Self.insetY)
            .background(alignment: .topLeading) { rulers }
            .overlay(alignment: .topLeading) {
                if text.characters.isEmpty {
                    Text(placeholder)
                        .font(Theme.sans(16))
                        .foregroundStyle(Theme.textFaint)
                        .allowsHitTesting(false)
                }
            }
            .focused(focus)
    }

    /// The draft and one empty line, laid out at the editor's text width: their
    /// heights are the growth curve and its cap.
    private var rulers: some View {
        var mirrored = text
        // `Text` drops a trailing newline, so a just-typed Return would not
        // grow the editor without this.
        mirrored.append(AttributedString(emptyLine))
        return ZStack(alignment: .topLeading) {
            ruler(mirrored)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { contentHeight = $0 }
            ruler(AttributedString(emptyLine))
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { lineHeight = $0 }
        }
    }

    private func ruler(_ string: AttributedString) -> some View {
        Text(string)
            .font(Theme.sans(16))
            .fixedSize(horizontal: false, vertical: true)
            .hidden()
    }

    /// Zero-width space: makes a line exist without taking width.
    private var emptyLine: String { "\u{200B}" }
}

// MARK: - Slash commands (composer.rs render_slash_popup)

/// The `/` completion popup above the pill: one row per advertised command,
/// filtered by the typed token. Keyboard-less — a tap accepts; the card closes
/// with its token (§2.3).
struct SlashCommandPopup: View {
    let state: SlashPopup
    let accept: (SlashCommand) -> Void

    var body: some View {
        Group {
            if case .hidden = state {
                EmptyView()
            } else {
                CompletionPopupCard { content }
            }
        }
        .motionAnimation(Motion.menuIn, value: state)
    }

    @ViewBuilder
    private var content: some View {
        switch state {
        case .hidden:
            EmptyView()
        case .loading:
            HStack(spacing: 8) {
                MiniSpinner(cellSize: 2.5)
                completionNotice("Loading commands…")
            }
            .padding(.leading, 14)
        case .failed(let message):
            completionNotice(message, tone: Theme.danger)
        case .noCommands:
            completionNotice("This agent has no slash commands")
        case .noMatches:
            completionNotice("No matching commands")
        case .commands(let commands):
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(commands) { row($0) }
                }
            }
            .frame(maxHeight: 220)
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private func row(_ command: SlashCommand) -> some View {
        Button {
            accept(command)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(command.title)
                    .font(Theme.sans(13.5, weight: .medium))
                    .foregroundStyle(Theme.text)
                    .fixedSize()
                Text(command.detail)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - File mentions (composer.rs render_file_mention_popup)

/// The `@` completion popup above the pill: the ranked hits of the session's
/// checkout (§4.3/§4.4), icon · file name · muted directory. A tap accepts and
/// inserts the chip; the card closes with its token (§2.3).
struct FileMentionPopup: View {
    let state: MentionPopup
    let accept: (FileSearchMatch) -> Void

    var body: some View {
        Group {
            if case .hidden = state {
                EmptyView()
            } else {
                CompletionPopupCard { content }
            }
        }
        .motionAnimation(Motion.menuIn, value: state)
    }

    @ViewBuilder
    private var content: some View {
        switch state {
        case .hidden:
            EmptyView()
        case .loading:
            HStack(spacing: 8) {
                MiniSpinner(cellSize: 2.5)
                completionNotice("Searching files…")
            }
            .padding(.leading, 14)
        case .failed(let message):
            completionNotice(message, tone: Theme.danger)
        case .noFiles:
            completionNotice("No files available")
        case .noMatches:
            completionNotice("No matching files")
        case .matches(let matches):
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(matches) { row($0) }
                }
            }
            .frame(maxHeight: 220)
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private func row(_ match: FileSearchMatch) -> some View {
        Button {
            accept(match)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: match.isDir ? "folder" : "doc")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.textMuted)
                Text(match.name)
                    .font(Theme.sans(13.5, weight: .medium))
                    .foregroundStyle(Theme.text)
                    .fixedSize()
                if !match.directory.isEmpty {
                    Text(match.directory)
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Shared popup chrome

/// The chrome both completion cards wear: a glass panel above the pill whose
/// rows start at the top edge. There is no dismiss control: the card closes
/// when its token does (§2.3), the way the desktop's closes on click-away.
struct CompletionPopupCard<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 20))
        .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        .padding(.horizontal, 16)
        .transition(.opacity)
    }
}

/// A popup's one-line state row (loading caption, error, empty state).
func completionNotice(_ text: String, tone: Color = Theme.textMuted) -> some View {
    Text(text)
        .font(Theme.sans(12))
        .foregroundStyle(tone)
        .padding(.horizontal, 14)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
}

// MARK: - Sync + accept, shared by both composers

/// Both composers drive the `/` popup the same way: re-read the token on every
/// edit / caret move / harness or folder switch, and probe `ListCommands` once
/// per open of a (device, harness, cwd) key — the model decides which opens
/// need one (§10.4 "Freshness").
@MainActor
func syncSlash(_ slash: Binding<SlashCommandsModel>, text: String, cursor: Int,
               key: SlashCatalogKey?, model: AppModel) {
    guard let fetch = slash.wrappedValue.update(text: text, cursor: cursor, key: key)
    else { return }
    Task { @MainActor in
        switch await model.listCommands(deviceId: fetch.deviceId, harness: fetch.harness,
                                        cwd: fetch.cwd) {
        case .commands(let commands): slash.wrappedValue.received(commands, for: fetch)
        case .failure(let message): slash.wrappedValue.failed(message, for: fetch)
        }
    }
}

/// Accept: the token becomes `/name `, the caret lands after it so arguments
/// follow, and the prompt still sends as plain text.
@MainActor
func acceptSlash(_ slash: Binding<SlashCommandsModel>, _ command: SlashCommand,
                 draft: Binding<MentionDraft>, selection: Binding<AttributedTextSelection>) {
    guard let replacement = slash.wrappedValue.acceptReplacement(
        command, in: draft.wrappedValue.display) else { return }
    selection.wrappedValue = draft.wrappedValue.apply(replacement)
}

/// Both composers drive the `@` popup the same way: re-read the token on every
/// edit / caret move, then send at most one `SearchFiles` per token, 80 ms
/// after it settled (§4.2 — the generation guard drops a reply the next token
/// already outran). A surface with no chat and no space never sends one.
@MainActor
func syncMentionSearch(_ mentions: Binding<FileMentionsModel>, task: Binding<Task<Void, Never>?>,
                       text: String, cursor: Int,
                       search: (deviceId: String, scope: FileSearchScope)?, model: AppModel) {
    guard let request = mentions.wrappedValue.update(text: text, cursor: cursor) else { return }
    task.wrappedValue?.cancel()
    guard let search else {
        mentions.wrappedValue.searchUnavailable()
        return
    }
    task.wrappedValue = Task { @MainActor in
        try? await Task.sleep(for: .milliseconds(80))
        guard !Task.isCancelled, !mentions.wrappedValue.isStale(request.generation) else { return }
        switch await model.searchFiles(deviceId: search.deviceId, scope: search.scope,
                                       query: request.query) {
        case .matches(let matches):
            mentions.wrappedValue.received(matches, generation: request.generation)
        case .failure(let message):
            mentions.wrappedValue.failed(message, generation: request.generation)
        }
    }
}

/// Accept: the token becomes one chip run carrying the mention, and the caret
/// lands after the separator so prose follows (§2.2, §5).
@MainActor
func acceptMention(_ mentions: Binding<FileMentionsModel>, _ match: FileSearchMatch,
                   draft: Binding<MentionDraft>, selection: Binding<AttributedTextSelection>) {
    guard let (replacement, chip) = mentions.wrappedValue.accept(
        match, in: draft.wrappedValue.display) else { return }
    selection.wrappedValue = draft.wrappedValue.insertMention(replacement, chip: chip)
}

/// The live-chat composer: input, the photo attach button, the model + trait
/// picker chips (harness stays locked mid-chat; picks merge into the chat's
/// config row for the next dispatch), and the morphing action button.
struct ComposerView: View {
    @Environment(AppModel.self) private var model
    let store: SessionStore
    let chat: Chat
    let runLive: Bool

    @State private var draft = MentionDraft()
    @State private var selection = AttributedTextSelection()
    /// `/` completion, fed by ListCommands on the chat's device (§10.2).
    @State private var slash = SlashCommandsModel()
    /// `@` completion, fed by SearchFiles over the chat's checkout (§4.1).
    @State private var mentions = FileMentionsModel()
    /// The in-flight search, held so the 80 ms debounce can cancel it (§4.2).
    @State private var mentionSearch: Task<Void, Never>?
    @State private var attachments: [StagedAttachment] = []
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var showPicker = false
    @State private var uploading = false
    @State private var uploadProgress: Double?
    @State private var uploadError: String?
    @State private var showModelPicker = false
    @State private var showTraitPicker = false
    /// Live catalog for the chat's harness from its space's device.
    @State private var catalogs: [String: [ModelInfo]] = [:]

    private var harness: String { chat.config?.harness ?? "claude-code" }

    private var models: [ModelInfo] {
        catalogs[harness] ?? HarnessCatalog.models(for: harness)
    }

    private var currentModel: ModelInfo {
        models.first { $0.id == chat.config?.model } ?? HarnessCatalog.defaultModel(for: harness)
    }

    private var currentReasoning: String? {
        guard !currentModel.reasoningLevels.isEmpty else { return nil }
        if let r = chat.config?.reasoning, currentModel.reasoningLevels.contains(r) { return r }
        return HarnessCatalog.defaultReasoning(for: currentModel)
    }

    var body: some View {
        // Subscribe to the connectivity pulse so the degraded caption follows
        // the graced stream (1Hz only while something is degraded/pending).
        let _ = model.connectivity.pulse
        return VStack(spacing: 6) {
            if let uploadError {
                Text(uploadError)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.danger)
                    .lineLimit(2)
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if uploading, let uploadProgress {
                Text("Uploading… \(Int(uploadProgress * 100))%")
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textMuted)
                    .monospacedDigit()
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Pre-send honesty (composer.rs degraded notice, post-#170 copy):
            // one quiet caption line, never a warning box — the send still
            // works, it just queues.
            if model.chatDeliveryDegraded(chat) {
                Text(model.connectivity.state == .offline
                    ? "Offline — messages will send when you're back online."
                    : "Messages will send once the connection recovers.")
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textFaint)
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.opacity)
            }
            // One popup slot: slash wins when both tokens could match, exactly
            // as the desktop event router does (§2.1).
            if slash.popup != .hidden {
                SlashCommandPopup(
                    state: slash.popup,
                    accept: { acceptSlash($slash, $0, draft: $draft, selection: $selection) }
                )
            } else {
                FileMentionPopup(
                    state: mentions.popup,
                    accept: { acceptMention($mentions, $0, draft: $draft, selection: $selection) }
                )
            }
            ComposerShell(
                draft: $draft.text,
                sendEnabled: true,
                showStop: runLive,
                busy: uploading,
                keepExpanded: showModelPicker || showTraitPicker,
                selection: $selection,
                onSend: send,
                onStop: { store.sendInterrupt() },
                attachments: attachments,
                onAttach: { showPicker = true },
                onRemoveAttachment: { id in attachments.removeAll { $0.id == id } },
                autoFocus: model.launchFocusComposer
            ) {
                if let pullRequest = model.changeRequest(for: chat) {
                    PullRequestBadge(summary: pullRequest, surface: .composer)
                }
                if let branch = chat.branch?.trimmingCharacters(in: .whitespacesAndNewlines),
                   !branch.isEmpty {
                    BranchContextChip(branch: branch)
                }
                ComposerChip(label: currentModel.label, badgeHarness: harness) {
                    showModelPicker = true
                }
                if let currentReasoning {
                    ComposerChip(label: HarnessCatalog.reasoningLabel(currentReasoning)) {
                        showTraitPicker = true
                    }
                }
            }
        }
        .photosPicker(isPresented: $showPicker, selection: $pickerItems,
                      maxSelectionCount: 8, matching: .images)
        .onChange(of: pickerItems) { _, items in
            guard !items.isEmpty else { return }
            stage(items)
        }
        .sheet(isPresented: $showModelPicker) {
            ModelPickerSheet(
                harness: .constant(harness),
                modelId: Binding(
                    get: { currentModel.id },
                    set: { writeConfig(model: $0, reasoning: chat.config?.reasoning) }
                ),
                reasoning: Binding(
                    get: { chat.config?.reasoning },
                    set: { writeConfig(model: chat.config?.model, reasoning: $0) }
                ),
                lockedHarness: true,
                catalogs: catalogs
            )
        }
        .sheet(isPresented: $showTraitPicker) {
            TraitPickerSheet(
                reasoning: Binding(
                    get: { currentReasoning },
                    set: { writeConfig(model: chat.config?.model, reasoning: $0) }
                ),
                levels: currentModel.reasoningLevels
            )
        }
        .task(id: "\(chat.id)/\(harness)") {
            guard let space = model.space(for: chat) else { return }
            catalogs[harness] = await model.listModels(space: space, harness: harness)
        }
        .onChange(of: draft.text) {
            // §5.3: a run an edit broke is removed whole before anything reads
            // the draft. The write re-fires this hook, which then settles.
            if draft.reconcile() { return }
            syncSlashCommands()
            syncMentions()
        }
        .onChange(of: selection) {
            // Chip atomicity first (§5.3): the snap re-fires this hook.
            if let snapped = draft.snapped(selection) {
                selection = snapped
                return
            }
            syncSlashCommands()
            syncMentions()
        }
        .onChange(of: harness) { syncSlashCommands() }
        .onAppear {
            if model.launchSheet == "config" {
                model.launchSheet = nil
                showModelPicker = true
            }
        }
    }

    /// The catalog is the chat's own harness on the chat's own device, probed
    /// in the chat's own folder (§10.4). A chat with no config row has no
    /// harness to ask for — the skill layer never falls back to a default one
    /// (ARCHITECTURE.md §10.3), so there is simply no key.
    private func syncSlashCommands() {
        syncSlash($slash, text: draft.display, cursor: draft.caret(of: selection),
                  key: chat.config.map {
                      SlashCatalogKey(deviceId: chat.deviceId, harness: $0.harness, cwd: chat.cwd)
                  },
                  model: model)
    }

    /// Mentions search the chat's own checkout on the chat's own device (§4.1).
    private func syncMentions() {
        syncMentionSearch($mentions, task: $mentionSearch, text: draft.display,
                          cursor: draft.caret(of: selection),
                          search: (chat.deviceId, .chat(chatId: chat.id)), model: model)
    }

    /// Merge a model/effort change into the chat's config row (LWW; the host
    /// picks it up on the next run dispatch). Copies preserve modelOptions.
    private func writeConfig(model newModel: String?, reasoning newReasoning: String?) {
        var config = chat.config ?? ChatConfig(harness: harness, model: nil,
                                               reasoning: nil, sandbox: "workspace-write")
        config.model = newModel
        config.reasoning = newReasoning
        model.setChatConfig(chatId: chat.id, config: config)
    }

    /// Load picked photos into staged attachments (HEIC transcodes to JPEG;
    /// unsupported/oversized picks surface as an error line).
    private func stage(_ items: [PhotosPickerItem]) {
        Task { @MainActor in
            var failed = 0
            for item in items {
                guard let data = try? await item.loadTransferable(type: Data.self),
                      let staged = StagedAttachment.stage(data: data) else {
                    failed += 1
                    continue
                }
                attachments.append(staged)
            }
            pickerItems = []
            if failed > 0 {
                uploadError = failed == 1
                    ? "One image couldn't be attached (unsupported or over 24 MB)."
                    : "\(failed) images couldn't be attached (unsupported or over 24 MB)."
            } else {
                uploadError = nil
            }
        }
    }

    private func send() {
        // Chips become their Markdown links here and nowhere else (§6); the
        // attachments trailer and the steer/run split compose with them
        // unchanged.
        let prompt = draft.serialized().trimmingCharacters(in: .whitespacesAndNewlines)
        let staged = attachments
        guard !prompt.isEmpty || !staged.isEmpty else { return }

        if staged.isEmpty {
            deliver(content: prompt, paths: [])
            clearDraft()
            return
        }
        if model.hostSupportsQueuedAttachments(chat) {
            // Queued flow (host ≥ 0.2.12): the send is a durable local write
            // NOW — pending:// refs in the doc, bytes escorted afterwards
            // with retry-forever — so an image send survives a dead link
            // exactly like a text send does.
            let transfers = staged.map {
                AttachmentTransfer(uploadId: UUID().uuidString.lowercased(),
                                   name: $0.name, data: $0.data)
            }
            for (transfer, att) in zip(transfers, staged) {
                // Seed under the pending ref — the echo's thumbnail renders
                // from local bytes while the upload crosses the relay.
                AttachmentImageCache.shared.seed(
                    deviceId: chat.deviceId,
                    path: UploadStash.pendingRef(uploadId: transfer.uploadId, name: transfer.name),
                    name: att.name, data: att.data)
            }
            store.sendWithTransfers(prompt: prompt, chat: chat, live: runLive,
                                    transfers: transfers)
            attachments = []
            clearDraft()
            return
        }
        // Legacy host-staged flow (host < 0.2.12): upload first, send after —
        // the refs trailer needs the committed paths. Bounded by the
        // whole-attachment deadline; progress narrates instead of a bare
        // spinner (a lawful crawl must never read as a hang).
        uploading = true
        uploadError = nil
        uploadProgress = 0
        let progressBinding = $uploadProgress
        Task { @MainActor in
            defer {
                uploading = false
                uploadProgress = nil
            }
            do {
                var paths: [String] = []
                let total = staged.count
                for (ix, att) in staged.enumerated() {
                    let path = try await store.uploadAttachment(name: att.name, data: att.data) { fraction in
                        progressBinding.wrappedValue = (Double(ix) + fraction) / Double(total)
                    }
                    // Seed the cache so our own bubble renders from local
                    // bytes instead of a round-trip.
                    AttachmentImageCache.shared.seed(deviceId: chat.deviceId, path: path,
                                                     name: att.name, data: att.data)
                    paths.append(path)
                }
                deliver(content: withAttachments(text: prompt, paths: paths), paths: paths)
                attachments = []
                clearDraft()
            } catch {
                uploadError = "Attachment upload failed — \(error.localizedDescription)"
            }
        }
    }

    private func deliver(content: String, paths: [String]) {
        if runLive {
            store.sendSteer(prompt: content)
        } else {
            store.sendRun(prompt: content, chat: chat, attachments: paths)
        }
    }

    private func clearDraft() {
        draft = MentionDraft()
        // The clear above is unconditional, so a prompt left sitting in the
        // composer after a successful send is not this path failing to run —
        // it is the text view writing the pre-send string back. A focused
        // multiline text editor commits pending autocorrect/marked text through
        // the binding AFTER a programmatic change, which restores the prompt.
        // Re-clear once that has drained; a keystroke can't land inside the
        // same main-actor turn, so this can never eat real input.
        Task { @MainActor in draft = MentionDraft() }
    }
}

// MARK: - Question panel (composer.rs Wizard)

struct QuestionPanel: View {
    let requestId: String
    let questions: [UserInputQuestion]
    let respond: (String, [UserInputAnswer]) -> Void

    @State private var page = 0
    @State private var picked: [String: Set<String>] = [:]  // questionId → labels
    @State private var typed: [String: String] = [:]
    @State private var autoAdvanceTask: Task<Void, Never>?

    var body: some View {
        // `questions[min(page, count - 1)]` traps on an empty list (count - 1
        // is -1). A request whose questions fail to decode reaches here empty,
        // so this crashed the app on any session holding one.
        if questions.isEmpty {
            EmptyView()
        } else {
            panel(for: questions[min(max(page, 0), questions.count - 1)])
        }
    }

    private func panel(for question: UserInputQuestion) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(question.header.uppercased())
                    .font(Theme.sans(10.5, weight: .medium))
                    .kerning(1)
                    .foregroundStyle(Theme.textMuted.opacity(0.6))
                Spacer()
                if questions.count > 1 {
                    Text("\(page + 1)/\(questions.count)")
                        .font(Theme.sans(10))
                        .foregroundStyle(Theme.textMuted)
                        .padding(.horizontal, 6)
                        .frame(height: 20)
                        .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 6))
                }
            }

            Text(question.question)
                .font(Theme.sans(15, weight: .medium))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)

            if question.multiSelect == true {
                Text("Select one or more options.")
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
            }

            VStack(spacing: 4) {
                ForEach(Array(question.options.enumerated()), id: \.offset) { ix, option in
                    optionRow(question: question, ix: ix, option: option)
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Rectangle().fill(whiteAlpha(0.06)).frame(height: 1)
                TextField("Or type your own answer", text: Binding(
                    get: { typed[question.id] ?? "" },
                    set: { typed[question.id] = $0 }
                ))
                .font(Theme.sans(13))
                .foregroundStyle(Theme.text)
                .padding(.top, 6)
            }

            HStack {
                if page > 0 {
                    Button("Back") {
                        page -= 1
                    }
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                }
                Spacer()
                Button(page < questions.count - 1 ? "Next" : "Submit") {
                    advance()
                }
                .font(Theme.sans(13, weight: .medium))
                .foregroundStyle(Theme.bg)
                .padding(.horizontal, 16)
                .frame(height: 34)
                .background(Theme.text, in: Capsule())
                .opacity(canAdvance(question) ? 1 : 0.4)
                .disabled(!canAdvance(question))
            }
        }
        .padding(16)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 26))
        .overlay(RoundedRectangle(cornerRadius: 26).strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        .padding(.horizontal, 12)
        .transition(.opacity)
    }

    private func optionRow(question: UserInputQuestion, ix: Int, option: String) -> some View {
        let isPicked = (typed[question.id] ?? "").isEmpty
            && picked[question.id, default: []].contains(option)
        return Button {
            pick(question: question, option: option)
        } label: {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(option)
                        .font(Theme.sans(13.5, weight: .medium))
                        .foregroundStyle(Theme.text)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
                if ix < 9 {
                    Text("\(ix + 1)")
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 22, height: 22)
                        .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 6))
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(isPicked ? whiteAlpha(0.09) : whiteAlpha(0.025),
                        in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12)
                .strokeBorder(isPicked ? whiteAlpha(0.16) : .clear, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    private func pick(question: UserInputQuestion, option: String) {
        typed[question.id] = nil
        if question.multiSelect == true {
            var set = picked[question.id, default: []]
            if set.contains(option) { set.remove(option) } else { set.insert(option) }
            picked[question.id] = set
        } else {
            picked[question.id] = [option]
            // Single-select auto-advances after 220ms (AUTO_ADVANCE_MS).
            autoAdvanceTask?.cancel()
            autoAdvanceTask = Task {
                try? await Task.sleep(nanoseconds: 220_000_000)
                guard !Task.isCancelled else { return }
                advance()
            }
        }
    }

    private func canAdvance(_ question: UserInputQuestion) -> Bool {
        !(typed[question.id] ?? "").isEmpty || !picked[question.id, default: []].isEmpty
    }

    private func advance() {
        let question = questions[min(page, questions.count - 1)]
        guard canAdvance(question) else { return }
        if page < questions.count - 1 {
            page += 1
            return
        }
        let answers = questions.map { q -> UserInputAnswer in
            let typedAnswer = (typed[q.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if !typedAnswer.isEmpty {
                return UserInputAnswer(questionId: q.id, labels: [typedAnswer])
            }
            return UserInputAnswer(questionId: q.id, labels: Array(picked[q.id, default: []]))
        }
        respond(requestId, answers)
    }
}
