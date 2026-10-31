import { isCloud, profileOf } from '@shared/connections'
import type { BotDraft } from '@shared/models'
import { ChoicePicker, SegmentedChoice, ValueRow } from '../../components/Controls'
import { CloudBadge } from '../../components/CloudBadge'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'
import { useGatewayModels } from './gateway-models'

type Set = <K extends keyof BotDraft>(key: K, value: BotDraft[K]) => void

/** The bot's profile: its own name, or the host's from its id (the id is unknown until it's saved). */
const profileName = (draft: BotDraft) =>
  draft.hermesProfile?.trim() ? profileOf({ id: draft.id ?? '', hermesProfile: draft.hermesProfile }) : null

/**
 * Where the bot runs, in the Agent card: this computer's own agent, or Hermes on a gateway
 * (the user's VPS). A cloud bot always runs as Hermes there, in its own profile.
 */
export function RunLocationSwitch({ draft, set }: { draft: BotDraft; set: Set }) {
  const tr = useTr()
  const store = useStore()
  const cloud = isCloud(draft)
  const connections = store.connections
  const choice = draft.connection ?? ''

  // Switching sides clears the model: it belonged to the other agent.
  const toCloud = () => {
    if (!connections.length) return
    set('connection', choice || connections[0]!.id)
    set('backend', 'hermes')
    set('model', null)
    set('modelOptions', {})
  }
  const toLocal = () => {
    set('connection', null)
    // Back to this computer's Hermes, or Claude when Hermes isn't installed here.
    if (draft.backend === 'hermes' && store.hello?.backends.find((b) => b.id === 'hermes')?.available !== true) {
      set('backend', store.hello?.backends.find((b) => b.available && b.curated)?.id ?? 'claude')
    }
    set('model', null)
    set('modelOptions', {})
  }

  return (
    <>
      <ValueRow
        label="Runs on"
        detail={cloud
          ? 'On the gateway, in its own Hermes profile. It works on that computer, not this one.'
          : 'As an agent process on this computer.'}
      >
        {connections.length ? (
          <SegmentedChoice
            selection={cloud ? 'cloud' : 'local'}
            options={[{ id: 'local', label: 'This computer' }, { id: 'cloud', label: 'Gateway' }]}
            onChange={(id) => (id === 'cloud' ? toCloud() : toLocal())}
          />
        ) : (
          <Text size="caption" tone="secondary">{tr('This computer')}</Text>
        )}
      </ValueRow>
      {!cloud ? null : connections.length ? (
        <>
          <ValueRow label="Gateway" detail={connections.length === 1 ? 'Add another in Settings → Hermes connections.' : null}>
            <ChoicePicker
              selection={choice}
              options={connections.map((c) => ({ id: c.id, label: c.name }))}
              onChange={(id) => {
                set('connection', id)
                set('model', null)
              }}
            />
          </ValueRow>
          <ValueRow
            label="Profile"
            detail="One profile per bot keeps its memory, skills and sessions apart. Codync makes it on the gateway the first time the bot needs it."
          >
            <ProfileField draft={draft} set={set} />
          </ValueRow>
        </>
      ) : (
        <HStack gap={8} align="center" className="bot-form-note">
          <Icon name="cloud" size={14} color="var(--secondary)" />
          <Text size="caption" tone="secondary">{tr('Add your VPS in Settings → Hermes connections to run bots there.')}</Text>
        </HStack>
      )}
    </>
  )
}

/** The profile name; empty is automatic (the host names one from the bot's id). */
function ProfileField({ draft, set }: { draft: BotDraft; set: Set }) {
  const tr = useTr()
  const named = profileName(draft)
  const automatic = draft.id ? profileOf({ id: draft.id }) : 'codync-<bot id>'
  return (
    <HStack gap={8} align="center">
      <Text size="caption" tone="tertiary" truncate title={tr('Automatic')}>{named ?? automatic}</Text>
      <input
        className="field-box bot-profile"
        value={draft.hermesProfile ?? ''}
        placeholder={tr('Automatic')}
        aria-label={tr('Hermes profile')}
        spellCheck={false}
        onChange={(e) => set('hermesProfile', e.target.value.trim() ? e.target.value.trim().toLowerCase() : null)}
      />
    </HStack>
  )
}

/** The profile's models from the gateway (`/v1/models`), beside the Agent card's own picker. */
export function GatewayModelRow({ draft, set }: { draft: BotDraft; set: Set }) {
  const tr = useTr()
  const store = useStore()
  const profile = profileName(draft) ?? profileOf({ id: draft.id ?? '' })
  const { models, failure, loading } = useGatewayModels(store, draft.connection ?? null, profile)
  return (
    <VStack gap={4}>
      <ValueRow label="Model" detail={failure ?? "Which model the gateway's profile answers with."}>
        {loading ? (
          <Text size="caption" tone="secondary">{tr('Loading…')}</Text>
        ) : models.length ? (
          <ChoicePicker
            selection={draft.model ?? ''}
            options={[{ id: '', label: tr('Default') }, ...models.map((m) => ({ id: m, label: m }))]}
            onChange={(id) => set('model', id || null)}
          />
        ) : (
          <Text size="caption" tone={failure ? 'attention' : 'secondary'}>{failure ? tr("The gateway didn't answer") : tr('Default')}</Text>
        )}
      </ValueRow>
    </VStack>
  )
}

/** Where a bot runs, beside its name (the details sheet and the editor's profile card). */
export function RunLocationBadge({ draft }: { draft: BotDraft }) {
  const tr = useTr()
  const cloud = isCloud(draft)
  return (
    <HStack gap={6} align="center" className="bot-form-note">
      <CloudBadge cloud={cloud} size={13} />
      <Text size="caption" tone="secondary">{tr(cloud ? 'On a Hermes gateway' : 'On this computer')}</Text>
    </HStack>
  )
}
