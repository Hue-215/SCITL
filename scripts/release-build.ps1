if ($PSVersionTable.PSVersion -lt [version] '7.2') { throw 'Run this script with PowerShell 7.2 or later (pwsh).' }
# 配布用のビルド(Windows)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
# 1行目は、Windows PowerShell 5.1(BOMの無いUTF-8を読めない)で開いても読めるようASCIIで書く。
#
# ビルドした人の絶対パス(ユーザーフォルダ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置、Cのソースの場所等)から外す。
# 理由と、Cargoの`trim-paths`が安定版に入ったときの扱いは release-build.sh と同じ。
#
# 続けて、CLIも同じ置き換えを付けてビルドし、GUI・CLI・ライセンス類を`target\dist`のzipにまとめる。
#
# 引数はそのまま`tauri build`に渡す(例: `--bundles msi`、`--no-bundle`)。`--target`と
# `CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わるため)。
$ErrorActionPreference = 'Stop'

if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) {
    throw 'RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(置き換えの指定を上書きしないため)'
}

# リンク(ジャンクション・シンボリックリンク)を辿った先のパスを求める。
function Get-Physical([string] $path) {
    $physical = [System.IO.Path]::GetPathRoot($path)
    $names = $path.Substring($physical.Length).Split([char[]] '\/', [System.StringSplitOptions]::RemoveEmptyEntries)
    foreach ($name in $names) {
        $physical = [System.IO.Path]::Combine($physical, $name)
        $target = [System.IO.Directory]::ResolveLinkTarget($physical, $true)
        if ($target) { $physical = $target.FullName }
    }
    $physical
}

# 置き換える場所は、書かれたままのパス、リンクを辿ったパス、それぞれの8.3形式を持つ(同じなら1つ)。
# cargoは書かれたままの形を使う。依存クレートは、リンクを辿り、パスの長さの上限を避けるために
# 8.3形式にしてから、Cのコンパイラへ渡すことがある。
$fileSystem = New-Object -ComObject Scripting.FileSystemObject
function Get-Forms([string] $path) {
    @($path, (Get-Physical $path)) | ForEach-Object { $_; $fileSystem.GetFolder($_).ShortPath } |
        Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\', '/') } | Select-Object -Unique
}
$rootPath = Split-Path $PSScriptRoot -Parent

# 第三者ライセンスの一覧を作れるか(道具の有無、許容していないライセンスの依存)を、時間のかかる
# ビルドの前に確かめる。前の配布物は、失敗したときに今回のものと取り違えないよう先に消す。
node (Join-Path $rootPath 'scripts\assemble-dist.mjs') --check-licenses
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Remove-Item -LiteralPath (Join-Path $rootPath 'target\dist') -Recurse -Force -ErrorAction SilentlyContinue

# 1つだけでも配列として持つ(PowerShellは要素が1つの出力を配列にしない)。
$root = @(Get-Forms $rootPath)
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

# rustcの置き換えは、依存クレートがCのコンパイラに作らせる部分には届かない。MSVCのコンパイラは
# 環境変数CLの中身を引数の前に足して読むので、そこで同じ場所を`__FILE__`から外させる。先に
# 書いたものが当たるので、狭いものを先に置く。末尾の`\`を重ねるのは、閉じる引用符を打ち消さないため。
$trims = $root + $cargoHome + $userHome | Select-Object -Unique | ForEach-Object { "`"/d1trimfile:$_\\`"" }
$previousCl = $env:CL
$env:CL = (@($previousCl) + $trims | Where-Object { $_ }) -join ' '

# npmは`.cmd`を名指しし、tauriのCLIはnpxを通さずにnodeで動かす。拡張子を省くとPowerShellは
# `.ps1`の版を選び、その版は呼び出しの行を文字列として読み直して実行するので、`@args`が空になる。
# `npx.cmd`は、cmd.exeが引用符や`&`を解釈して引数を壊す。
try {
    npm.cmd --prefix (Join-Path $rootPath 'frontend') ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Push-Location (Join-Path $rootPath 'crates\scitl-tauri')
    try {
        npm.cmd ci
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        node node_modules\@tauri-apps\cli\tauri.js build @args
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        # `tauri build`はGUIしか作らない。配布物に入れるCLIも、同じ置き換えを付けて作る。
        cargo build --release --locked -p scitl-cli
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Pop-Location
    }
} finally {
    $env:CL = $previousCl
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS
}

# 配布するバイナリに、置き換えたはずのパスが残っていないかを確かめる。パスはバイト列として
# 埋め込まれるので、UTF-8として読んで探す。区切りは`\`と`/`の両方を見て、Windowsのパスは
# 大文字・小文字を区別しないので区別せずに探す。
$binaries = 'scitl.exe', 'scitl-cli.exe' | ForEach-Object { Join-Path $rootPath "target\release\$_" }
$found = $false
foreach ($binary in $binaries) {
    if (-not (Test-Path $binary)) {
        throw "検査するバイナリがありません: $binary(--target・CARGO_TARGET_DIRには対応していません)"
    }
    $text = [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($binary))
    foreach ($path in @($userHome + $cargoHome + $root | Select-Object -Unique)) {
        foreach ($form in @("$path\", ($path.Replace('\', '/') + '/'))) {
            if ($text.IndexOf($form, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
                Write-Host "バイナリに絶対パスが残っています: $form($binary)"
                $found = $true
            }
        }
    }
}
if ($found) { exit 1 }
Write-Host "絶対パスは残っていません: $($binaries -join ', ')"

# 配布物のフォルダを組み立てて、zipにまとめる。
$name = node (Join-Path $rootPath 'scripts\assemble-dist.mjs') @binaries
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$dist = Join-Path $rootPath "target\dist\$name"
Compress-Archive -LiteralPath $dist -DestinationPath "$dist.zip" -Force
Write-Host "配布物: $dist.zip"
