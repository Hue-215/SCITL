// 配布物のフォルダを組み立てる。release-build.ps1が、ビルドと検査の後に呼ぶ。
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
// Rustのクレートの一覧を作れるかだけを確かめる(CIと、ビルドを始める前のps1が使う)。何も残さない。
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const args = process.argv.slice(2)
const checkOnly = args.length === 1 && args[0] === '--check-licenses'
if (args.length === 0 || (!checkOnly && args.some((arg) => arg.startsWith('-')))) {
  console.error('usage: node scripts/assemble-dist.mjs <binary>... | --check-licenses')
  process.exit(2)
}

function fail(message) {
  console.error(message)
  process.exit(1)
}

// 途中経過は標準エラーへ流す(標準出力はフォルダの名前だけにする)。
function run(command, commandArgs) {
  try {
    return execFileSync(command, commandArgs, {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'inherit'],
      maxBuffer: 256 * 1024 * 1024,
    })
  } catch {
    fail(`failed: ${command} ${commandArgs.join(' ')}`)
  }
}

// ---- Rustのクレートのライセンス ----

// クレートの中の、ライセンス・著作権表示のファイル。
const LICENSE_FILE = /^(licen[sc]e|copying|copyright|notice|unlicense)/i

// 配布する対象(about.tomlの`targets`)に入るクレートを、cargo-aboutに洗い出させる。許容していない
// ライセンスの依存があると、ここで失敗する。配布しないクレートの依存も見るので、一覧は実行ファイルに
// 入るものより広い(漏れが無ければよい)。結果はファイルに書かせて読む(cargo-aboutは、PowerShellから
// 呼ばれると標準出力への書き出しを断る)。
function rustCrates() {
  const scratch = mkdtempSync(join(tmpdir(), 'scitl-licenses-'))
  let about
  try {
    const file = join(scratch, 'about.json')
    run('cargo', ['about', 'generate', '--locked', '--workspace', '--fail', '--format', 'json', '-o', file])
    about = JSON.parse(readFileSync(file, 'utf8'))
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
  const crates = new Map()
  for (const license of about.licenses) {
    for (const { crate } of license.used_by) {
      // このリポジトリのクレート(`source`が無い)は第三者ではない。
      if (crate.source) crates.set(crate.id, { crate, fallback: license })
    }
  }
  return [...crates.values()].sort((a, b) => a.crate.name.localeCompare(b.crate.name))
}

// 一覧の本文を作る。ライセンス文は、cargo-aboutが照合して選んだものではなく、各クレートに入っている
// ファイルをそのまま載せる(照合に外れると、著作権者の名前が入っていないひな形に置き換わるため)。
// 同じ文面は1度だけ載せ、どのクレートのものかを添える。ファイルを持たないクレートは、ライセンスの
// 名前と入手先を示し、cargo-aboutが出す標準の文面を載せる。
function rustLicenses() {
  const texts = new Map()
  const add = (text, label) => {
    const normalized = text.replace(/\r\n/g, '\n').trim()
    const key = createHash('sha256').update(normalized).digest('hex')
    if (!texts.has(key)) texts.set(key, { text: normalized, labels: [] })
    texts.get(key).labels.push(label)
  }
  const index = []
  for (const { crate, fallback } of rustCrates()) {
    const dir = dirname(crate.manifest_path)
    const files = readdirSync(dir)
      .filter((name) => LICENSE_FILE.test(name) && statSync(join(dir, name)).isFile())
      .sort()
    const where = crate.repository ?? `https://crates.io/crates/${crate.name}`
    index.push(`${crate.name} ${crate.version}  (${crate.license ?? 'see license file'})  ${where}`)
    for (const name of files) {
      add(readFileSync(join(dir, name), 'utf8'), `${crate.name} ${crate.version}: ${name}`)
    }
    if (files.length === 0) {
      add(fallback.text, `${crate.name} ${crate.version}: no license file in the package; standard text of ${fallback.id}`)
    }
  }
  const rule = '='.repeat(78)
  const sections = [...texts.values()].map(({ text, labels }) => [rule, ...labels, rule, '', text, ''].join('\n'))
  return [
    'Third-party licenses (Rust crates)',
    '',
    'SCITL Task Companion is built from the crates listed below. The license and notice',
    'files shipped in each crate follow the list, each headed by the crates it comes from.',
    'The source code of a crate is available at the address next to its name.',
    '',
    'The Rust standard library is also linked. It is distributed under MIT OR Apache-2.0',
    '(https://www.rust-lang.org/policies/licenses).',
    '',
    ...index,
    '',
    ...sections,
  ].join('\n')
}

if (checkOnly) {
  rustLicenses()
  process.exit(0)
}

// ---- 配布物のフォルダ ----

// 版はワークスペースで1つ(Cargo.tomlの`[workspace.package]`)。
const metadata = JSON.parse(run('cargo', ['metadata', '--no-deps', '--locked', '--format-version', '1']))
const version = metadata.packages.find((p) => p.name === 'scitl-tauri').version
const os = { win32: 'windows', linux: 'linux' }[process.platform] ?? process.platform
const name = `scitl-${version}-${os}-${process.arch}`
const dist = join(root, 'target', 'dist', name)

// 前の配布物を消す前に、要るものが揃っているかを確かめる(一覧の生成もここで済ませる)。
const copies = [
  ...args.map((binary) => [binary, basename(binary)]),
  [join(root, 'LICENSE'), 'LICENSE'],
  // 画面のバンドルに入ったnpmのパッケージ(フロントエンドのビルドが出す)と、同梱フォント。
  [join(root, 'frontend', 'dist', '.vite', 'license.md'), join('THIRD-PARTY-LICENSES', 'frontend.md')],
  [join(root, 'frontend', 'public', 'fonts', 'NotoJP-LICENSE.txt'), join('THIRD-PARTY-LICENSES', 'NotoJP-LICENSE.txt')],
]
for (const [from] of copies) {
  if (!existsSync(from)) fail(`missing: ${from}`)
}
const rust = rustLicenses()

rmSync(dist, { recursive: true, force: true })
mkdirSync(join(dist, 'THIRD-PARTY-LICENSES'), { recursive: true })
for (const [from, to] of copies) {
  copyFileSync(from, join(dist, to))
}
writeFileSync(join(dist, 'THIRD-PARTY-LICENSES', 'rust.txt'), rust)

console.log(name)
