// 設定画面の複数のタブが共有する入力の扱い。
import { useState } from 'react'
import type { ChangeEvent } from 'react'

// フォーカスを外すと自動保存する数値の欄。入力中は自身のstateだけを更新し、blur時にのみ欄の
// 文字列をそのまま親へ渡す。解釈(全角の数字・空欄は未設定・1以上の整数か)と検証はRust側が
// 行い、断られたら理由の文言(`onSave`が返す)を欄の下に出す。設定欄(NumberField)とモデル表の
// コンテキスト長の両方がこれを使う。
export function useNumberInput(value: number | null, onSave: (text: string) => Promise<string[]>) {
  const saved = value?.toString() ?? ''
  const [text, setText] = useState(saved)
  const [errors, setErrors] = useState<string[]>([])

  // 保存された値が変わったら、入力欄をそれに戻す。描画の中で前回の値と比べて揃える。
  const [shown, setShown] = useState(value)
  if (shown !== value) {
    setShown(value)
    setText(saved)
    setErrors([])
  }

  const save = async () => {
    // フォーカスが通り過ぎただけで保存しない(モデル表では行ごとに欄がある)。
    if (text === saved) {
      setErrors([])
      return
    }
    setErrors(await onSave(text))
  }

  return {
    errors,
    inputProps: {
      type: 'text',
      inputMode: 'numeric' as const,
      value: text,
      onChange: (e: ChangeEvent<HTMLInputElement>) => setText(e.target.value),
      onBlur: () => void save(),
      'aria-invalid': errors.length > 0,
    },
  }
}
