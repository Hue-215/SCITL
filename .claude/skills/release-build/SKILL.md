---
name: release-build
description: SCITLの配布用ビルドの手順(scripts/release-build.sh・release-build.ps1の使い方、ビルドした人の絶対パスをバイナリから外す仕組みと確かめ方、trim-pathsが安定版に入ったときの移し替え)。配布物を作るとき、配布用ビルドの設定(RUSTFLAGS・[profile.release])を変えるとき、バイナリに焼き込まれる情報を調べるときに開く。
---

# 配布用のビルド

配布物は`scripts/`のスクリプトで作る。`cargo build --release`や`npx tauri build`を直接叩くと、
ビルドした人の絶対パスがバイナリに残る(下の「なぜスクリプトを通すか」)。`release/*`への移行と
配布はメンテナが行う(CLAUDE.md「進行中の作業」)。

## 1. 実行する

| OS | コマンド |
|---|---|
| Linux・macOS | `scripts/release-build.sh` |
| Windows | `pwsh scripts/release-build.ps1`(PowerShell 7) |

- 引数はそのまま`tauri build`に渡る。束ね方を絞るなら`--bundles deb`・`--bundles msi`、バイナリ
  だけなら`--no-bundle`
- Tauriのビルドに要るシステムの依存は、CI(`.github/workflows/ci.yml`の「システム依存を入れる」)と同じ
- `RUSTFLAGS`・`CARGO_ENCODED_RUSTFLAGS`を設定したままだと止まる。外してから実行する
- フラグが開発時のビルドと違うので、`target/release`は全部作り直しになる

## 2. 何をしているか

`--remap-path-prefix`で、次の3つのパスをバイナリに焼き込まれる形から置き換える(Issue #380)。

| パス | 置き換え先 | 主に入るもの |
|---|---|---|
| ホームディレクトリ | `~` | 下の2つに当たらない分 |
| `CARGO_HOME`(既定は`~/.cargo`) | `cargo-home` | 依存クレートのパニックの位置(`registry/src/…`) |
| このリポジトリ | `.` | このリポジトリのクレートのパニックの位置 |

- `strip = true`(`[profile.release]`)が消すのはシンボルとデバッグ情報だけで、パニックの位置などの
  文字列は残る
- `.cargo/config.toml`の`rustflags`は環境変数を展開できないので、スクリプトで渡す
- パスに空白があっても割れないよう、区切りが空白でない`CARGO_ENCODED_RUSTFLAGS`で渡す
- rustcは後に書いた置き換えから当てはまるかを見るので、広いもの(ホーム)を先に書く

## 3. 確かめる

スクリプトはビルドの後、`target/release/scitl`(Windowsは`scitl.exe`)に3つのパスが残っていないかを
調べ、残っていれば失敗で終わる。束ねた配布物(deb・msi等)は同じバイナリを入れるので、別には
調べない。手で確かめるなら次のとおり。

```sh
grep -c -a -F "$HOME/" target/release/scitl   # 0 なら残っていない
```

置き換えの対象を足したら(例えば別の場所に置いた依存)、スクリプトの検査にも足す。

## 4. trim-pathsが安定版に入ったら

Cargoの`trim-paths`が安定版に入ったら、`[profile.release]`に`trim-paths = true`を置き、スクリプトの
`--remap-path-prefix`を外す(Rust 1.97では、まだ`-Z`の不安定な機能)。検査はそのまま残す。
