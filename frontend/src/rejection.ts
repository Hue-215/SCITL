// 入力を受け付けなかった理由(Rust側が種類で返す)を、表示言語の文言にする。判定はRust側
// (`settings::InputRejection`)だけが行い、画面は種類から文言を引くだけにする。
import { isolated, t } from './i18n'
import type { InputRejection } from './types'

export function rejectionText(reason: InputRejection): string {
  switch (reason.kind) {
    case 'not_positive_integer':
      return t('errors.positive_integer')
    case 'number_too_large':
      return t('errors.number_too_large', { max: reason.max })
    case 'mcp_server_name_required':
      return t('settings.tools.id_required')
    case 'mcp_server_name_invalid':
      return t('settings.tools.id_invalid', { max: reason.max_chars })
    case 'mcp_server_name_taken':
      return t('settings.tools.id_duplicate', { id: isolated(reason.name) })
    case 'url_required':
      return t('settings.tools.url_required')
    case 'header_line_invalid':
      return t('settings.tools.kv_line_invalid', {
        line_no: reason.line_no,
        sample: t('settings.tools.kv_sample'),
      })
  }
}

// 追加のフォームの送信の結果。登録しなかったとき、欄の誤りがあれば`errors`に文言が入る
// (確認のダイアログで取りやめたときは空)。フォームは登録したときだけ入力を空にする。
export interface AddResult {
  added: boolean
  errors: string[]
}
