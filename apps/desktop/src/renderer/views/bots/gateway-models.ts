import { useEffect, useState } from 'react'
import type { BotStore } from '../../store/bot-store'
import { connections } from '../../store/connections'

/** The models a gateway's profile offers (`/v1/models`), for a cloud bot's editor. */
export function useGatewayModels(store: BotStore, connection: string | null, profile: string) {
  const [models, setModels] = useState<string[]>([])
  const [failure, setFailure] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const online = store.connection.kind === 'online'

  useEffect(() => {
    setModels([])
    setFailure(null)
    if (!connection || !online) return
    let live = true
    setLoading(true)
    connections
      .test(store, { id: connection, profile: profile.trim() || undefined })
      .then((found) => live && setModels(found.models))
      .catch((e: unknown) => live && setFailure(e instanceof Error ? e.message : String(e)))
      .finally(() => live && setLoading(false))
    return () => {
      live = false
    }
  }, [store, connection, profile, online])

  return { models, failure, loading }
}
