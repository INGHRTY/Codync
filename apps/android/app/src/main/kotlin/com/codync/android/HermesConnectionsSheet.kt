package com.codync.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.Bot
import com.codync.android.core.HermesConnection
import com.codync.android.core.hermesProfileOf
import com.codync.android.design.*

/**
 * The Hermes gateways on the user's other computers (the cloud bots' side), read-only (iOS
 * `HermesConnectionsSheet`): where each one is, the profile its plain routes serve, whether it answered recently,
 * and which bots run on it. Gateways are added and their keys entered on the computer; a phone only reads the list.
 */
@Composable internal fun HermesConnectionsSheet(connections: List<HermesConnection>, bots: List<Bot>, refresh: () -> Unit) {
    LaunchedEffect(Unit) { refresh() }
    Column(Modifier.fillMaxSize().background(Palette.background)) {
        ModalHeader({
            Symbol("cloud", size = 24.dp, tint = Palette.text)
            Spacer(Modifier.width(8.dp))
            Text(tr("Hermes connections"), style = Type.body.semibold, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
        })
        CardForm {
            if (connections.isEmpty()) CardSection(
                footer = "A gateway is a Hermes install on a computer you keep running (your VPS). Bots on it work here beside this computer's bots, in the same chats. Add one on the computer, in Codync → Settings → Hermes connections.") {
                row { Text(tr("No gateways yet."), style = Type.body, color = Palette.secondary) }
            }
            for (connection in connections) CardSection(connection.name) {
                row { ValueRow("Address", connection.baseUrl) }
                row { ValueRow("Profile", connection.defaultProfile) }
                connection.ssh?.let { ssh -> row { ValueRow("SSH host", ssh) } }
                row {
                    ValueRow("Reachable", connection.verifiedAt?.let { RelativeTime.day(it, yesterday = tr("Yesterday")) } ?: tr("Not checked"))
                }
                val running = bots.filter { it.connection == connection.id }.sortedBy { it.name }
                if (running.isEmpty()) row {
                    Text(tr("No bots run on it yet."), style = Type.caption, color = Palette.secondary)
                } else for (bot in running) row {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        CharacterAvatar(bot.avatarShape, bot.avatarColor, 24.dp)
                        Text(bot.name, style = Type.body, color = Palette.text, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                        Text(hermesProfileOf(bot.id, bot.hermesProfile), style = Type.caption.monospaced, color = Palette.tertiary,
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
            if (connections.isNotEmpty()) CardSection(footer = "The key for each profile is kept on the computer and never leaves it.") {}
        }
    }
}
