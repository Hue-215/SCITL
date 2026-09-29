import { useState } from 'react'
import { failureText } from './api'

/**
 * 非同期の操作1つの実行中と失敗を持つ(追加フォームの送信・一覧の取得・書き出し等)。
 * `run`は実行中の間`running`を立て、成功したときだけ結果を`onDone`へ渡す。失敗は`describe`で
 * 画面の文言にして`error`に持ち、次に実行するまで残す(操作ごとにその場へ出すため、画面全体の
 * エラー欄には流さない)。
 */
export function useAsyncAction(describe: (error: string) => string = (error) => error) {
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = async <T>(action: () => Promise<T>, onDone?: (result: T) => void) => {
    setRunning(true)
    setError(null)
    try {
      const result = await action()
      onDone?.(result)
    } catch (e) {
      setError(describe(failureText(e)))
    } finally {
      setRunning(false)
    }
  }

  return { running, error, run }
}
