// 配布物のフォルダを組み立てる。release-build.ps1・release-build.shが、ビルドと検査の後に呼ぶ。
// 手順は.claude/skills/release-build/SKILL.md。
//
//   node scripts/assemble-dist.mjs <実行ファイル>...
//
// `target/dist/scitl-<版>-<OS>-<CPU>/`を作り直し、渡された実行ファイル、このリポジトリのREADME.md、
// このリポジトリのLICENSE、第三者ライセンスの一覧(THIRD-PARTY-LICENSES/)を入れて、フォルダの名前を標準出力に書く
// (名前はASCIIだけなので、呼び出し側の文字コードに左右されない)。圧縮は呼び出し側が行う。
//
//   node scripts/assemble-dist.mjs --check-licenses
//
// Rustのクレートの一覧を作れるかだけを確かめる(CIと、ビルドを始める前のスクリプトが使う)。何も残さない。
//
//   node scripts/assemble-dist.mjs --check-frontend-licenses
//
// 画面のバンドルに入ったnpmのパッケージの一覧を作れるかだけを確かめる(フロントエンドのビルドの後に
// CIが使う)。何も残さない。
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
const CHECKS = ['--check-licenses', '--check-frontend-licenses']
const check = args.length === 1 && CHECKS.includes(args[0]) ? args[0] : null
if (args.length === 0 || (!check && args.some((arg) => arg.startsWith('-')))) {
  console.error(`usage: node scripts/assemble-dist.mjs <binary>... | ${CHECKS.join(' | ')}`)
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

// ---- 許容するライセンス ----

// about.tomlの`accepted`。npmのパッケージもRustのクレートと同じ範囲で許容する。コメント(`#`以降)を
// 先に除くので、コメントアウトしたライセンスは許容に入らない。
function acceptedLicenses() {
  const toml = readFileSync(join(root, 'about.toml'), 'utf8')
    .split('\n')
    .map((line) => line.replace(/#.*/, ''))
    .join('\n')
  const list = toml.match(/^accepted\s*=\s*\[([^\]]*)\]/m)
  if (!list) fail('about.toml: accepted = [...] not found')
  return new Set([...list[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]))
}

// SPDXの式(`MIT OR Apache-2.0`・`(MIT AND Zlib)`・`Apache-2.0 WITH LLVM-exception`)が、許容する
// ライセンスだけで満たせるか。ORはどれか1つ、ANDはすべてを満たす。読めない式は満たさないものとする。
function satisfies(expression, accepted) {
  const tokens = expression.match(/\(|\)|[^\s()]+/g) ?? []
  let pos = 0
  const peek = () => tokens[pos]
  const term = () => {
    if (peek() === '(') {
      pos++
      const value = or()
      if (tokens[pos++] !== ')') throw new Error('unbalanced')
      return value
    }
    let id = tokens[pos++]
    if (id === undefined || ['AND', 'OR', 'WITH', ')'].includes(id)) throw new Error('unexpected')
    if (peek() === 'WITH') {
      pos++
      id = `${id} WITH ${tokens[pos++]}`
    }
    return accepted.has(id)
  }
  const and = () => {
    let value = term()
    while (peek() === 'AND') {
      pos++
      value = term() && value
    }
    return value
  }
  const or = () => {
    let value = and()
    while (peek() === 'OR') {
      pos++
      value = and() || value
    }
    return value
  }
  try {
    const value = or()
    return pos === tokens.length && value
  } catch {
    return false
  }
}

// ---- Rustのクレートのライセンス ----

// クレートの中の、ライセンス・著作権表示のファイル。
const LICENSE_FILE = /^(licen[sc]e|copying|copyright|notice|unlicense)/i

// パッケージにライセンスファイルを持たないクレートに、代わりに載せるファイルの置き場所(licenses/の
// 下のフォルダ。上流のリポジトリから写したもの。入手先はlicenses/README.md)。
const SUPPLIED = {
  'alloc-stdlib': 'alloc-stdlib',
  'clipboard-win': 'clipboard-win',
  dlopen2: 'dlopen2',
  dlopen2_derive: 'dlopen2',
  rmcp: 'rmcp',
  'unic-char-property': 'unic',
  'unic-char-range': 'unic',
  'unic-common': 'unic',
  'unic-ucd-ident': 'unic',
  'unic-ucd-version': 'unic',
  'webview2-com': 'webview2-com',
  'webview2-com-macros': 'webview2-com',
  'webview2-com-sys': 'webview2-com',
}

// 上流にもライセンスファイルが無く、cargo-aboutの標準の文面で足りるクレート(MPL-2.0の文面は
// 著作権者を含まない)。
const STANDARD_TEXT = new Set(['selectors'])

// Rustの標準ライブラリと、それと一緒に実行ファイルに入るクレートのうち、依存の一覧に現れないもの
// (cargo-aboutは標準ライブラリの中を見ない)。licenses/の下のフォルダの写しを載せる。標準ライブラリが
// 使うほかのクレート(hashbrown・miniz_oxide・libc等)は、依存としても入っていて一覧に載る(版は違いうるが、
// ライセンス文は同じ)。
const STD = [
  ['Rust standard library', 'rust'],
  ['addr2line', 'addr2line'],
  ['compiler_builtins', 'compiler_builtins'],
  ['gimli', 'gimli'],
  ['object', 'object'],
  ['rustc-demangle', 'rustc-demangle'],
]

// クレートのライセンスとは別に、そのクレートが実行ファイルに入れる第三者のもの。
const BUNDLED = {
  'webview2-com-sys': {
    dir: 'webview2-sdk',
    what: 'Microsoft WebView2 SDK, whose loader (WebView2LoaderStatic.lib) is linked into the Windows executable',
  },
}

function licenseFiles(dir) {
  return readdirSync(dir)
    .filter((name) => LICENSE_FILE.test(name) && statSync(join(dir, name)).isFile())
    .sort()
}

// licenses/の下のフォルダの、ライセンスファイル。写し忘れ・名前の打ち間違いで空なら止める。
function suppliedFiles(name) {
  const dir = join(root, 'licenses', name)
  const files = existsSync(dir) ? licenseFiles(dir) : []
  if (files.length === 0) fail(`no license files in licenses/${name}`)
  return files.map((file) => [file, readFileSync(join(dir, file), 'utf8')])
}

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
  const unsupplied = []
  const stale = new Set(Object.keys(SUPPLIED))
  const unbundled = new Set(Object.keys(BUNDLED))
  for (const { crate, fallback } of rustCrates()) {
    const label = `${crate.name} ${crate.version}`
    const dir = dirname(crate.manifest_path)
    const where = crate.repository ?? `https://crates.io/crates/${crate.name}`
    index.push(`${label}  (${crate.license ?? 'see license file'})  ${where}`)
    const files = licenseFiles(dir)
    for (const name of files) {
      add(readFileSync(join(dir, name), 'utf8'), `${label}: ${name}`)
    }
    if (files.length === 0 && SUPPLIED[crate.name]) {
      stale.delete(crate.name)
      for (const [name, text] of suppliedFiles(SUPPLIED[crate.name])) {
        add(text, `${label}: ${name} (from the upstream repository)`)
      }
    } else if (files.length === 0) {
      if (!STANDARD_TEXT.has(crate.name)) unsupplied.push(label)
      add(fallback.text, `${label}: no license file in the package; standard text of ${fallback.id}`)
    }
    const bundled = BUNDLED[crate.name]
    if (bundled) {
      unbundled.delete(crate.name)
      for (const [name, text] of suppliedFiles(bundled.dir)) {
        add(text, `${label}: ${name} of the ${bundled.what}`)
      }
    }
  }
  // 写しが無いクレートが増えたら、標準の文面で済ませずに止める(著作権者の名前が載らないため)。
  // 写しが要らなくなったものも止めて、SUPPLIEDとlicenses/を整理させる。
  if (unsupplied.length > 0) {
    fail(
      `crates without license files: ${unsupplied.join(', ')}\n` +
        'copy their license files from the upstream repository into licenses/ and add them to SUPPLIED',
    )
  }
  if (stale.size > 0) {
    fail(`SUPPLIED lists crates that no longer need it: ${[...stale].join(', ')}`)
  }
  if (unbundled.size > 0) {
    fail(`BUNDLED lists crates that are no longer dependencies: ${[...unbundled].join(', ')}`)
  }
  for (const [what, dir] of STD) {
    for (const [name, text] of suppliedFiles(dir)) {
      add(text, `${what} (linked as part of the Rust standard library): ${name}`)
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
    'The Rust standard library (https://github.com/rust-lang/rust, MIT OR Apache-2.0) is',
    'also linked, together with the crates it is built from. Of those, addr2line,',
    'compiler_builtins, gimli, object and rustc-demangle are not in the list; their license',
    'files are included below, after those of the listed crates.',
    '',
    ...index,
    '',
    ...sections,
  ].join('\n')
}

// ---- npmのパッケージのライセンス ----

// 画面のバンドルに入ったnpmのパッケージの一覧の本文を作る。フロントエンドのビルド(Viteの
// `build.license`が出し、`vite.config.ts`が`dist`の外へ移したもの)の一覧を読み、許容していないライセンスのパッケージか、ライセンスファイルを
// 持たない(著作権者の名前を載せられない)パッケージがあれば失敗する。
function frontendLicenses() {
  const file = join(root, 'frontend', 'dist-meta', 'license.json')
  if (!existsSync(file)) fail(`missing: ${file} (build the frontend first)`)
  const accepted = acceptedLicenses()
  const packages = JSON.parse(readFileSync(file, 'utf8'))
  const rejected = packages.filter((p) => !p.identifier || !satisfies(p.identifier, accepted))
  if (rejected.length > 0) {
    fail(
      `npm packages with licenses not accepted in about.toml: ` +
        rejected.map((p) => `${p.name}@${p.version} (${p.identifier ?? 'no license'})`).join(', '),
    )
  }
  const unsupplied = packages.filter((p) => !p.text)
  if (unsupplied.length > 0) {
    fail(`npm packages without license files: ${unsupplied.map((p) => `${p.name}@${p.version}`).join(', ')}`)
  }
  const rule = '='.repeat(78)
  return [
    'Third-party licenses (npm packages)',
    '',
    'The user interface of SCITL Task Companion bundles the npm packages listed below.',
    'The license file shipped in each package follows the list.',
    '',
    ...packages.map((p) => `${p.name} ${p.version}  (${p.identifier})  https://www.npmjs.com/package/${p.name}`),
    '',
    ...packages.flatMap((p) => [
      rule,
      `${p.name} ${p.version}`,
      rule,
      '',
      p.text,
      '',
    ]),
  ].join('\n')
}

if (check === '--check-licenses') {
  rustLicenses()
  process.exit(0)
}
if (check === '--check-frontend-licenses') {
  frontendLicenses()
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
  [join(root, 'README.md'), 'README.md'],
  [join(root, 'LICENSE'), 'LICENSE'],
  // 同梱フォント。
  [join(root, 'frontend', 'public', 'fonts', 'NotoJP-LICENSE.txt'), join('THIRD-PARTY-LICENSES', 'NotoJP-LICENSE.txt')],
  // 同梱アイコン(frontend/src/Icon.tsx)。
  [
    join(root, 'frontend', 'src', 'icons', 'MaterialSymbols-LICENSE.txt'),
    join('THIRD-PARTY-LICENSES', 'MaterialSymbols-LICENSE.txt'),
  ],
]
for (const [from] of copies) {
  if (!existsSync(from)) fail(`missing: ${from}`)
}
const rust = rustLicenses()
const frontend = frontendLicenses()

rmSync(dist, { recursive: true, force: true })
mkdirSync(join(dist, 'THIRD-PARTY-LICENSES'), { recursive: true })
for (const [from, to] of copies) {
  copyFileSync(from, join(dist, to))
}
writeFileSync(join(dist, 'THIRD-PARTY-LICENSES', 'rust.txt'), rust)
writeFileSync(join(dist, 'THIRD-PARTY-LICENSES', 'frontend.txt'), frontend)

console.log(name)
