// 配布物のフォルダを組み立てる。release-build.sh・release-build.ps1が、ビルドと検査の後に呼ぶ。
// 手順は.claude/skills/release-build/SKILL.md。
//
//   node scripts/assemble-dist.mjs <実行ファイル>...
//
// `target/dist/scitl-<版>-<OS>-<CPU>/`を作り直し、渡された実行ファイル、このリポジトリのLICENSE、
// 第三者ライセンスの一覧(THIRD-PARTY-LICENSES/)を入れて、フォルダの名前を標準出力に書く
// (名前はASCIIだけなので、呼び出し側の文字コードに左右されない)。圧縮は呼び出し側が行う。
//
//   node scripts/assemble-dist.mjs --check-licenses
//
// Rustのクレートの一覧を作れるかだけを確かめる(CIが使う)。何も残さない。
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const binaries = process.argv.slice(2)
if (binaries.length === 0) {
  console.error('usage: node scripts/assemble-dist.mjs <binary>... | --check-licenses')
  process.exit(2)
}

// 途中経過は標準エラーへ流す(標準出力はフォルダの名前だけにする)。
function run(command, args) {
  return execFileSync(command, args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] })
}

// Rustのクレートのライセンスの一覧を書き出す。許容していないライセンスの依存があると失敗する
// (about.toml)。
function writeRustLicenses(file) {
  run('cargo', ['about', 'generate', '--locked', '--workspace', '--fail', '-o', file, 'about.hbs'])
}

if (binaries[0] === '--check-licenses') {
  const scratch = mkdtempSync(join(tmpdir(), 'scitl-licenses-'))
  try {
    writeRustLicenses(join(scratch, 'rust.html'))
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
  process.exit(0)
}

// 版はワークスペースで1つ(Cargo.tomlの`[workspace.package]`)。
const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--locked', '--format-version', '1']))
const version = metadata.packages.find((p) => p.name === 'scitl-tauri').version
const os = { win32: 'windows', linux: 'linux' }[process.platform] ?? process.platform
const name = `scitl-${version}-${os}-${process.arch}`
const dist = join(root, 'target', 'dist', name)
const licenses = join(dist, 'THIRD-PARTY-LICENSES')

rmSync(dist, { recursive: true, force: true })
mkdirSync(licenses, { recursive: true })

// 無いファイルは、何が無いかを言って止める(ビルドの前に呼ばれた場合など)。
function copy(from, to) {
  if (!existsSync(from)) {
    console.error(`missing: ${from}`)
    process.exit(1)
  }
  copyFileSync(from, to)
}

for (const binary of binaries) {
  copy(binary, join(dist, basename(binary)))
}
copy(join(root, 'LICENSE'), join(dist, 'LICENSE'))

writeRustLicenses(join(licenses, 'rust.html'))
// 画面のバンドルに入ったnpmのパッケージ(フロントエンドのビルドが出す)と、同梱フォント。
copy(join(root, 'frontend', 'dist', '.vite', 'license.md'), join(licenses, 'frontend.md'))
copy(join(root, 'frontend', 'public', 'fonts', 'NotoJP-LICENSE.txt'), join(licenses, 'NotoJP-LICENSE.txt'))

console.log(name)
