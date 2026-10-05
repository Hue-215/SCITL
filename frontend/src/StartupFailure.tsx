import { isolated, t, type MessageKey } from './i18n'
import type { DataDirError } from './types'

const MESSAGES: Record<DataDirError['kind'], MessageKey> = {
  no_executable: 'startup.no_executable',
  temporary_dir: 'startup.temporary_dir',
  unusable: 'startup.unusable',
}

/** データフォルダを開けなかったときに、アプリの代わりに出す画面。 */
export default function StartupFailure({ failure }: { failure: DataDirError }) {
  return (
    <main className="startup-failure">
      <h1>{t('startup.title')}</h1>
      <p>{t(MESSAGES[failure.kind])}</p>
      {'dir' in failure && (
        <p className="startup-failure-detail">
          {t('startup.location', { dir: isolated(failure.dir) })}
        </p>
      )}
      {'reason' in failure && (
        <p className="startup-failure-detail">
          {t('startup.reason', { reason: isolated(failure.reason) })}
        </p>
      )}
    </main>
  )
}
