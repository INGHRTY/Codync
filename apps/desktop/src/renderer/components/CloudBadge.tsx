import { useTr } from '../lib/use-language'
import { Icon } from './Icon'

/**
 * Where a bot runs: on this computer (a laptop) or on a Hermes gateway on another one (a cloud).
 * The roster shows it on the avatar, the editor beside the bot's name; the text form is the
 * TUI's and the badge's accessible label.
 */
export function CloudBadge({ cloud, size = 14 }: { cloud: boolean; size?: number }) {
  const tr = useTr()
  const label = tr(cloud ? 'Runs on a Hermes gateway' : 'Runs on this computer')
  return (
    <span className="cloud-badge" role="img" aria-label={label} title={label}>
      <Icon name={cloud ? 'cloud' : 'desktopcomputer'} size={size} color="var(--secondary)" />
    </span>
  )
}

/** The roster avatar's corner marker for a cloud bot, sized like the lead badge it sits beside. */
export function CloudAvatarBadge({ size }: { size: number }) {
  const tr = useTr()
  return (
    <span className="cloud-avatar-badge" style={{ width: size, height: size }} role="img" aria-label={tr('Runs on a Hermes gateway')}>
      <Icon name="cloud" size={size * 0.58} color="var(--text)" scaled={false} />
    </span>
  )
}
