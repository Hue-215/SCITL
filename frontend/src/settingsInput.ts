// 設定画面の複数のタブが共有する入力の扱い。
import { useState } from 'react'
import type { ChangeEvent } from 'react'
import { isolated, t } from './i18n'

// フォーカスを外すと自動保存する数値入力。入力中は自身のstateだけを更新し、blur時にのみ親へ
// 確定した値を渡す。入力チェック(空欄は未設定、それ以外は1以上の整数)をこの1箇所に閉じ、
// 設定欄(NumberField)とモデル表のコンテキスト長の両方がこれを使う。
export function usePositiveIntegerInput(
  value: number | null,
  onSave: (value: number | null) => void,
) {
  const [text, setText] = useState(value?.toString() ?? '')
  const [invalid, setInvalid] = useState(false)

  // 保存された値が変わったら、入力欄をそれに戻す。描画の中で前回の値と比べて揃える。
  const [shown, setShown] = useState(value)
  if (shown !== value) {
    setShown(value)
    setText(value?.toString() ?? '')
    setInvalid(false)
  }

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

// 「1行1件、KEY=VALUE」形式のテキスト(HTTPヘッダーの欄)をパースする。エラーは行ごとに
// 個別指摘する。行は前後の空白を除いてから見るので、`=`が先頭でなければキーは空にならない。
export function parseKeyValueLines(text: string): { pairs: [string, string][]; errors: string[] } {
  const pairs: [string, string][] = []
  const errors: string[] = []
  text.split('\n').forEach((line, i) => {
    const trimmed = line.trim()
    if (trimmed === '') return
    const eq = trimmed.indexOf('=')
    if (eq <= 0) {
      errors.push(
        t('settings.tools.kv_line_invalid', {
          line_no: i + 1,
          line: isolated(trimmed),
          sample: t('settings.tools.kv_sample'),
        }),
      )
      return
    }
    pairs.push([trimmed.slice(0, eq).trim(), trimmed.slice(eq + 1).trim()])
  })
  return { pairs, errors }
}
