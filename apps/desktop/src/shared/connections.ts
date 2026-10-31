/** Hermes gateways on the user's other computers: where their cloud bots run. */

/** One gateway, as the host saved it (`listConnections`). The key itself never leaves the computer. */
export interface Connection {
  id: string
  name: string
  /** `https://vps.example.com` or `http://100.x.y.z:8642`, without a `/p/<profile>` suffix. */
  baseUrl: string
  /** The profile the gateway's un-prefixed routes serve, usually `default`. */
  defaultProfile: string
  /** How to reach that computer's shell, so Codync makes a bot's profile there (`user@host` or an SSH alias). */
  ssh?: string | null
  /** The last time the gateway answered, in ms. */
  verifiedAt?: number | null
}

/** What `testConnection` found: the profile it checked, its models and what the gateway can do. */
export interface ConnectionCheck {
  profile: string
  models: string[]
  features: Record<string, unknown>
}

/** A bot's `connection` and `hermesProfile`: the gateway it runs on and the profile there. */
export interface BotConnection {
  connection?: string | null
  hermesProfile?: string | null
}

/** Whether a saved connection has answered recently enough to show as reachable. */
export const isVerified = (c: Connection): boolean => c.verifiedAt != null

/** The Hermes profile a bot runs as: its own name, or the one the host names from its id (see `hermes::profile_of`). */
export function profileOf(bot: { id: string; hermesProfile?: string | null }): string {
  const named = bot.hermesProfile?.trim()
  if (named) return named.toLowerCase()
  return `codync-${bot.id.replace(/[^0-9a-fA-F]/g, '').slice(0, 12)}`
}

/** Whether a bot runs on a gateway rather than as a process on its own computer. */
export const isCloud = (bot: { connection?: string | null }): boolean => !!bot.connection
