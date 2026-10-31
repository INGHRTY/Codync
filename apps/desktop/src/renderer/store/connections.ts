import type { Connection, ConnectionCheck } from '@shared/connections'
import type { BotStore } from './bot-store'

/**
 * Hermes gateways on the user's other computers (the cloud bots' side) through this computer's
 * loopback API: the calls behind Settings → Hermes connections and a cloud bot's editor. Keys
 * are saved on the computer's vault and never travel back, so a form sends a key and reads only
 * whether the connection answered. The `connections` event keeps `store.connections` current.
 */

/** A gateway probe waits for another computer's network and the profile's first key. */
const SLOW = 45_000

async function call<T>(store: BotStore, method: string, body: object = {}, timeout?: number) {
  return (await store.ready()).call<T>(method, body, timeout)
}

/** Reads the list again; the host's `connections` event usually got there first. */
export async function refreshConnections(store: BotStore) {
  try {
    store.setConnections((await call<{ connections: Connection[] }>(store, 'listConnections')).connections)
  } catch {
    // A host without Hermes connections: the page stays empty and the next connection reads it again.
  }
}

/** A connection as the editor sends it: `id` edits a saved one, `key` replaces its key for `profile`. */
export interface ConnectionDraft {
  id?: string
  name: string
  baseUrl: string
  profile: string
  ssh?: string | null
  key?: string
}

export const connections = {
  save: async (store: BotStore, draft: ConnectionDraft) => {
    const saved = await call<{ connection: Connection }>(store, 'saveConnection', draft)
    await refreshConnections(store)
    return saved.connection
  },
  /** Checks the saved connection `id`, or one being typed (`baseUrl` + `key`); the key is never saved by this. */
  test: (store: BotStore, probe: { id?: string; baseUrl?: string; profile?: string; key?: string }) =>
    call<ConnectionCheck>(store, 'testConnection', probe, SLOW),
  remove: async (store: BotStore, id: string) => {
    await call(store, 'deleteConnection', { id })
    await refreshConnections(store)
  },
}
