// 同期の画面。設定画面の「同期」タブ(`SyncTab`)から、窓全体を覆う同期の画面(`SyncScreen`)を開く。
// 同期の画面は、つなぐ側の入力から始め、待ち受けへ移る・QRを読む・途中経過・競合の選択・結果を
// 1枚ずつ出す。出るのは「戻る」だけで、閉じたら進んでいる同期を止める。
// 待ち受け・接続・結合の順序と、始める前の確認はRust側が持つ(`scitl_sync::listen`)。
import { type ReactNode, useEffect, useId, useRef, useState } from 'react'
import {
  approveSyncWrite,
  canScanSyncQr,
  cancelSync,
  resolveSyncConflicts,
  syncConnect,
  syncConnectScanned,
  syncListen,
} from './api'
import type {
  ChosenSide,
  ConflictSide,
  ConflictView,
  QrModules,
  SettingsView,
  SyncEvent,
  SyncReport,
  WriteCounts,
  WritePlan,
} from './types'
import { SYNC_ERROR_KEY_PREFIX } from './bindings/SyncConstants'
import Dialog from './Dialog'
import { isolated, type MessageKey, t } from './i18n'
import { DisclosureMark } from './Icon'
import { NumberField } from './settingsFields'
import { useCloseOnBack } from './useCloseOnBack'

type Running = 'listen' | 'connect'
type Stage = 'syncing' | 'peer_choosing' | 'peer_confirming' | 'finishing'
type Failure = Extract<SyncReport, { status: 'failed' }>

// 入力の画面に留めて出す失敗(打ち直すか読み直せば済むもの)。ほかは結果の画面へ移る。
const INPUT_FAILURES = new Set([
  'invalid_address',
  'invalid_code',
  'invalid_qr',
  'scan_failed',
  'camera_denied',
])

// 失敗の種類から文言を引く。英語の診断文は別に添える。
function failureMessage(report: Failure): string {
  return t(`${SYNC_ERROR_KEY_PREFIX}${report.kind}` as MessageKey)
}

// 升目をSVGの1本の経路にする。周りに4升の余白を取る(読み取りに要る)。
function QrCode({ qr }: { qr: QrModules }) {
  const margin = 4
  const path = qr.dark
    .map((dark, i) =>
      dark ? `M${(i % qr.size) + margin} ${Math.floor(i / qr.size) + margin}h1v1h-1z` : '',
    )
    .join('')
  const extent = qr.size + margin * 2
  return (
    <svg
      className="sync-qr"
      viewBox={`0 0 ${extent} ${extent}`}
      role="img"
      aria-label={t('settings.sync.qr_label')}
      shapeRendering="crispEdges"
    >
      <rect width={extent} height={extent} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  )
}

const ROLE_LABELS: Record<string, MessageKey> = {
  user: 'settings.sync.role_user',
  assistant: 'settings.sync.role_assistant',
  error: 'settings.sync.role_error',
}

function conflictTitle(conflict: ConflictView): string {
  switch (conflict.kind) {
    case 'task':
      return t('settings.sync.conflict_task')
    case 'general':
      return t('chat.general_title')
    case 'memory':
      return t('settings.sync.conflict_memory')
  }
}

// 片側の中身。相手の端末から来た中身も、普段の表示と同じく要素の中に閉じ込めて出す。
function SideContent({ side, kind }: { side: ConflictSide; kind: ConflictView['kind'] }) {
  const status = [
    side.deleted ? t('settings.sync.deleted') : null,
    side.archived ? t('settings.sync.archived') : null,
    side.deadline ? t('task.deadline', { deadline: isolated(side.deadline) }) : null,
  ].filter((s) => s !== null)
  return (
    <div className="sync-side-content">
      {kind === 'task' && <bdi className="sync-side-title">{side.title ?? t('task.untitled')}</bdi>}
      {kind === 'memory' && <p className="sync-side-text">{side.content}</p>}
      {status.length > 0 && <p className="settings-hint">{status.join(' · ')}</p>}
      {side.description && <p className="sync-side-text">{side.description}</p>}
      {side.steps.length > 0 && (
        <ul className="sync-side-steps">
          {side.steps.map((step, i) => (
            <li key={i} className={step.done ? 'sync-step-done' : undefined}>
              {step.description}
            </li>
          ))}
        </ul>
      )}
      {side.earlier_messages > 0 && (
        <p className="settings-hint">
          {t('settings.sync.earlier_messages', { count: side.earlier_messages })}
        </p>
      )}
      {side.messages.map((message, i) => (
        <div key={i} className="sync-side-message">
          <span className="settings-hint">
            {ROLE_LABELS[message.role] ? t(ROLE_LABELS[message.role]) : message.role}
          </span>
          <p className="sync-side-text">{message.text}</p>
        </div>
      ))}
      {side.only_here > 0 && (
        <p className="settings-hint">{t('settings.sync.only_here', { count: side.only_here })}</p>
      )}
    </div>
  )
}

// 同期の画面の1枚。見出しの帯・本文・下のボタンの行。情報の少ない画面(`centered`)は本文を
// 上下左右の中央に、入力の画面(`middle`)は上下だけ中央に寄せる。競合の画面(`fill`)は本文を
// 高さいっぱいに広げる。
function SyncPage({
  title,
  aside,
  layout,
  actions,
  children,
}: {
  title: string
  aside?: string
  layout?: 'centered' | 'middle' | 'fill'
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="settings-main">
      <header className="pane-top">
        <div className="settings-header">
          <h1>{title}</h1>
          {aside && <span className="settings-hint sync-screen-aside">{aside}</span>}
        </div>
      </header>
      <div className="settings-content">
        <div className={layout ? `sync-screen-body ${layout}` : 'sync-screen-body'}>{children}</div>
      </div>
      {actions && (
        <footer className="sync-screen-footer">
          <div className="button-row sync-screen-actions">{actions}</div>
        </footer>
      )}
    </div>
  )
}

// 書き込む前の確認の、1つの端末分の件数。変わらない側は1行で示す。分類は結果の画面と同じ。
function ConfirmCounts({ label, counts }: { label: string; counts: WriteCounts }) {
  const rows = [
    ['settings.sync.count_tasks', counts.tasks],
    ['settings.sync.count_conversations', counts.conversations],
    ['settings.sync.count_memories', counts.memories],
  ] as const
  return (
    <div>
      <p>{label}</p>
      {rows.every(([, count]) => count === 0) ? (
        <p>{t('settings.sync.confirm_nothing')}</p>
      ) : (
        <dl className="sync-counts">
          {rows.map(([key, count]) => (
            <div key={key} className="sync-count">
              <dt>{t(key)}</dt>
              <dd>{t('settings.sync.count', { count })}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  )
}

// 競合を1件ずつ選ばせ、最後の1件で選んだ側をまとめて返す。2件目からの「戻る」(画面とAndroid)は
// 前の件へ戻り、同期を止めるのは1件目の「戻る」だけにする。
function ConflictChooser({
  conflicts,
  stopButton,
  onDone,
}: {
  conflicts: ConflictView[]
  stopButton: ReactNode
  onDone: (sent: ConflictView[]) => void
}) {
  const [index, setIndex] = useState(0)
  const [chosen, setChosen] = useState<Record<string, ChosenSide>>({})
  const [sending, setSending] = useState(false)
  const conflict = conflicts[index]
  const last = index === conflicts.length - 1
  const previous = () => setIndex((i) => Math.max(i - 1, 0))
  // 同期の画面の「戻る」(止める)より後に積むので、2件目からはこちらが先に受ける。
  useCloseOnBack(index > 0 && !sending, previous)

  const submit = async () => {
    setSending(true)
    try {
      const accepted = await resolveSyncConflicts(
        conflicts.map((c) => ({ id: c.id, side: chosen[c.id] })),
      )
      if (accepted) onDone(conflicts)
    } finally {
      setSending(false)
    }
  }

  return (
    <SyncPage
      title={t('settings.sync.conflicts_heading')}
      aside={t('settings.sync.conflict_position', {
        current: index + 1,
        total: conflicts.length,
      })}
      layout="fill"
      actions={
        <>
          {index > 0 ? (
            <button type="button" disabled={sending} onClick={previous}>
              {t('settings.sync.back')}
            </button>
          ) : (
            stopButton
          )}
          <button
            type="button"
            className="primary"
            disabled={chosen[conflict.id] === undefined || sending}
            onClick={last ? () => void submit() : () => setIndex(index + 1)}
          >
            {t(last ? 'settings.sync.resolve' : 'settings.sync.next')}
          </button>
        </>
      }
    >
      <div>
        <p>{t('settings.sync.conflicts_message')}</p>
        <p className="settings-hint">{t('settings.sync.conflicts_warning')}</p>
      </div>
      <fieldset className="sync-sides">
        <legend>{conflictTitle(conflict)}</legend>
        {(['ours', 'theirs'] as const).map((side) => (
          <label
            key={side}
            className={chosen[conflict.id] === side ? 'sync-side selected' : 'sync-side'}
          >
            <span className="sync-side-choice">
              <input
                type="radio"
                name={conflict.id}
                checked={chosen[conflict.id] === side}
                disabled={sending}
                onChange={() => setChosen((c) => ({ ...c, [conflict.id]: side }))}
              />
              {t(side === 'ours' ? 'settings.sync.ours' : 'settings.sync.theirs')}
            </span>
            <div className="sync-side-scroll">
              <SideContent side={conflict[side]} kind={conflict.kind} />
            </div>
          </label>
        ))}
      </fieldset>
    </SyncPage>
  )
}

interface SyncTabProps {
  settings: SettingsView
  onStart: () => void
  onSaveListenPort: (text: string) => Promise<string[]>
}

// 設定画面の「同期」タブ。同期の画面を開くボタンと、待ち受けの設定を置く。
export function SyncTab({ settings, onStart, onSaveListenPort }: SyncTabProps) {
  return (
    <div className="settings-panel">
      <div className="settings-field">
        <p>{t('settings.sync.intro')}</p>
        <p>{t('settings.sync.intro_hint')}</p>
      </div>
      <div className="button-row">
        <button type="button" className="primary" onClick={onStart}>
          {t('settings.sync.start')}
        </button>
      </div>

      <details className="settings-advanced">
        <summary>
          <DisclosureMark />
          {t('settings.sync.advanced_settings')}
        </summary>
        <div className="settings-section">
          <NumberField
            label={t('settings.sync.listen_port_label')}
            value={settings.sync.listen_port}
            hint={t('settings.sync.listen_port_hint')}
            onSave={onSaveListenPort}
          />
        </div>
      </details>
    </div>
  )
}

export function SyncScreen({ onClose }: { onClose: () => void }) {
  const [running, setRunning] = useState<Running | null>(null)
  const [listening, setListening] = useState<{
    addresses: string[]
    code: string
    qr: QrModules
  } | null>(null)
  const [showCodes, setShowCodes] = useState(false)
  const [canScan, setCanScan] = useState(false)
  // 相手を確かめたあとの段。
  const [stage, setStage] = useState<Stage | null>(null)
  // 書き込む前の確認。`null`の間は見せない。
  const [confirm, setConfirm] = useState<WritePlan | null>(null)
  // 止められるか(Rust側が途中経過に載せて知らせる)。最初の途中経過が届くまでは止められる。
  const [stoppable, setStoppable] = useState(true)
  const [conflicts, setConflicts] = useState<ConflictView[] | null>(null)
  // 最後に届いた競合。選んで送ったときに、そのあと次の競合が届いていないかを見る。
  const latestConflicts = useRef<ConflictView[] | null>(null)
  // 競合が届くたびに増やし、選び直し(確定をやり直したとき)を最初の1件から始める。
  const [conflictRound, setConflictRound] = useState(0)
  const [result, setResult] = useState<SyncReport | null>(null)
  const [inputError, setInputError] = useState<Failure | null>(null)
  const [address, setAddress] = useState('')
  const [code, setCode] = useState('')
  const formId = useId()
  // 同期の画面を閉じたら(設定画面ごと閉じたときも)、進んでいる同期を止める。
  const runningRef = useRef(false)
  useEffect(() => {
    void canScanSyncQr().then(setCanScan)
  }, [])
  useEffect(
    () => () => {
      if (runningRef.current) void cancelSync()
    },
    [],
  )

  // 途中経過と結果は別の経路で届くので、結果のあとに届いた途中経過は捨てる。
  const onEvent = (event: SyncEvent) => {
    if (!runningRef.current) return
    setStoppable(event.stoppable)
    switch (event.type) {
      case 'listening':
        setListening({ addresses: event.addresses, code: event.code, qr: event.qr })
        break
      case 'conflicts':
        latestConflicts.current = event.conflicts
        setConflicts(event.conflicts)
        setConfirm(null)
        setConflictRound((n) => n + 1)
        break
      case 'confirm':
        setListening(null)
        latestConflicts.current = null
        setConflicts(null)
        setStage(null)
        setConfirm(event.plan)
        break
      default:
        setListening(null)
        latestConflicts.current = null
        setConflicts(null)
        setConfirm(null)
        setStage(event.type)
    }
  }

  const start = async (which: Running, sync: () => Promise<SyncReport>) => {
    setRunning(which)
    setResult(null)
    setInputError(null)
    setShowCodes(false)
    setStoppable(true)
    runningRef.current = true
    let report: SyncReport
    try {
      report = await sync()
    } finally {
      runningRef.current = false
      setRunning(null)
      setListening(null)
      setStage(null)
      setConfirm(null)
      latestConflicts.current = null
      setConflicts(null)
    }
    // 止めたときは入力の画面へ戻る。
    if (report.status === 'cancelled') return
    if (report.status === 'failed' && INPUT_FAILURES.has(report.kind)) {
      setInputError(report)
      return
    }
    setResult(report)
  }

  const connect = (e: React.FormEvent) => {
    e.preventDefault()
    void start('connect', () => syncConnect(address, code, onEvent))
  }

  // やり直すときは最初から打ち直す。どの失敗のあとにどの値が使えなくなるかは画面が持たない。
  const retry = () => {
    setAddress('')
    setCode('')
    setResult(null)
  }

  // Androidの「戻る」は、画面の「戻る」「閉じる」と同じ。止められない間は何もしない。
  // 書き込む前の確認では、確認の「戻る」の答えになる(競合があれば選択へ、無ければ止める)。
  useCloseOnBack(true, () => {
    if (running === null) onClose()
    else if (confirm !== null) void approveSyncWrite(false)
    else if (stoppable) void cancelSync()
  })

  const stopButton = stoppable && (
    <button type="button" onClick={() => void cancelSync()}>
      {t('settings.sync.back')}
    </button>
  )

  if (result !== null) {
    const report = result
    if (report.status === 'synced') {
      const counts = [
        ['settings.sync.count_tasks', report.changed.tasks],
        ['settings.sync.count_conversations', report.changed.conversations],
        ['settings.sync.count_memories', report.changed.memories],
      ] as const
      return (
        <SyncPage
          title={t('settings.sync.synced_heading')}
          layout="centered"
          actions={
            <button type="button" className="primary" onClick={onClose}>
              {t('common.close')}
            </button>
          }
        >
          <p>{t('settings.sync.synced_changed')}</p>
          {counts.every(([, count]) => count === 0) ? (
            <p>{t('settings.sync.synced_nothing')}</p>
          ) : (
            <dl className="sync-counts">
              {counts.map(([label, count]) => (
                <div key={label} className="sync-count">
                  <dt>{t(label)}</dt>
                  <dd>{t('settings.sync.count', { count })}</dd>
                </div>
              ))}
            </dl>
          )}
        </SyncPage>
      )
    }
    if (report.status === 'failed') {
      return (
        <SyncPage
          title={t('settings.sync.failed_heading')}
          layout="centered"
          actions={
            <>
              <button type="button" onClick={onClose}>
                {t('common.close')}
              </button>
              <button type="button" className="primary" onClick={retry}>
                {t('settings.sync.retry')}
              </button>
            </>
          }
        >
          <p className="error" role="alert">
            {failureMessage(report)}
          </p>
          <p className="settings-hint">{isolated(report.detail)}</p>
        </SyncPage>
      )
    }
  }

  if (running !== null && conflicts !== null) {
    return (
      <ConflictChooser
        key={conflictRound}
        conflicts={conflicts}
        stopButton={stopButton}
        // 送ったあとに次の競合(確定をやり直したとき)が届いていれば、そちらを残す。届いていなければ、
        // Rust側は選んだ直後に書き込みへ進むので、その段が届くまでも止めない。
        onDone={(sent) => {
          if (latestConflicts.current !== sent) return
          latestConflicts.current = null
          setConflicts(null)
          setStoppable(false)
        }}
      />
    )
  }

  // 書き込む前の確認。「戻る」は競合の選択に戻り(無ければ同期を止め)、書き込むかを決める。
  if (running !== null && confirm !== null) {
    return (
      <SyncPage
        title={t('settings.sync.confirm_heading')}
        layout="centered"
        actions={
          <>
            <button type="button" onClick={() => void approveSyncWrite(false)}>
              {t('settings.sync.back')}
            </button>
            <button type="button" className="primary" onClick={() => void approveSyncWrite(true)}>
              {t('settings.sync.confirm_write')}
            </button>
          </>
        }
      >
        <ConfirmCounts label={t('settings.sync.ours')} counts={confirm.ours} />
        <ConfirmCounts label={t('settings.sync.theirs')} counts={confirm.theirs} />
      </SyncPage>
    )
  }

  if (running !== null && stage === 'finishing') {
    return (
      <SyncPage title={t('settings.sync.progress_heading')} layout="centered">
        <div>
          <p>{t('settings.sync.writing_message')}</p>
          <p className="settings-hint">{t('settings.sync.writing_hint')}</p>
        </div>
      </SyncPage>
    )
  }

  if (running !== null && stage === 'peer_confirming') {
    return (
      <SyncPage title={t('settings.sync.progress_heading')} layout="centered" actions={stopButton}>
        <p>{t('settings.sync.peer_confirming')}</p>
      </SyncPage>
    )
  }

  if (running !== null && stage === 'peer_choosing') {
    return (
      <SyncPage title={t('settings.sync.progress_heading')} layout="centered" actions={stopButton}>
        <p>{t('settings.sync.peer_choosing')}</p>
      </SyncPage>
    )
  }

  if (running !== null && stage === 'syncing') {
    return (
      <SyncPage title={t('settings.sync.progress_heading')} layout="centered" actions={stopButton}>
        <p>{t('settings.sync.syncing')}</p>
      </SyncPage>
    )
  }

  if (running === 'listen' && listening !== null) {
    return (
      <SyncPage title={t('settings.sync.listen_heading')} layout="centered" actions={stopButton}>
        <p>{t('settings.sync.listen_hint')}</p>
        <QrCode qr={listening.qr} />
        <button type="button" onClick={() => setShowCodes(true)}>
          {t('settings.sync.show_codes')}
        </button>
        {showCodes && (
          <Dialog title={t('settings.sync.codes_title')} onClose={() => setShowCodes(false)}>
            <div className="settings-field">
              <span className="settings-hint">{t('settings.sync.address_label')}</span>
              <ul className="sync-addresses">
                {listening.addresses.map((a) => (
                  <li key={a}>
                    <code>{a}</code>
                  </li>
                ))}
              </ul>
            </div>
            <div className="settings-field">
              <span className="settings-hint">{t('settings.sync.code_label')}</span>
              <code className="sync-code">{listening.code}</code>
            </div>
            <div className="button-row dialog-actions">
              <button type="button" onClick={() => setShowCodes(false)}>
                {t('common.close')}
              </button>
            </div>
          </Dialog>
        )}
      </SyncPage>
    )
  }

  if (running !== null) {
    return (
      <SyncPage
        title={t(
          running === 'listen' ? 'settings.sync.listen_heading' : 'settings.sync.progress_heading',
        )}
        layout="centered"
        actions={stopButton}
      >
        <p>
          {t(running === 'listen' ? 'settings.sync.preparing_listen' : 'settings.sync.connecting')}
        </p>
      </SyncPage>
    )
  }

  return (
    <SyncPage
      title={t('settings.sync.connect_heading')}
      layout="middle"
      actions={
        <>
          <button type="button" onClick={onClose}>
            {t('settings.sync.back')}
          </button>
          <button
            type="submit"
            form={formId}
            className="primary"
            disabled={address.trim() === '' || code.trim() === ''}
          >
            {t('settings.sync.connect')}
          </button>
        </>
      }
    >
      <form id={formId} className="sync-connect-form" onSubmit={connect}>
        <p>{t('settings.sync.connect_hint')}</p>
        <label className="settings-field">
          <span>{t('settings.sync.address_label')}</span>
          <input
            type="text"
            value={address}
            autoComplete="off"
            spellCheck={false}
            aria-invalid={inputError?.kind === 'invalid_address' || undefined}
            onChange={(e) => {
              setAddress(e.target.value)
              setInputError(null)
            }}
          />
        </label>
        <label className="settings-field">
          <span>{t('settings.sync.code_label')}</span>
          <input
            type="text"
            inputMode="numeric"
            className="sync-code-input"
            value={code}
            autoComplete="off"
            aria-invalid={inputError?.kind === 'invalid_code' || undefined}
            onChange={(e) => {
              setCode(e.target.value)
              setInputError(null)
            }}
          />
        </label>
        {inputError && (
          <p className="error" role="alert">
            {failureMessage(inputError)}
          </p>
        )}
        {/* カメラの無い端末では「QRを読み取る」を出さず、「コードを発行する」が行の幅を占める。 */}
        <div className="sync-alt-actions">
          <button type="button" onClick={() => void start('listen', () => syncListen(onEvent))}>
            {t('settings.sync.listen')}
          </button>
          {canScan && (
            <button
              type="button"
              onClick={() => void start('connect', () => syncConnectScanned(onEvent))}
            >
              {t('settings.sync.scan')}
            </button>
          )}
        </div>
      </form>
    </SyncPage>
  )
}
