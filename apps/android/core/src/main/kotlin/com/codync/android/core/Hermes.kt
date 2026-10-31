package com.codync.android.core

import kotlinx.serialization.Serializable

/**
 * A Hermes gateway on another computer (`hermes::connections`): where the user's cloud bots run. Its keys stay
 * on the computer that owns it; a phone reads only these names and addresses.
 */
@Serializable
data class HermesConnection(
    val id: String,
    val name: String,
    /** `https://vps.example.com`, without a `/p/<profile>` suffix. */
    val baseUrl: String,
    /** The profile the gateway's plain routes serve, usually `default`. */
    val defaultProfile: String = "default",
    /** How Codync reaches that computer's shell to make profiles (`user@host` or an SSH alias). */
    val ssh: String? = null,
    /** The last time it answered, in ms. */
    val verifiedAt: Long? = null,
)

/** What `testConnection` found: the profile it checked and its models. */
@Serializable
data class ConnectionCheck(val profile: String, val models: List<String> = emptyList())

/** The Hermes profile a bot runs as: its own name, or the one the host names from its id (`hermes::profile_of`). */
fun hermesProfileOf(id: String, named: String?): String {
    val name = named?.trim().orEmpty()
    if (name.isNotEmpty()) return name.lowercase()
    return "codync-" + id.filter { it.isDigit() || it.lowercaseChar() in 'a'..'f' }.take(12)
}
