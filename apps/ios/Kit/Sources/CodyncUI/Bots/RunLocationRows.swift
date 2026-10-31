import CodyncKit
import SwiftUI

/// Where a bot runs, in the Agent card: this computer's own agent, or Hermes on a gateway (the user's
/// VPS). A gateway bot always runs as Hermes there, in its own profile.
struct RunLocationRows: View {
    @Binding var draft: BotDraft
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var cloud: Bool { draft.isCloud }
    private var connections: [HermesConnection] { model.connections }

    var body: some View {
        ValueRow("Runs on", detail: cloud
                 ? "On the gateway, in its own Hermes profile. It works on that computer, not this one."
                 : "As an agent process on this computer.") {
            if connections.isEmpty {
                Text(L10n.tr("This computer")).foregroundStyle(Palette.secondary)
            } else {
                ChoicePicker(selection: Binding(get: { cloud ? "cloud" : "local" },
                                                set: { $0 == "cloud" ? toCloud() : toLocal() }),
                             options: [("local", "This computer"), ("cloud", "Gateway")])
            }
        }
        if cloud, !connections.isEmpty {
            ValueRow("Gateway", detail: connections.count == 1 ? "Add another in Settings on the computer." : nil) {
                ChoicePicker(selection: Binding(get: { draft.connection ?? connections[0].id },
                                                set: { draft.connection = $0; draft.model = nil }),
                             options: connections.map { ($0.id, $0.name) })
            }
            ValueRow("Profile", detail: "One profile per bot keeps its memory, skills and sessions apart. Codync makes it on the gateway the first time the bot needs it.") {
                HStack(spacing: 8) {
                    Text(profileName ?? automatic).font(.caption).foregroundStyle(Palette.tertiary).lineLimit(1)
                    TextField(L10n.tr("Automatic"), text: Binding(get: { draft.hermesProfile ?? "" },
                                                                  set: { draft.hermesProfile = $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0.lowercased() }))
                        .font(.caption.monospaced())
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
            }
        } else if cloud {
            HStack(spacing: 8) {
                Image(systemName: "cloud").font(.caption).foregroundStyle(Palette.secondary)
                Text(L10n.tr("Add your VPS in Settings on the computer to run bots there."))
                    .font(.caption).foregroundStyle(Palette.secondary)
            }
        }
    }

    /// The profile's name, or the host's from the bot's id (unknown until it's saved).
    private var profileName: String? {
        guard let named = draft.hermesProfile?.trimmingCharacters(in: .whitespaces), !named.isEmpty else { return nil }
        return hermesProfileOf(id: draft.id ?? "", named: named)
    }

    private var automatic: String { draft.id.map { hermesProfileOf(id: $0, named: nil) } ?? "codync-<bot id>" }

    private func toCloud() {
        guard let first = connections.first else { return }
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
            draft.connection = draft.connection ?? first.id
            draft.backend = "hermes"
            draft.model = nil
            draft.modelOptions = [:]
        }
    }

    private func toLocal() {
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
            draft.connection = nil
            // Back to this computer's Hermes, or Claude when Hermes isn't installed here.
            if draft.backend == "hermes", model.hello?.backends.first(where: { $0.id == "hermes" })?.available != true {
                draft.backend = model.hello?.backends.first(where: { $0.available && $0.curated == true })?.id ?? "claude"
            }
            draft.model = nil
            draft.modelOptions = [:]
        }
    }
}

/// A gateway bot's model: what the gateway's profile offers (`/v1/models`), never this computer's agents.
struct GatewayModelRow: View {
    @Binding var draft: BotDraft
    @Environment(BotStore.self) private var model
    @State private var models: [String] = []
    @State private var failure: String?
    @State private var loading = false

    private var profile: String { hermesProfileOf(id: draft.id ?? "", named: draft.hermesProfile) }

    var body: some View {
        ValueRow("Model", detail: failure ?? "Which model the gateway's profile answers with.") {
            if loading {
                Spinner()
            } else if !models.isEmpty {
                ChoicePicker(selection: Binding(get: { draft.model ?? "" }, set: { draft.model = $0.isEmpty ? nil : $0 }),
                             options: [("", L10n.tr("Default"))] + models.map { ($0, $0) })
            } else {
                Text(failure == nil ? L10n.tr("Default") : L10n.tr("The gateway didn't answer"))
                    .foregroundStyle(failure == nil ? Palette.secondary : Palette.warning)
            }
        }
        .task(id: "\(draft.connection ?? "")/\(profile)") {
            models = []
            failure = nil
            guard let connection = draft.connection, let client = model.client else { return }
            loading = true
            defer { loading = false }
            do {
                models = try await client.testConnection(id: connection, profile: profile).models
            } catch {
                failure = error.localizedDescription
            }
        }
    }
}
