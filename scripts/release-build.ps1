if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'Run this script with PowerShell 7 or later (pwsh).' }
# 配布用のビルド(Windows)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
# 1行目は、Windows PowerShell 5.1(BOMの無いUTF-8を読めない)で開いても読めるようASCIIで書く。
#
# ビルドした人の絶対パス(ユーザーフォルダ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す(Issue #380)。理由と、Cargoの
# `trim-paths`が安定版に入ったときの扱いは release-build.sh と同じ。
#
# 引数はそのまま`tauri build`に渡す(例: `--bundles msi`、`--no-bundle`)。`--target`と
# `CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わるため)。
$ErrorActionPreference = 'Stop'

if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) {
    throw 'RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(置き換えの指定を上書きしないため)'
}

# 置き換える場所は、書かれたままのパスと、解決したパスの両方を持つ(同じなら1つ)。
function Get-Forms([string] $path) {
    @($path, (Resolve-Path $path).Path) | ForEach-Object { $_.TrimEnd('\', '/') } | Select-Object -Unique
}
# 1つだけでも配列として持つ(PowerShellは要素が1つの出力を配列にしない)。
$root = @(Get-Forms (Split-Path $PSScriptRoot -Parent))
$userHome = @(Get-Forms $env:USERPROFILE)
$cargoHome = @(Get-Forms $(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }))

foreach ($path in $userHome) {
    if ($path.Length -le 3) { throw "ユーザーフォルダがドライブの直下です。置き換えがすべてのパスに当たるので止めます: $path" }
}

# rustcは後に書いたものから当てはまるかを見るので、広いもの(ユーザーフォルダ)を先に置く。
# 空白を含むパスでも割れないよう、区切りが0x1fのCARGO_ENCODED_RUSTFLAGSで渡す。
$prefixes = @()
$prefixes += $userHome | ForEach-Object { "--remap-path-prefix=$_=~" }
$prefixes += $cargoHome | ForEach-Object { "--remap-path-prefix=$_=cargo-home" }
$prefixes += $root | ForEach-Object { "--remap-path-prefix=$_=." }
$env:CARGO_ENCODED_RUSTFLAGS = ($prefixes | Select-Object -Unique) -join [char]0x1f

$rootPath = $root[-1]
try {
    npm --prefix (Join-Path $rootPath 'frontend') ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Push-Location (Join-Path $rootPath 'crates\scitl-tauri')
    try {
        npm ci
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        npx tauri build @args
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS
}

# 配布するバイナリに、置き換えたはずのパスが残っていないかを確かめる。パスはバイト列として
# 埋め込まれるので、UTF-8として読んで探す。区切りは`\`と`/`の両方を見て、Windowsのパスは
# 大文字・小文字を区別しないので区別せずに探す。
$binary = Join-Path $rootPath 'target\release\scitl.exe'
if (-not (Test-Path $binary)) {
    throw "検査するバイナリがありません: $binary(--target・CARGO_TARGET_DIRには対応していません)"
}
$text = [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($binary))
$found = $false
foreach ($path in @($userHome + $cargoHome + $root | Select-Object -Unique)) {
    foreach ($form in @("$path\", ($path.Replace('\', '/') + '/'))) {
        if ($text.IndexOf($form, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
            Write-Host "バイナリに絶対パスが残っています: $form"
            $found = $true
        }
    }
}
if ($found) { exit 1 }
Write-Host "絶対パスは残っていません: $binary"
