import { useEffect, useState } from 'react'
import type { Connection } from '@shared/connections'
import { Button, CardForm, CardSection, IconButton, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { ModalHeader, Sheet } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useModel } from '../../lib/observable'
import { useTr } from '../../lib/use-language'
import type { BotStore } from '../../store/bot-store'
import { connections, refreshConnections, type ConnectionDraft } from '../../store/connections'
import { useApp } from '../../store/context'
import { errorText, relativeDay } from './parts'
import './connections.css'

/**
 * Settings → Hermes connections: the gateways on the user's other computers where cloud bots run.
 * A connection is a base URL, the profile its plain routes serve, the SSH host Codync sets profiles
 * up over, and a key per profile (kept in the computer's vault, never sent to a client).
 */
export function ConnectionsView() {
  const tr = useTr()
  const app = useApp()
  const store = app.local
  useModel(store)
  const [editing, setEditing] = useState<Connection | 'new' | null>(null)
  const [removing, setRemoving] = useState<Connection | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (store && store.connection.kind === 'online') void refreshConnections(store)
  }, [store, store?.connection.kind])

  if (!store) {
    return (
      <CardForm>
        <Text tone="secondary">{tr(`Connections are kept by this computer's host, which isn't running.`)}</Text>
      </CardForm>
    )
  }
  const online = store.connection.kind === 'online' && !store.mismatch
  const list = store.connections

  const remove = async (c: Connection) => {
    setBusy(true)
    try {
      await connections.remove(store, c.id)
      setRemoving(null)
    } catch (e) {
      store.setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <CardForm>
      <CardSection
        title="Gateways"
        footer="A gateway is a Hermes install on a computer you keep running (your VPS). Bots on it work here beside this computer's bots, in the same chats."
        accessory={
          <Button kind="secondary" disabled={!online} onClick={() => setEditing('new')}>
            {tr('Add')}
          </Button>
        }
      >
        {list.length ? (
          list.map((c) => (
            <button key={c.id} className="plain-row-button connections-row" onClick={() => setEditing(c)} disabled={!online}>
              <VStack gap={2} grow>
                <HStack gap={8} align="center">
                  <Text truncate weight="medium">{c.name}</Text>
                  <Text size="caption" tone={c.verifiedAt != null ? 'secondary' : 'tertiary'}>
                    {c.verifiedAt != null ? tr(`Reachable`) : tr(`Not checked`)}
                  </Text>
                </HStack>
                <Text size="caption" tone="secondary" truncate>
                  {`${c.baseUrl} · ${tr('profile')} ${c.defaultProfile}${c.ssh ? ` · ssh ${c.ssh}` : ''}`}
                </Text>
              </VStack>
              <Icon name="chevron.right" size={10} color="var(--secondary)" />
            </button>
          ))
        ) : (
          <Text tone="secondary">{tr(`No gateways yet. Add your VPS to run bots on it from here.`)}</Text>
        )}
        <Text size="caption" tone="secondary">
          {tr(`The key is kept on this computer and never leaves it.`)}
        </Text>
      </CardSection>
      {store.lastError ? <Text size="footnote" tone="danger">{store.lastError}</Text> : null}
      <Sheet open={editing !== null} onClose={() => setEditing(null)} width={480} height={520}>
        {editing ? (
          <ConnectionEditor
            store={store}
            existing={editing === 'new' ? null : editing}
            done={() => setEditing(null)}
            remove={() => {
              setRemoving(editing === 'new' ? null : editing)
              setEditing(null)
            }}
          />
        ) : null}
      </Sheet>
      <Sheet open={removing !== null} onClose={() => setRemoving(null)} width={420} height={280}>
        {removing ? (
          <VStack gap={20} className="connections-confirm">
            <ModalHeader title={tr('Remove {name}?').replace('{name}', removing.name)} />
            <Text tone="secondary">
              {tr(`Bots on it stop working. Its profile keys are removed from this computer; the gateway itself is untouched.`)}
            </Text>
            <HStack gap={10} justify="flex-end">
              <Button kind="secondary" onClick={() => setRemoving(null)}>{tr('Cancel')}</Button>
              <Button disabled={busy} onClick={() => void remove(removing)}>{busy ? '…' : tr('Remove')}</Button>
            </HStack>
          </VStack>
        ) : null}
      </Sheet>
    </CardForm>
  )
}

/** Add or edit one gateway: where it is, how Codync reaches it, and the key for one profile. */
function ConnectionEditor({ store, existing, done, remove }: {
  store: BotStore
  existing: Connection | null
  done: () => void
  remove: () => void
}) {
  const tr = useTr()
  const [draft, setDraft] = useState<ConnectionDraft>({
    id: existing?.id,
    name: existing?.name ?? '',
    baseUrl: existing?.baseUrl ?? '',
    profile: existing?.defaultProfile ?? 'default',
    ssh: existing?.ssh ?? '',
  })
  const [key, setKey] = useState('')
  const [check, setCheck] = useState<{ state: 'idle' | 'testing' | 'ok' | 'failed'; models?: string[]; message?: string }>({ state: 'idle' })
  const [busy, setBusy] = useState(false)
  const set = <K extends keyof ConnectionDraft>(k: K, v: ConnectionDraft[K]) => setDraft((d) => ({ ...d, [k]: v }))
  // A changed address or profile is a different gateway until checked again.
  useEffect(() => setCheck({ state: 'idle' }), [draft.baseUrl, draft.profile, key])

  const ready = draft.name.trim() !== '' && draft.baseUrl.trim() !== '' && draft.profile.trim() !== '' && (existing !== null || key.trim() !== '')

  const test = async () => {
    setCheck({ state: 'testing' })
    try {
      const found = await connections.test(store, {
        ...(draft.id ? { id: draft.id } : { baseUrl: draft.baseUrl }),
        profile: draft.profile,
        ...(key.trim() ? { key: key.trim() } : {}),
      })
      setCheck({ state: 'ok', models: found.models })
    } catch (e) {
      setCheck({ state: 'failed', message: errorText(e) })
    }
  }

  const save = async () => {
    if (!ready || busy) return
    setBusy(true)
    try {
      await connections.save(store, { ...draft, key: key.trim() || undefined })
      done()
    } catch (e) {
      setCheck({ state: 'failed', message: errorText(e) })
    } finally {
      setBusy(false)
    }
  }

  return (
    <VStack gap={20} className="connections-editor">
      <ModalHeader
        icon="cloud"
        title={existing ? 'Edit gateway' : 'Add gateway'}
        trailing={busy ? <Spinner /> : <IconButton title="Save" icon="checkmark" disabled={!ready || busy} onClick={() => void save()} />}
      />
      <VStack gap={16}>
        <CardSection>
          <Field label="Name" value={draft.name} onChange={(v) => set('name', v)} placeholder="VPS" />
          <Field label="Address" value={draft.baseUrl} onChange={(v) => set('baseUrl', v)} placeholder="https://vps.example.com" mono />
          <Field label="Profile" value={draft.profile} onChange={(v) => set('profile', v)} placeholder="default" mono detail="The profile this gateway's plain routes serve, usually default. A bot gets its own profile under /p/." />
          <Field label="SSH host" value={draft.ssh ?? ''} onChange={(v) => set('ssh', v)} placeholder="user@vps.example.com" mono detail="Optional. Lets Codync make each bot's profile and key on that computer, and install its Codync tools (messages, memory, other bots) in that profile. Without it, make the profiles yourself and save their keys here: those bots then answer without those tools." />
        </CardSection>
        <CardSection title="Key" footer={`The key for this profile, at least 16 characters. ${existing ? 'Leave empty to keep the saved one.' : ''}`}>
          <Field label="Profile key" value={key} onChange={setKey} placeholder={existing ? tr('Saved') : 'Paste the gateway key'} secret mono />
          <HStack gap={10} align="center">
            <Button kind="secondary" onClick={() => void test()} disabled={!draft.baseUrl.trim() || check.state === 'testing'}>
              {check.state === 'testing' ? tr('Checking…') : tr('Test connection')}
            </Button>
            {check.state === 'ok' ? (
              <Text size="caption" tone="secondary">{check.models?.length ? tr(`Works · {n} models`).replace('{n}', String(check.models.length)) : tr('Works')}</Text>
            ) : null}
            {check.state === 'failed' ? <Text size="caption" tone="danger" grow>{check.message}</Text> : null}
          </HStack>
        </CardSection>
        {existing ? (
          <Button kind="secondary" onClick={remove} disabled={busy}>
            {tr('Remove gateway')}
          </Button>
        ) : null}
        {existing?.verifiedAt ? (
          <Text size="caption" tone="tertiary">{tr(`Last answered {when}`).replace('{when}', relativeDay(new Date(existing.verifiedAt)))}</Text>
        ) : null}
      </VStack>
    </VStack>
  )
}

/** A labelled text input in the settings rows' style. */
function Field({ label, value, onChange, placeholder, detail, mono = false, secret = false }: {
  label: string
  value: string
  onChange: (v: string) => void
  placeholder: string
  detail?: string
  mono?: boolean
  secret?: boolean
}) {
  const tr = useTr()
  return (
    <div className="connections-field">
      <Text tone="text">{tr(label)}</Text>
      {detail ? <Text size="caption" tone="secondary">{tr(detail)}</Text> : null}
      <input
        className={`settings-field${mono ? ' settings-mono' : ''}`}
        aria-label={tr(label)}
        placeholder={tr(placeholder)}
        value={value}
        spellCheck={false}
        autoComplete={secret ? 'off' : undefined}
        type={secret ? 'password' : 'text'}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  )
}
