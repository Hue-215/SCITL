// 単色シードから明暗2種のカラースキームを生成し、CSS変数として適用する。役割
// (background/surface/text/primary...)ごとに彩度・明度を決め打ちし、シードの色相(と危険色を
// 除く彩度)だけを引き継ぐ。奥行きの影(--shadow-*)も明暗で中身が変わるため、同じ切り替えに
// 乗せてここで発行する。
//
// シードの色味が乗るのは背景・面・境界線・プライマリだけで、文字(on*を含む)は
// 無彩色で固定する。文字に色を付けると読みにくく、かつシードの選び方で
// 読みやすさが変わってしまうため。例外はエラーの赤(DANGER_ROLES)だけ。

type Role = {
  /** シード彩度に掛ける係数。0なら無彩色。 */
  saturationFactor: number
  lightness: number
}

type RoleSet = Record<string, { light: Role; dark: Role }>

// 危険色ロール: シードに依存しない固定色相を使うため、彩度は係数ではなく
// パーセント値そのものを持つ(Roleの saturationFactor とは単位が異なる)。
type DangerRole = {
  /** 彩度(%)そのもの。RoleのsaturationFactorと違い、シード彩度には掛けない。 */
  saturation: number
  lightness: number
}

type DangerRoleSet = Record<string, { light: DangerRole; dark: DangerRole }>

// 危険色(エラー表示)はシードから独立した固定色相を使う。シードが何色でも
// 「エラーは赤系」という読み取りやすさを保つため。
const DANGER_HUE = 4

const ROLES = {
  // ダークの背景は、有機ELの画面で黒の画素を消灯させるため純黒にする。
  bg: { light: { saturationFactor: 0.08, lightness: 98 }, dark: { saturationFactor: 0, lightness: 0 } },
  // surfaceは沈んだ部品(入力欄・選択中の項目)、surfaceAltは浮いた部品(ボタン)の塗り。
  // どちらも背景(bg)との差で部品の輪郭を作り、影は奥行きの補助に留める(ui.md「塗りと奥行き」)。
  surface: { light: { saturationFactor: 0.1, lightness: 94 }, dark: { saturationFactor: 0.14, lightness: 14 } },
  surfaceAlt: { light: { saturationFactor: 0.12, lightness: 90 }, dark: { saturationFactor: 0.16, lightness: 21 } },
  border: { light: { saturationFactor: 0.14, lightness: 82 }, dark: { saturationFactor: 0.16, lightness: 32 } },
  // 文字ロールは無彩色に固定する(saturationFactor: 0)。文字に色味が乗ると読みにくく、
  // シード次第で読みやすさが変わってしまうため。明暗の反転(ライトで黒・ダークで白)は
  // 明度側で保つ。エラーの赤だけはDANGER_ROLES側で別に持つ。
  text: { light: { saturationFactor: 0, lightness: 15 }, dark: { saturationFactor: 0, lightness: 94 } },
  textSecondary: {
    light: { saturationFactor: 0, lightness: 42 },
    dark: { saturationFactor: 0, lightness: 68 },
  },
  primary: { light: { saturationFactor: 1, lightness: 45 }, dark: { saturationFactor: 0.85, lightness: 70 } },
  primaryContainer: {
    light: { saturationFactor: 0.55, lightness: 90 },
    dark: { saturationFactor: 0.4, lightness: 28 },
  },
  // 主ボタン(タスクを追加・送信)の塗り。押せる所が目に付くよう、ユーザーの吹き出し
  // (primaryContainer)より彩度を上げ、明度をHSLで最も鮮やかな50%の側へ寄せる。文字は
  // onPrimaryContainerを使う。
  primaryButton: {
    light: { saturationFactor: 1, lightness: 82 },
    dark: { saturationFactor: 0.8, lightness: 38 },
  },
  onPrimaryContainer: {
    light: { saturationFactor: 0, lightness: 20 },
    dark: { saturationFactor: 0, lightness: 92 },
  },
} satisfies RoleSet

// 危険色は独自の色相を持つため、彩度は(シードではなく)固定値からの相対にする。
const DANGER_ROLES = {
  danger: { light: { saturation: 70, lightness: 42 }, dark: { saturation: 65, lightness: 68 } },
  dangerContainer: { light: { saturation: 75, lightness: 92 }, dark: { saturation: 40, lightness: 26 } },
  onDangerContainer: {
    light: { saturation: 65, lightness: 24 },
    dark: { saturation: 35, lightness: 92 },
  },
} satisfies DangerRoleSet

// 奥行きの表現。ライトは影、ダークは影が背景に沈んで見えないため縁の光で代える。
// 浮き(raise)は押せる部品、沈み(sink)は値を入れる所と選択中の項目、持ち上げ(lift)は
// 押せない面(吹き出し)に使う。liftの強さは吹き出しが背景から離れて見える量で決めており、
// raiseより強い(押せる部品とは大きさと置き場所で見分けられる)。留め(pinned)はスクロールする
// 中身の上に留める見出しの帯に使い、上と左右は窓の端で切れるので、下端に落とす影だけが見える。
type Elevation = { raise: string; sink: string; lift: string; pinned: string; focus: string }

function buildElevation(h: number, s: number, mode: 'light' | 'dark'): Elevation {
  const hue = h.toFixed(1)
  if (mode === 'light') {
    // 影の色は純粋な黒より背景になじむよう、シードの色相を薄く残す。
    const shade = (alpha: number) => `hsl(${hue} ${(s * 0.24).toFixed(1)}% 20% / ${alpha})`
    const lift = `0 2px 5px -1px ${shade(0.24)}`
    return {
      raise: `0 2px 4px -1px ${shade(0.19)}`,
      sink: `inset 0 2px 4px -2px ${shade(0.34)}, inset 0 1px 1px ${shade(0.1)}`,
      lift,
      // 見出しの帯は吹き出しと同じだけ浮かせる。
      pinned: lift,
      focus: `0 0 var(--focus-glow-spread) hsl(${hue} ${s.toFixed(1)}% 50% / 0.45)`,
    }
  }
  const light = (alpha: number) => `hsl(0 0% 100% / ${alpha})`
  return {
    raise: `inset 0 1px 0 ${light(0.16)}`,
    sink: `inset 0 -1px 0 ${light(0.12)}`,
    lift: `inset 0 1px 0 ${light(0.1)}`,
    // 帯で見えるのは下端だけで、縁の光の理屈では光の当たらない側になる。光ではなく影を落とす。
    // 純黒の背景の上では見えず、帯の下へ潜った中身(吹き出し等)の上にだけ見える。
    pinned: `0 2px 5px -1px hsl(0 0% 0% / 0.6)`,
    focus: `0 0 var(--focus-glow-spread) hsl(${hue} ${s.toFixed(1)}% 65% / 0.5)`,
  }
}

function hexToHueSaturation(hex: string): { h: number; s: number } {
  const r = parseInt(hex.slice(1, 3), 16) / 255
  const g = parseInt(hex.slice(3, 5), 16) / 255
  const b = parseInt(hex.slice(5, 7), 16) / 255
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  const l = (max + min) / 2
  if (max === min) return { h: 0, s: 0 }
  const d = max - min
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
  let h: number
  switch (max) {
    case r:
      h = (g - b) / d + (g < b ? 6 : 0)
      break
    case g:
      h = (b - r) / d + 2
      break
    default:
      h = (r - g) / d + 4
  }
  return { h: h * 60, s: s * 100 }
}

function roleNameToCssVar(name: string): string {
  return `--color-${name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`
}

/** 色(--color-*)と奥行き(--shadow-*)をまとめて、CSS変数名から値への対応にする。 */
function buildThemeVars(seedHex: string, mode: 'light' | 'dark'): Record<string, string> {
  const { h, s } = hexToHueSaturation(seedHex)
  const vars: Record<string, string> = {}
  for (const [name, role] of Object.entries(ROLES)) {
    const { saturationFactor, lightness } = role[mode]
    vars[roleNameToCssVar(name)] = `hsl(${h.toFixed(1)} ${(s * saturationFactor).toFixed(1)}% ${lightness}%)`
  }
  for (const [name, role] of Object.entries(DANGER_ROLES)) {
    const { saturation, lightness } = role[mode]
    vars[roleNameToCssVar(name)] = `hsl(${DANGER_HUE} ${saturation}% ${lightness}%)`
  }
  for (const [name, value] of Object.entries(buildElevation(h, s, mode))) {
    vars[`--shadow-${name}`] = value
  }
  return vars
}

function applyVars(vars: Record<string, string>): void {
  const root = document.documentElement.style
  for (const [name, value] of Object.entries(vars)) {
    root.setProperty(name, value)
  }
}

/** シードからテーマを適用し、OSの明暗設定の変更にも追従させる。 */
export function applyTheme(seedHex: string): void {
  const light = buildThemeVars(seedHex, 'light')
  const dark = buildThemeVars(seedHex, 'dark')
  const media = window.matchMedia('(prefers-color-scheme: dark)')
  const update = () => applyVars(media.matches ? dark : light)
  update()
  media.addEventListener('change', update)
}
