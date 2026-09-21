// 単色シードから明暗2種のカラースキームを生成し、CSS変数として適用する(Issue #79)。
// 役割(background/surface/text/primary...)ごとに彩度・明度を決め打ちし、シードの色相(と
// 危険色を除く彩度)だけを引き継ぐ。デザイントークンを1箇所に集約する方針
// (docs/spec/principles.md 6節)の一部。
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
  bg: { light: { saturationFactor: 0.08, lightness: 98 }, dark: { saturationFactor: 0.12, lightness: 9 } },
  surface: { light: { saturationFactor: 0.1, lightness: 95 }, dark: { saturationFactor: 0.14, lightness: 14 } },
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
  onPrimary: { light: { saturationFactor: 0, lightness: 100 }, dark: { saturationFactor: 0, lightness: 15 } },
  primaryContainer: {
    light: { saturationFactor: 0.55, lightness: 90 },
    dark: { saturationFactor: 0.4, lightness: 28 },
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

function buildPalette(seedHex: string, mode: 'light' | 'dark'): Record<string, string> {
  const { h, s } = hexToHueSaturation(seedHex)
  const palette: Record<string, string> = {}
  for (const [name, role] of Object.entries(ROLES)) {
    const { saturationFactor, lightness } = role[mode]
    palette[name] = `hsl(${h.toFixed(1)} ${(s * saturationFactor).toFixed(1)}% ${lightness}%)`
  }
  for (const [name, role] of Object.entries(DANGER_ROLES)) {
    const { saturation, lightness } = role[mode]
    palette[name] = `hsl(${DANGER_HUE} ${saturation}% ${lightness}%)`
  }
  return palette
}

function roleNameToCssVar(name: string): string {
  return `--color-${name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`
}

function applyPalette(palette: Record<string, string>): void {
  const root = document.documentElement.style
  for (const [name, value] of Object.entries(palette)) {
    root.setProperty(roleNameToCssVar(name), value)
  }
}

/** シードからテーマを適用し、OSの明暗設定の変更にも追従させる。 */
export function applyTheme(seedHex: string): void {
  const light = buildPalette(seedHex, 'light')
  const dark = buildPalette(seedHex, 'dark')
  const media = window.matchMedia('(prefers-color-scheme: dark)')
  const update = () => applyPalette(media.matches ? dark : light)
  update()
  media.addEventListener('change', update)
}
