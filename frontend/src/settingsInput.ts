// 設定画面の複数のタブが共有する入力の扱い。
import { useEffect, useState } from 'react'
import type { ChangeEvent } from 'react'
import { failureText } from './api'
import { t } from './i18n'

// httpの許可範囲(crates/scitl-core/src/net.rsのclassify_host)が変わったときに
// 片方だけ直し忘れないよう、URLを入力させる箇所で共通のヒント文を使う。
export function httpPlainTextHint(secret: string): string {
  return t('settings.http_warning', { secret })
}

// フォーカスを外すと自動保存する数値入力(legacy/frontend.md 2節・4節)。入力中は自身の
// stateだけを更新し、blur時にのみ親へ確定した値を渡す。入力チェック(空欄は未設定、
// それ以外は1以上の整数)をこの1箇所に閉じ、設定欄(NumberField)とモデル表の
// コンテキスト長の両方がこれを使う(ui.md 1節)。
export function usePositiveIntegerInput(
  value: number | null,
  onSave: (value: number | null) => void,
) {
  const [text, setText] = useState(value?.toString() ?? '')
  const [invalid, setInvalid] = useState(false)

  useEffect(() => {
    setText(value?.toString() ?? '')
    setInvalid(false)
  }, [value])

  const save = () => {
    // IMEを切り忘れて打った全角の数字も受け付ける。
    const trimmed = text.normalize('NFKC').trim()
    const parsed = trimmed === '' ? null : Number(trimmed)
    if (parsed !== null && (!Number.isInteger(parsed) || parsed <= 0)) {
      setInvalid(true)
      return
    }
    setInvalid(false)
    // フォーカスが通り過ぎただけで保存しない(モデル表では行ごとに欄がある)。
    if (parsed !== value) onSave(parsed)
  }

  return {
    invalid,
    inputProps: {
      type: 'text',
      inputMode: 'numeric' as const,
      value: text,
      onChange: (e: ChangeEvent<HTMLInputElement>) => setText(e.target.value),
      onBlur: save,
      'aria-invalid': invalid,
    },
  }
}

// 追加フォームの送信。結果を待ち、成功したときだけ`onDone`で入力を空にする。失敗は
// フォームの直下に出し、入力は残す(Rust側の検証で弾かれても打ち直さずに済むように)。
export function useAddSubmission() {
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = async (add: () => Promise<unknown>, onDone: () => void) => {
    setAdding(true)
    setError(null)
    try {
      await add()
      onDone()
    } catch (e) {
      setError(failureText(e))
    } finally {
      setAdding(false)
    }
  }

  return { adding, error, run }
}
