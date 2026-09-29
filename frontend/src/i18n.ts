import en from '../../lang/en.json'
import ja from '../../lang/ja.json'
import type { Language } from './types'

// 画面の文言を言語ファイル(lang/*.json)から引く。
//
// 表示言語はプロセスの間変わらない(切り替えは再起動で反映する)。
// そのため`t()`はフックではなくモジュールの関数にし、描画の前に`initI18n`で1度だけ
// 決める。Reactの外(remarkプラグイン等)からも同じ関数で引ける。
//
// モジュールの最上位で`t()`を呼ばないこと。importは`initI18n`より先に評価されるため、
// 表示言語が決まる前の文言になる。定数にはキーを持たせ、描画のときに引く。

export type MessageKey = keyof typeof ja

// 正本(すべてのキーを持つ)で、未設定のときの表示言語。core側の`Language::DEFAULT`と同じ
// 言語を指す。キーと差し込む値の名前が言語間で揃っていることは、core側のテスト
// (scitl_core::i18n)が確かめる。
export const DEFAULT_LANGUAGE: Language = 'ja'

const CATALOGS: Record<Language, Record<string, string>> = { ja, en }

export const LANGUAGES = Object.keys(CATALOGS) as Language[]

interface Current {
  language: Language
  // 正本の上に表示言語を重ねたもの。表示言語に無いキーは正本の文言になる。
  messages: Map<string, string>
  dateTime: Intl.DateTimeFormat
}

let current: Current | null = null

export function initI18n(language: Language): void {
  current = {
    language,
    messages: new Map(Object.entries({ ...CATALOGS[DEFAULT_LANGUAGE], ...CATALOGS[language] })),
    dateTime: new Intl.DateTimeFormat(language, {
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    }),
  }
  document.documentElement.lang = language
}

function state(): Current {
  if (current === null) {
    // 最上位で`t()`を呼んだ箇所を、開発中に起動した時点で見つけるため。
    if (import.meta.env.DEV) throw new Error('t() was called before initI18n()')
    initI18n(DEFAULT_LANGUAGE)
  }
  return current!
}

const PLACEHOLDER = /\{([A-Za-z0-9_]+)\}/g

function fill(text: string, params: Record<string, string | number>): string {
  if (!text.includes('{')) return text
  // 1回の走査で置き換える。差し込んだ値に`{name}`が含まれていても、それは置き換えない。
  return text.replace(PLACEHOLDER, (match, name: string) => {
    const value = params[name]
    if (value !== undefined) return String(value)
    // 渡し忘れは画面に`{name}`のまま残して気付けるようにする。
    if (import.meta.env.DEV) console.error(`missing placeholder {${name}} in "${text}"`)
    return match
  })
}

/**
 * 文言に差し込む、モデル・ユーザー由来の値。双方向制御文字を含んでいても、文言の残りの並び
 * 順を入れ替えないよう、分離の制御文字(FSI・PDI)で閉じ込める(ui.md「部品ごとの決まり」)。
 */
export function isolated(value: string): string {
  return `\u2068${value}\u2069`
}

/** `key`の文言。表示言語に無ければ正本、そこにも無ければキーそのものを返す。 */
export function t(key: MessageKey, params: Record<string, string | number> = {}): string {
  return fill(state().messages.get(key) ?? key, params)
}

export function currentLanguage(): Language {
  return state().language
}

/** 言語の選択肢に出す名前。どの表示言語でも、その言語自身での呼び名にする。 */
export function languageName(language: Language): string {
  return CATALOGS[language]['meta.name']
}

export function formatDateTime(iso: string): string {
  return state().dateTime.format(new Date(iso))
}

const BYTE_UNITS = ['byte', 'kilobyte', 'megabyte', 'gigabyte'] as const

/** ファイルの大きさ。単位の刻みは1024(上限の値がKiB・MiB単位で決めてあるため)。 */
export function formatBytes(bytes: number): string {
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return new Intl.NumberFormat(state().language, {
    style: 'unit',
    unit: BYTE_UNITS[unit],
    maximumFractionDigits: unit === 0 ? 0 : 1,
  }).format(value)
}

/**
 * エラー発言(role='error')の本文。保存された`content`は英語の定型文言なので、画面は種別
 * コードから表示言語の文言を引く。知らない種別の行(別の版で保存された等)は
 * `content`をそのまま出す。キーの組み立て方はcoreの`turn_error`と同じ。
 */
export function turnErrorText(errorKind: string | null, content: string): string {
  if (errorKind === null) return content
  return state().messages.get(`turn_error.${errorKind}`) ?? content
}
