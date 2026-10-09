// 設定画面の「一般」タブ。
import { useEffect, useId, useState } from 'react'
import { exportMarkdown, getExportTarget, openExportFolder, updateGeneralSettings } from './api'
import type { ExportOutcome, ExportTarget, Language, SettingsView } from './types'
import Dropdown from './Dropdown'
import { currentLanguage, isolated, languageName, LANGUAGES, t } from './i18n'
import { NumberField } from './settingsFields'
import { useAsyncAction } from './useAsyncAction'

type GeneralUpdate = Parameters<typeof updateGeneralSettings>[0]

interface GeneralTabProps {
  settings: SettingsView
  // 欄の誤りは理由の文言で返る(数値の欄が欄の下に出す)。
  onSave: (update: GeneralUpdate) => Promise<string[]>
  onSaveLanguage: (language: Language) => void
}

interface PromptFieldProps {
  label: string
  value: string
  onChange: (value: string) => void
  onBlur: () => void
}

function PromptField({ label, value, onChange, onBlur }: PromptFieldProps) {
  return (
    <label className="settings-field">
      <span>{label}</span>
      <textarea value={value} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />
    </label>
  )
}

// フォーカスを外すと自動保存。入力中は自身のstateだけを更新し、blur時にのみ親へ確定した値を
// 渡す。
//
// 既定の文面を持つ欄は、未設定の間は既定の文面を表示する(書き換えの起点にできるように)。
// 空欄と既定の文面のままの値は、Rust側が未設定として保存する(`Settings::update_general`)。
export function GeneralTab({ settings, onSave, onSaveLanguage }: GeneralTabProps) {
  const { general } = settings
  const languageLabelId = useId()
  const [systemPrompt, setSystemPrompt] = useState(general.system_prompt ?? '')
  const [taskChatSystemPrompt, setTaskChatSystemPrompt] = useState(
    general.task_chat_system_prompt ?? general.default_task_chat_system_prompt,
  )
  const [taskOpeningMessage, setTaskOpeningMessage] = useState(
    general.task_opening_message ?? general.default_task_opening_message,
  )

  // 保存して設定が読み直されたら、入力欄を保存された値に戻す。描画の中で前回の値と比べて
  // 揃える(effectで揃えると、古い値で1回描いてから描き直す)。
  const [shown, setShown] = useState(general)
  if (shown !== general) {
    setShown(general)
    setSystemPrompt(general.system_prompt ?? '')
    setTaskChatSystemPrompt(
      general.task_chat_system_prompt ?? general.default_task_chat_system_prompt,
    )
    setTaskOpeningMessage(general.task_opening_message ?? general.default_task_opening_message)
  }

  const current = (): GeneralUpdate => ({
    systemPrompt: systemPrompt || null,
    taskChatSystemPrompt: taskChatSystemPrompt || null,
    taskOpeningMessage: taskOpeningMessage || null,
    // 数値の欄はその欄を書き換えたときだけ送る(プロンプトの保存で検証し直さない)。
    responseTimeoutSecs: null,
  })

  return (
    <div className="settings-panel">
      <div className="settings-field">
        <span id={languageLabelId}>{t('settings.general.language_label')}</span>
        <Dropdown
          labelledBy={languageLabelId}
          label={languageName(general.language)}
          options={LANGUAGES.map((language) => ({ key: language, label: languageName(language) }))}
          selectedKey={general.language}
          onSelect={(key) => onSaveLanguage(key as Language)}
          direction="down"
          align="start"
        />
        {/* 画面は起動時の言語で描かれているので、保存した言語と違う間だけ出す */}
        {general.language !== currentLanguage() && (
          <p className="settings-hint">{t('settings.general.language_restart_note')}</p>
        )}
        {general.unknown_language !== null && (
          <p className="settings-hint">
            {t('settings.general.language_unknown_note', {
              value: isolated(general.unknown_language),
            })}
          </p>
        )}
      </div>

      <PromptField
        label={t('settings.general.system_prompt_label')}
        value={systemPrompt}
        onChange={setSystemPrompt}
        onBlur={() => void onSave(current())}
      />

      <NumberField
        label={t('settings.general.timeout_label')}
        value={general.response_timeout_secs}
        defaultValue={general.default_response_timeout_secs}
        onSave={(secs) => onSave({ ...current(), responseTimeoutSecs: secs })}
      />

      <details className="settings-advanced">
        <summary>{t('settings.general.advanced_settings')}</summary>
        {/* <details>自体はflexにしないので(index.cssの.settings-advanced)、欄の間隔は
            中の入れ物のgapで持つ */}
        <div className="settings-section">
          <PromptField
            label={t('settings.general.task_chat_prompt_label')}
            value={taskChatSystemPrompt}
            onChange={setTaskChatSystemPrompt}
            onBlur={() => void onSave(current())}
          />
          <PromptField
            label={t('settings.general.task_opening_label')}
            value={taskOpeningMessage}
            onChange={setTaskOpeningMessage}
            onBlur={() => void onSave(current())}
          />
        </div>
      </details>

      <ExportSection />
    </div>
  )
}

// 押すと確認なしで書き出し(保存画面を出すOSでは、そこで場所を選ぶ)、成否はこの欄に出す。タブ全体の
// エラー欄を使わないのは、設定の保存とは別の操作の結果だから。フォルダを開く操作は、書き出す先が
// フォルダのOSでだけ出す(`ExportTarget`。Rust側が決める)。
function ExportSection() {
  const [target, setTarget] = useState<ExportTarget | null>(null)
  const [outcome, setOutcome] = useState<ExportOutcome | null>(null)
  const exporting = useAsyncAction((error) =>
    t('settings.general.export_failed', { error: isolated(error) }),
  )
  const opening = useAsyncAction((error) =>
    t('settings.general.open_export_folder_failed', { error: isolated(error) }),
  )

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
    void getExportTarget().then(setTarget)
  }, [])

  // 結果の欄には最後に行った操作の成否だけを出す。
  const runExport = () => {
    setOutcome(null)
    opening.clear()
    void exporting.run(exportMarkdown, setOutcome)
  }
  const openFolder = () => {
    setOutcome(null)
    exporting.clear()
    void opening.run(openExportFolder)
  }

  const summary = outcome?.status === 'written' ? outcome.summary : null
  return (
    <div className="settings-field settings-section-break">
      <span>{t('settings.general.export_label')}</span>
      <div className="button-row">
        <button type="button" onClick={runExport} disabled={exporting.running}>
          {exporting.running
            ? t('settings.general.exporting')
            : t('settings.general.export_button')}
        </button>
        {target === 'folder' && (
          <button type="button" onClick={openFolder}>
            {t('settings.general.open_export_folder')}
          </button>
        )}
      </div>
      {summary && (
        <p>
          {target === 'chosen_file'
            ? t('settings.general.export_saved')
            : t('settings.general.export_done', { folder: isolated(summary.folder) })}
        </p>
      )}
      {summary && summary.missing_attachments > 0 && (
        <p className="error">
          {t('settings.general.export_missing_attachments', {
            count: summary.missing_attachments,
          })}
        </p>
      )}
      {outcome?.status === 'left_incomplete' && (
        <p className="error">
          {t('settings.general.export_left_incomplete', { error: isolated(outcome.reason) })}
        </p>
      )}
      {exporting.error && <p className="error">{exporting.error}</p>}
      {opening.error && <p className="error">{opening.error}</p>}
    </div>
  )
}

