// 設定画面の「同期」タブ。この端末で待ち受けるか、待ち受けている端末につなぐかを選び、途中経過と
// 結果を出す。待ち受け・接続・写す順序と、始める前の確認はRust側が持つ(`scitl_sync::listen`)。
import { useEffect, useId, useRef, useState } from 'react'
import { cancelSync, syncConnect, syncListen } from './api'
import type { SyncEvent, SyncReport } from './types'
import { isolated, type MessageKey, t } from './i18n'

type Running = 'listen' | 'connect'

// 失敗の種類から文言を引き、英語の診断文を詳細として添える(コマンドの失敗と同じ形)。
function reportText(report: SyncReport): string {
  switch (report.status) {
    case 'sent':
      return t('settings.sync.sent', { ...report.counts })
    case 'received':
      return t('settings.sync.received', { ...report.counts })
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

export function SyncTab() {
  const [running, setRunning] = useState<Running | null>(null)
  const [listening, setListening] = useState<{ addresses: string[]; code: string } | null>(null)
  const [copying, setCopying] = useState(false)
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

  const onEvent = (event: SyncEvent) => {
    if (event.type === 'listening') {
      setListening({ addresses: event.addresses, code: event.code })
    } else {
      setListening(null)
      setCopying(true)
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
      setCopying(false)
    }
  }

  const connect = (e: React.FormEvent) => {
    e.preventDefault()
    void start('connect', () => syncConnect(address, code, onEvent))
  }

  const status = copying
    ? t('settings.sync.copying')
    : running === 'connect'
      ? t('settings.sync.connecting')
      : null

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
            <button type="button" onClick={() => void cancelSync()}>
              {t('settings.sync.stop')}
            </button>
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
            <button type="button" onClick={() => void cancelSync()}>
              {t('settings.sync.stop')}
            </button>
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

      {status && <p>{status}</p>}
      {report && (
        <p className={report.status === 'failed' ? 'error' : undefined}>{reportText(report)}</p>
      )}
    </div>
  )
}
