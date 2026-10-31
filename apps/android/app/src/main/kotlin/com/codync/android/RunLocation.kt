package com.codync.android

import androidx.compose.foundation.layout.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.material3.Text
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.codync.android.core.*
import com.codync.android.design.*
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.put

/** The bot's profile: its own name, or the host's from its id (unknown until it's saved). */
private fun profileName(draft: BotDraft): String? =
    draft.hermesProfile?.trim()?.takeIf(String::isNotEmpty)?.let { hermesProfileOf(draft.id.orEmpty(), it) }

/**
 * Where the bot runs, in the Agent card: this computer's own agent, or Hermes on a gateway (the user's VPS).
 * A gateway bot always runs as Hermes there, in its own profile.
 */
internal fun CardRows.runLocationRows(state: AppState, draft: BotDraft, change: (BotDraft) -> Unit) {
    val connections = state.connections
    row {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            ValueRow("Runs on", detail = if (draft.isCloud)
                "On the gateway, in its own Hermes profile. It works on that computer, not this one."
                else "As an agent process on this computer.") {
                if (connections.isEmpty()) Text(tr("This computer"), style = Type.body, color = Palette.secondary)
                else ChoicePicker(if (draft.isCloud) "cloud" else "local",
                    listOf("local" to "This computer", "cloud" to "Gateway")) {
                    change(draft.withRunLocation(if (it == "cloud") draft.connection ?: connections.first().id else null))
                }
            }
        }
    }
    if (!draft.isCloud) return
    if (connections.isEmpty()) {
        row {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Symbol("cloud", size = 14.dp, tint = Palette.secondary)
                Text(tr("Add your VPS in Settings on the computer to run bots there."), style = Type.caption, color = Palette.secondary)
            }
        }
        return
    }
    row {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            ValueRow("Gateway", detail = if (connections.size == 1) "Add another in Settings on the computer." else null) {
                ChoicePicker(draft.connection ?: connections.first().id, connections.map { it.id to it.name }) {
                    change(draft.copy(connection = it, model = null))
                }
            }
        }
    }
    row {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            ValueRow("Profile", detail = "One profile per bot keeps its memory, skills and sessions apart. Codync makes it on the gateway the first time the bot needs it.") {
                Text(profileName(draft) ?: (draft.id?.let { hermesProfileOf(it, null) } ?: "codync-<bot id>"),
                    style = Type.caption.monospaced(), color = Palette.tertiary, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            FieldBox(draft.hermesProfile.orEmpty(), { change(draft.copy(hermesProfile = it.trim().ifEmpty { null }?.lowercase())) },
                "Automatic", style = Type.caption.monospaced(), capitalize = false)
        }
    }
}

/** A gateway bot's model: what the gateway's profile offers (`/v1/models`), never this computer's agents. */
@Composable internal fun GatewayModelRow(state: AppState, store: AppStore, draft: BotDraft, change: (BotDraft) -> Unit,
    modifier: Modifier = Modifier) {
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var failure by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val connection = draft.connection
    val profile = hermesProfileOf(draft.id.orEmpty(), draft.hermesProfile)
    LaunchedEffect(connection, profile) {
        models = emptyList()
        failure = null
        val active = connection ?: return@LaunchedEffect
        loading = true
        try {
            val answer = store.ready().call("testConnection", buildJsonObject {
                put("id", active)
                put("profile", profile)
            }, 45_000)
            models = runCatching { WireJson.decodeFromJsonElement<ConnectionCheck>(answer) }.getOrNull()?.models ?: emptyList()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            failure = error.message ?: error.toString()
        } finally {
            loading = false
        }
    }
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        ValueRow("Model", detail = failure ?: "Which model the gateway's profile answers with.") {
            when {
                loading -> Spinner()
                models.isNotEmpty() -> ChoicePicker(draft.model ?: "", listOf("" to "Default") + models.map { it to it }) {
                    change(draft.copy(model = it.ifEmpty { null }))
                }
                else -> Text(tr(if (failure == null) "Default" else "The gateway didn't answer"),
                    style = Type.body, color = if (failure == null) Palette.secondary else Palette.warning)
            }
        }
    }
}
