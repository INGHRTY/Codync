import { useState } from 'react'
import type { PlanItem } from '@shared/models'
import { Icon } from '../../components/Icon'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import './plan-tray.css'

/** The running turn's plan, floating above the composer: foldable, dismissible, and a tap opens the trace. */
export function PlanTray({ turn, steps, openTrace }: { turn: number; steps: PlanItem[]; openTrace: () => void }) {
  const tr = useTr()
  // Hidden for the turn it was dismissed in; the next turn's plan shows again.
  const [dismissed, setDismissed] = useState<number | null>(null)
  const [folded, setFolded] = useState(false)
  if (dismissed === turn) return null
  const done = steps.filter((step) => step.status === 'completed').length
  return (
    <div className="plan-tray">
      <HStack gap={2} className="plan-tray-head">
        <button className="plan-tray-open" onClick={openTrace} title={tr(`Show what it did`)}>
          <HStack gap={6}>
            <Icon name="checklist" size={11} weight="bold" color="var(--secondary)" />
            <Text size="caption" weight="semibold" tone="text">{tr(`Plan`)}</Text>
            <Text size="caption" tone="tertiary" tabular>{`${done}/${steps.length}`}</Text>
          </HStack>
        </button>
        <button
          className="plan-tray-action"
          aria-expanded={!folded}
          aria-label={tr(folded ? `Unfold the plan` : `Fold the plan`)}
          title={tr(folded ? `Unfold the plan` : `Fold the plan`)}
          onClick={() => setFolded((f) => !f)}
        >
          <Icon name={folded ? 'chevron.up' : 'chevron.down'} size={9} weight="semibold" color="var(--secondary)" />
        </button>
        <button className="plan-tray-action" aria-label={tr(`Hide the plan`)} title={tr(`Hide the plan`)} onClick={() => setDismissed(turn)}>
          <Icon name="xmark" size={9} weight="semibold" color="var(--secondary)" />
        </button>
      </HStack>
      {folded ? null : (
        <VStack gap={4} className="plan-tray-steps">
          {steps.map((step, i) => {
            const finished = step.status === 'completed'
            return (
              <HStack key={i} gap={6} align="flex-start">
                <span className="plan-tray-icon">
                  <Icon
                    name={finished ? 'checkmark.circle.fill' : step.status === 'in_progress' ? 'circle.dotted' : 'circle'}
                    size={10}
                    color={finished ? 'var(--accent)' : 'var(--tertiary)'}
                  />
                </span>
                <Text size="caption" tone={finished ? 'tertiary' : 'text'} className={finished ? 'plan-tray-done' : undefined}>{step.content}</Text>
              </HStack>
            )
          })}
        </VStack>
      )}
    </div>
  )
}
