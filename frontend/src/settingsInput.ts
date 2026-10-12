// 設定画面の複数のタブが共有する入力の扱い。
import { createContext, useContext, useEffect, useRef, useState } from 'react'
import type { ChangeEvent } from 'react'

// 自動保存の欄の書きかけ。保存の契機はblurだが、blurが届かないまま欄が消える経路(Androidの
// 「戻る」、裏へ回したままの終了)があるので、設定画面を離れるときにもここから保存する。
export interface Drafts {
  // 書きかけのある欄の保存。断られた理由を返す(受け付けたら空)。
  entries: Set<() => Promise<string[]>>
  // 保存し終えて設定画面を閉じるところか。欄が消えるときに保存し直さない。
  closed: boolean
}

export const DraftsContext = createContext<Drafts | null>(null)

// 欄の値(`draft`)が保存済みの値(`saved`)と違う間、書きかけとして登録する。返す関数は`save`を
// 呼ぶが、同じ値の保存が進んでいる間はその結果を返す(「←」を押すと、blurと閉じる前の保存が
// 続けて来る)。画面を離れるのを待たせられずに欄が消えるとき(タブの切り替え・通知から会話を
// 開く)は、保存を投げるだけで結果を待たない。
export function useDraft(draft: string, saved: string, save: () => Promise<string[]>) {
  const drafts = useContext(DraftsContext)
  const dirty = draft !== saved
  const inFlight = useRef<{ draft: string; result: Promise<string[]> } | null>(null)

  const flush = () => {
    if (inFlight.current?.draft === draft) return inFlight.current.result
    const result = save()
    inFlight.current = { draft, result }
    const settled = () => {
      if (inFlight.current?.result === result) inFlight.current = null
    }
    result.then(settled, settled)
    return result
  }

  const latest = useRef({ dirty, flush })
  useEffect(() => {
    latest.current = { dirty, flush }
  })
  useEffect(() => {
    if (!dirty || drafts === null) return
    const entry = () => latest.current.flush()
    drafts.entries.add(entry)
    return () => {
      drafts.entries.delete(entry)
    }
  }, [dirty, drafts])
  useEffect(
    () => () => {
      if (latest.current.dirty && drafts?.closed !== true) void latest.current.flush()
    },
    [drafts],
  )

  return flush
}

// フォーカスを外すと自動保存する数値の欄。入力中は自身のstateだけを更新し、blur時(と設定画面を
// 離れるとき。`useDraft`)にのみ欄の文字列をそのまま親へ渡す。解釈(全角の数字・空欄は未設定・
// 1以上の整数か)と検証はRust側が行い、断られたら理由の文言(`onSave`が返す)を欄の下に出す。
// 設定欄(NumberField)とモデル表のコンテキスト長の両方がこれを使う。
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

  const flush = useDraft(text, saved, async () => {
    const rejected = await onSave(text)
    setErrors(rejected)
    return rejected
  })

  return {
    errors,
    inputProps: {
      type: 'text',
      inputMode: 'numeric' as const,
      value: text,
      onChange: (e: ChangeEvent<HTMLInputElement>) => setText(e.target.value),
      onBlur: () => {
        // フォーカスが通り過ぎただけで保存しない(モデル表では行ごとに欄がある)。
        if (text === saved) setErrors([])
        else void flush()
      },
      'aria-invalid': errors.length > 0,
    },
  }
}
