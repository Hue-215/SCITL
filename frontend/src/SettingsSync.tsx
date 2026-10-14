// 設定画面の「同期」タブ。この端末で待ち受けるか、待ち受けている端末につなぐかを選び、途中経過と
// 結果を出す。競合が出たら(待ち受ける側)両側の中身を並べ、単位ごとに残す側を選ばせる。
// 待ち受け・接続・結合の順序と、始める前の確認はRust側が持つ(`scitl_sync::listen`)。
import { useEffect, useId, useRef, useState } from 'react'
import { cancelSync, resolveSyncConflicts, syncConnect, syncListen } from './api'
import type { ChosenSide, ConflictSide, ConflictView, SyncEvent, SyncReport } from './types'
import { isolated, type MessageKey, t } from './i18n'

type Running = 'listen' | 'connect'
type Stage = 'syncing' | 'peer_choosing' | 'finishing'

// 失敗の種類から文言を引き、英語の診断文を詳細として添える(コマンドの失敗と同じ形)。
function reportText(report: SyncReport): string {
  switch (report.status) {
    case 'synced':
      return t('settings.sync.synced', { ...report.changed })
    case 'cancelled':
      return t('settings.sync.cancelled')
    case 'failed':
      return t('common.failure_with_detail', {
        message: t(`settings.sync.error.${report.kind}` as MessageKey),
        detail: isolated(report.detail),
      })
  }
}

// 数字は3桁ずつに分けて出す(読み上げて打ちやすいように)。
function spacedCode(code: string): string {
  return `${code.slice(0, 3)} ${code.slice(3)}`
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

function ConflictChooser({
  conflicts,
  onDone,
}: {
  conflicts: ConflictView[]
  onDone: (sent: ConflictView[]) => void
}) {
  const headingId = useId()
  const [chosen, setChosen] = useState<Record<string, ChosenSide>>({})
  const [sending, setSending] = useState(false)
  const complete = conflicts.every((c) => chosen[c.id] !== undefined)

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
    <section className="settings-section settings-section-break" aria-labelledby={headingId}>
      <h2 id={headingId}>{t('settings.sync.conflicts_heading')}</h2>
      <p className="settings-hint">{t('settings.sync.conflicts_hint')}</p>
      {conflicts.map((conflict) => (
        <fieldset key={conflict.id} className="sync-conflict">
          <legend>{conflictTitle(conflict)}</legend>
          <div className="sync-sides">
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
                <SideContent side={conflict[side]} kind={conflict.kind} />
              </label>
            ))}
          </div>
        </fieldset>
      ))}
      <div className="button-row">
        <button
          type="button"
          className="primary"
          disabled={!complete || sending}
          onClick={() => void submit()}
        >
          {t('settings.sync.resolve')}
        </button>
      </div>
    </section>
  )
}

export function SyncTab() {
  const [running, setRunning] = useState<Running | null>(null)
  const [listening, setListening] = useState<{ addresses: string[]; code: string } | null>(null)
  // 相手を確かめたあとの段。書き始めた段(`finishing`)は止められない。
  const [stage, setStage] = useState<Stage | null>(null)
  const [conflicts, setConflicts] = useState<ConflictView[] | null>(null)
  const [report, setReport] = useState<SyncReport | null>(null)
  const [address, setAddress] = useState('')
  const [code, setCode] = useState('')
  const listenId = useId()
  const connectId = useId()
  // タブを離れたら(設定画面を閉じたときも)、進んでいる同期を止める。
  const runningRef = useRef(false)
  useEffect(
    () => () => {
      if (runningRef.current) void cancelSync()
    },
    [],
  )

  // 途中経過と結果は別の経路で届くので、結果のあとに届いた途中経過は捨てる。
  const onEvent = (event: SyncEvent) => {
    if (!runningRef.current) return
    switch (event.type) {
      case 'listening':
        setListening({ addresses: event.addresses, code: event.code })
        break
      case 'conflicts':
        setConflicts(event.conflicts)
        break
      default:
        setListening(null)
        setConflicts(null)
        setStage(event.type)
    }
  }

  const start = async (which: Running, sync: () => Promise<SyncReport>) => {
    setRunning(which)
    setReport(null)
    runningRef.current = true
    try {
      setReport(await sync())
    } finally {
      runningRef.current = false
      setRunning(null)
      setListening(null)
      setStage(null)
      setConflicts(null)
    }
  }

  const connect = (e: React.FormEvent) => {
    e.preventDefault()
    void start('connect', () => syncConnect(address, code, onEvent))
  }

  const status =
    stage === 'peer_choosing'
      ? t('settings.sync.peer_choosing')
      : stage !== null && conflicts === null
        ? t('settings.sync.syncing')
        : running === 'connect' && stage === null
          ? t('settings.sync.connecting')
          : null

  const stopButton = (
    // 書き始めたら止められない。確定をやり直して競合を選び直す間は、まだ書いていないので止められる。
    <button
      type="button"
      disabled={stage === 'finishing' && conflicts === null}
      onClick={() => void cancelSync()}
    >
      {t('settings.sync.stop')}
    </button>
  )

  return (
    <div className="settings-panel">
      <div className="settings-field">
        <p>{t('settings.sync.intro')}</p>
        <p>{t('settings.sync.intro_hint')}</p>
      </div>

      <section className="settings-section" aria-labelledby={listenId}>
        <h2 id={listenId}>{t('settings.sync.listen_heading')}</h2>
        <p className="settings-hint">{t('settings.sync.listen_hint')}</p>
        {listening && (
          <>
            <p>{t('settings.sync.listening')}</p>
            <div className="settings-field">
              <span>{t('settings.sync.address_label')}</span>
              <ul>
                {listening.addresses.map((a) => (
                  <li key={a}>
                    <code>{a}</code>
                  </li>
                ))}
              </ul>
            </div>
            <div className="settings-field">
              <span>{t('settings.sync.code_label')}</span>
              <code>{spacedCode(listening.code)}</code>
            </div>
          </>
        )}
        <div className="button-row">
          {running === 'listen' ? (
            stopButton
          ) : (
            <button
              type="button"
              className="primary"
              disabled={running !== null}
              onClick={() => void start('listen', () => syncListen(onEvent))}
            >
              {t('settings.sync.listen')}
            </button>
          )}
        </div>
      </section>

      <form
        className="settings-section settings-section-break"
        aria-labelledby={connectId}
        onSubmit={connect}
      >
        <h2 id={connectId}>{t('settings.sync.connect_heading')}</h2>
        <label className="settings-field">
          <span>{t('settings.sync.address_label')}</span>
          <input
            type="text"
            value={address}
            autoComplete="off"
            spellCheck={false}
            disabled={running !== null}
            onChange={(e) => setAddress(e.target.value)}
          />
        </label>
        <label className="settings-field">
          <span>{t('settings.sync.code_label')}</span>
          <input
            type="text"
            inputMode="numeric"
            value={code}
            autoComplete="off"
            disabled={running !== null}
            onChange={(e) => setCode(e.target.value)}
          />
        </label>
        <div className="button-row">
          {running === 'connect' ? (
            stopButton
          ) : (
            <button
              type="submit"
              className="primary"
              disabled={running !== null || address.trim() === '' || code.trim() === ''}
            >
              {t('settings.sync.connect')}
            </button>
          )}
        </div>
      </form>

      {conflicts && (
        <ConflictChooser
          conflicts={conflicts}
          // 送ったあとに次の競合(確定をやり直したとき)が届いていれば、そちらを残す。
          onDone={(sent) => setConflicts((c) => (c === sent ? null : c))}
        />
      )}
      {status && <p>{status}</p>}
      {report && (
        <p className={report.status === 'failed' ? 'error' : undefined}>{reportText(report)}</p>
      )}
    </div>
  )
}
