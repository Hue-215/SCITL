/**
 * 貼り付けたクリップボードの画像を、`clipboardData`ではなく非同期のClipboard APIから読み直す
 * 必要があるか。WebKitGTKは、画像だけが載ったクリップボードを貼り付けても`clipboardData`に
 * ファイルを入れない(2.52.6で確認)。LinuxのTauriは必ずWebKitGTKを使う。ほかのWebView
 * (WebView2・WKWebView)は画像を`clipboardData`に入れ、Clipboard APIで読むと許可を求めうるので
 * 読まない。
 */
export const PASTED_IMAGES_NEED_READING = navigator.userAgent.includes('Linux')

// 貼り付けた画像に付ける名前の拡張子。種別はRust側が中身から決めるので、表示のためだけに使う。
const EXTENSIONS: Record<string, string> = {
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/gif': 'gif',
  'image/webp': 'webp',
}

/**
 * 貼り付けたクリップボードの画像を、非同期のClipboard APIで読む。WebKitGTKでは、貼り付けの
 * 操作の中から呼べば許可を求められずに読める(2.52.6で確認)。読めなかった項目は飛ばす。
 */
export async function readClipboardImages(): Promise<File[]> {
  let items: ClipboardItems
  try {
    items = await navigator.clipboard.read()
  } catch {
    return []
  }
  const files: File[] = []
  for (const item of items) {
    const type = item.types.find((t) => t.startsWith('image/'))
    if (type === undefined) continue
    try {
      const blob = await item.getType(type)
      // 名前はChromiumが貼り付けた画像に付けるものに揃える。
      const extension = EXTENSIONS[type]
      files.push(new File([blob], extension ? `image.${extension}` : 'image', { type }))
    } catch {
      continue
    }
  }
  return files
}
