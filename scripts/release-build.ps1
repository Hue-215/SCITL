# 配布用のビルド(Windows)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
#
# ビルドした人の絶対パス(ユーザーフォルダ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す(Issue #380)。理由と、Cargoの
# `trim-paths`が安定版に入ったときの扱いは release-build.sh と同じ。
#
# 引数はそのまま`tauri build`に渡す(例: `--bundles msi`、`--no-bundle`)。
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$userHome = (Resolve-Path $env:USERPROFILE).Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$cargoHome = (Resolve-Path $cargoHome).Path

if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) {
    Write-Error 'RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(置き換えの指定を上書きしないため)'
}

# rustcは後に書いたものから当てはまるかを見るので、広いもの(ユーザーフォルダ)を先に置く。
# 空白を含むパスでも割れないよう、区切りが0x1fのCARGO_ENCODED_RUSTFLAGSで渡す。
$sep = [char]0x1f
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$userHome=~",
    "--remap-path-prefix=$cargoHome=cargo-home",
    "--remap-path-prefix=$root=."
) -join $sep

Push-Location (Join-Path $root 'crates\scitl-tauri')
try {
    npm ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    npx tauri build @args
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS
}

# 配布するバイナリに、置き換えたはずのパスが残っていないかを確かめる。パスはバイト列として
# 埋め込まれるので、UTF-8として読んで探す(区切りは`\`と`/`の両方を見る)。
$binary = Join-Path $root 'target\release\scitl.exe'
$text = [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($binary))
$found = $false
foreach ($path in @($userHome, $cargoHome, $root)) {
    foreach ($form in @($path, $path.Replace('\', '/'))) {
        # Windowsのパスは大文字・小文字を区別しないので、区別せずに探す。
        if ($text.IndexOf($form, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
            Write-Host "バイナリに絶対パスが残っています: $form"
            $found = $true
        }
    }
}
if ($found) { exit 1 }
Write-Host "絶対パスは残っていません: $binary"
