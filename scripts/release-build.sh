#!/usr/bin/env bash
# 配布用のビルド(Linux・macOS)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
#
# ビルドした人の絶対パス(ホームディレクトリ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す(Issue #380)。`strip = true`では
# シンボルとデバッグ情報しか消えない。Cargoの`trim-paths`が安定版に入ったら、
# `[profile.release]`に置く形に替える。`.cargo/config.toml`の`rustflags`は環境変数を
# 展開できないので、ここで渡す。
#
# 引数はそのまま`tauri build`に渡す(例: `--bundles deb`、`--no-bundle`)。
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
home="$(cd "$HOME" && pwd -P)"
cargo_home="$(cd "${CARGO_HOME:-$HOME/.cargo}" && pwd -P)"

if [[ -n "${RUSTFLAGS:-}" || -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
  echo "RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(置き換えの指定を上書きしないため)" >&2
  exit 1
fi

# rustcは後に書いたものから当てはまるかを見るので、広いもの(ホーム)を先に置く。空白を含む
# パスでも割れないよう、区切りが0x1fのCARGO_ENCODED_RUSTFLAGSで渡す。
sep=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=${home}=~${sep}--remap-path-prefix=${cargo_home}=cargo-home${sep}--remap-path-prefix=${root}=."

cd "$root/crates/scitl-tauri"
npm ci
npx tauri build "$@"

# 配布するバイナリに、置き換えたはずのパスが残っていないかを確かめる。
binary="$root/target/release/scitl"
found=0
for path in "$home/" "$cargo_home/" "$root/"; do
  count="$(grep -c -a -F -- "$path" "$binary" || true)"
  if [[ "$count" != 0 ]]; then
    echo "バイナリに絶対パスが残っています: $path ($count 箇所)" >&2
    found=1
  fi
done
if [[ "$found" != 0 ]]; then
  exit 1
fi
echo "絶対パスは残っていません: $binary"
