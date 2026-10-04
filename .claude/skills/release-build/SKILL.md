---
name: release-build
description: SCITLの配布用ビルドの手順(scripts/release-build.sh・release-build.ps1の使い方、ビルドした人の絶対パスをバイナリから外す仕組みと確かめ方、trim-pathsが安定版に入ったときの移し替え)。配布物を作るとき、配布用ビルドの設定(RUSTFLAGS・[profile.release])を変えるとき、バイナリに焼き込まれる情報を調べるときに開く。
---

# 配布用のビルド

配布物は`scripts/`のスクリプトで作る。`cargo build --release`や`npx tauri build`を直接叩くと、
ビルドした人の絶対パスがバイナリに残る(下の「何をしているか」)。`release/*`への移行と
配布はメンテナが行う(CLAUDE.md「進行中の作業」)。

## 1. 実行する

| OS | コマンド |
|---|---|
| Linux | `scripts/release-build.sh` |
| Windows | `pwsh scripts/release-build.ps1`(PowerShell 7.2以降。それより古いと最初の行で止まる) |

macOSは今は対象にしていない(資格情報の保存先が無い。`docs/spec/architecture/network-secrets.md`)。
shはmacOSでも動く書き方にしてあるが、確かめていない。

- 前提: Rust・Node.js(npm)と、Tauriのビルドに要るシステムの依存(CI`.github/workflows/ci.yml`の
  「システム依存を入れる」と同じ)。npmの依存(`frontend/`と`crates/scitl-tauri/`)はスクリプトが入れる。
  WindowsはMSVCのツールチェーン(Rustの既定)を前提にする
- 引数はそのまま`tauri build`に渡る。束ね方を絞るなら`--bundles deb`・`--bundles msi`、バイナリ
  だけなら`--no-bundle`。`--target`と`CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が
  変わる。検査の前に止まる)
- `RUSTFLAGS`・`CARGO_ENCODED_RUSTFLAGS`を設定したままだと止まる。外してから実行する。スクリプトが渡す
  `CARGO_ENCODED_RUSTFLAGS`は、ほかの場所のrustflags(`~/.cargo/config.toml`の`build.rustflags`・
  `[target.*].rustflags`、`CARGO_BUILD_RUSTFLAGS`等)より優先され、それらは**黙って効かなくなる**
  (リンカーの指定や`target-cpu`等)。リポジトリの`.cargo/config.toml`にrustflagsは無い
- フラグが開発時のビルドと違うので、`target/release`は全部作り直しになる
- `tauri build`は`crates/scitl-tauri/Cargo.toml`の依存の書き方を自分の形(`{ version = "2",
  features = [] }`)に揃え直すので、リポジトリにはその形で置いてある。整理のつもりで短い形に戻すと、
  ビルドのたびに作業ツリーが汚れる。書き直しはLFなので、`.gitattributes`でこのファイルをLFに
  固定してある(CRLFでチェックアウトされると、内容が同じでも変更ありと出る)

## 2. 何をしているか

`--remap-path-prefix`で、次の3つのパスをバイナリに焼き込まれる形から置き換える(Issue #380)。

| パス | 置き換え先 | 主に入るもの |
|---|---|---|
| ホームディレクトリ | `~` | 下の2つに当たらない分 |
| `CARGO_HOME`(既定は`~/.cargo`) | `cargo-home` | 依存クレートのパニックの位置(`registry/src/…`)。Linuxで約580箇所あった |
| このリポジトリ | `.` | 今は0箇所(ワークスペースのクレートは相対パスで渡る)。`env!("CARGO_MANIFEST_DIR")`等が焼き込まれたときの保険 |

- `strip = true`(`[profile.release]`)が消すのはシンボルとデバッグ情報だけで、パニックの位置などの
  文字列は残る
- `.cargo/config.toml`の`rustflags`は環境変数を展開できないので、スクリプトで渡す
- パスは、書かれたままの形(cargoは`CARGO_HOME`のシンボリックリンクを解決せずに使う)と、リンクを
  解決した形の両方を置き換え、両方を検査する
- パスに空白があっても割れないよう、区切りが空白でない`CARGO_ENCODED_RUSTFLAGS`で渡す
- rustcは後に書いた置き換えから当てはまるかを見るので、広いもの(ホーム)を先に書く
- ホームがルート(`HOME=/`)・ドライブの直下なら止める(置き換えがすべてのパスに当たるため)

### Cのソースの場所

rustcの置き換えは、依存クレートがCのコンパイラに作らせる部分には届かない。今は`aws-lc-sys`(TLSの
暗号ライブラリ)のCのソースが、自分の場所を`__FILE__`として焼き込む。

- Linuxでは`aws-lc-sys`が自分で`-ffile-prefix-map`を付けるので、スクリプトは何もしない
- Windows(MSVC)では付けないので、ps1が環境変数`CL`(コンパイラが引数の前に足して読む)で
  `/d1trimfile:`を渡し、同じ3つの場所を取り除く。置き換えではないので、`registry\src\…`から
  始まる形で残る
- `aws-lc-sys`は、自分のソースの場所を、リンクを辿り、パスの長さの上限を避けるために8.3形式
  (`…\CARGO~1\registry\…`)にしてからコンパイラへ渡す。MSVCは`#include`されたファイルだけを長い形に
  直すので、両方の形が焼き込まれる。ps1は3つの場所それぞれの8.3形式も、置き換え・取り除き・検査の
  対象にする
- `/d1trimfile:`は文書に載っていないフラグ(MSVC 14.51で確認、2026-10)。効かなくなれば検査が失敗する
- cargoは`CL`の変化を見ない。`CL`に渡すフラグを変えたら、`cargo clean --release -p aws-lc-sys`で
  Cのソースを作り直させる

## 3. 確かめる

スクリプトはビルドの後、`target/release/scitl`(Windowsは`scitl.exe`)の中身に3つのパスが残って
いないかを調べ、残っていれば失敗で終わる。束ねた配布物(deb・msi等)は同じバイナリを入れるので、
別には調べない。調べるのは実行ファイルのバイト列だけで、Tauriが圧縮して埋め込む画面の資産の中は
見ない(今はVite側でsourcemapを出していないので、パスは入らない)。手で確かめるなら次のとおり。

```sh
LC_ALL=C grep -c -a -F "$HOME/" target/release/scitl   # 0 なら残っていない
```

置き換えの対象を足したら(例えば別の場所に置いた依存)、スクリプトの検査にも足す。

## 4. trim-pathsが安定版に入ったら

Cargoの`trim-paths`が安定版に入ったら、`[profile.release]`に`trim-paths = true`を置き、スクリプトの
`--remap-path-prefix`を外す(Rust 1.97では、まだ`-Z`の不安定な機能)。検査はそのまま残す。
`trim-paths`が変えるのはrustcの出力なので、ps1の`CL`(Cのソースの場所)は、外しても検査が通ると
確かめてから外す。
