import CodyncKit
import CodyncUI
import SwiftUI

/// The Hermes gateways on the user's other computers (the cloud bots' side), read-only: where each one is,
/// the profile its plain routes serve, whether it answered recently, and which bots run on it. Gateways are
/// added and their keys entered on the computer (Codync's Settings there); a phone only reads the list.
public struct HermesConnectionsSheet: View {
    @Environment(BotStore.self) private var store

    public init() {}

    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader {
                HStack(spacing: 8) {
                    Image(systemName: "cloud").font(.title2).accessibilityHidden(true)
                    Text(L10n.tr("Hermes connections"))
                }
            }
            CardForm {
                if store.connections.isEmpty {
                    CardSection(footer: "A gateway is a Hermes install on a computer you keep running (your VPS). Bots on it work here beside this computer's bots, in the same chats. Add one on the computer, in Codync → Settings → Hermes connections.") {
                        Text(L10n.tr("No gateways yet.")).foregroundStyle(Palette.secondary)
                    }
                }
                ForEach(store.connections) { connection in
                    CardSection(connection.name) {
                        ValueRow("Address", value: connection.baseUrl)
                        ValueRow("Profile", value: connection.defaultProfile)
                        if let ssh = connection.ssh {
                            ValueRow("SSH host", value: ssh)
                        }
                        ValueRow("Reachable", value: connection.verifiedAt.map { RelativeTime.day(Date(milliseconds: $0)) } ?? L10n.tr("Not checked"))
                        let bots = store.bots.values.filter { $0.connection == connection.id }.sorted { $0.name < $1.name }
                        if bots.isEmpty {
                            Text(L10n.tr("No bots run on it yet."))
                                .font(.caption)
                                .foregroundStyle(Palette.secondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        } else {
                            ForEach(bots) { bot in
                                HStack(spacing: 10) {
                                    CharacterAvatar(bot: bot, size: 24, animated: false)
                                    Text(bot.name).foregroundStyle(Palette.text).lineLimit(1)
                                    Spacer(minLength: 8)
                                    Text(hermesProfileOf(id: bot.id, named: bot.hermesProfile))
                                        .font(.caption.monospaced())
                                        .foregroundStyle(Palette.tertiary)
                                        .lineLimit(1)
                                }
                                .padding(.vertical, 2)
                            }
                        }
                    }
                }
            }
        }
        .background(Palette.background)
        .task { await store.refreshConnections() }
    }
}
