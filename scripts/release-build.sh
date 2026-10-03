#!/usr/bin/env bash
# 配布用のビルド(Linux)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
#
# ビルドした人の絶対パス(ホームディレクトリ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す(Issue #380)。`strip = true`では
# シンボルとデバッグ情報しか消えない。Cargoの`trim-paths`が安定版に入ったら、
# `[profile.release]`に置く形に替える。`.cargo/config.toml`の`rustflags`は環境変数を
# 展開できないので、ここで渡す。
#
# 引数はそのまま`tauri build`に渡す(例: `--bundles deb`、`--no-bundle`)。`--target`と
# `CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わるため)。
set -euo pipefail

if [[ -n "${RUSTFLAGS:-}" || -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
  echo "RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(置き換えの指定を上書きしないため)" >&2
  exit 1
fi

# 置き換える場所は、書かれたままのパス(cargoはCARGO_HOMEのリンクを解決せずに使う)と、
# リンクを解決したパスの両方を持つ。
root="$(cd "$(dirname "$0")/.." && pwd)"
root_physical="$(cd "$root" && pwd -P)"
home="$HOME"
home_physical="$(cd "$home" && pwd -P)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
cargo_home_physical="$(cd "$cargo_home" && pwd -P)"

if [[ "$home" != /?* || "$home_physical" == / ]]; then
  echo "HOME がルートか相対パスです。置き換えがすべてのパスに当たるので止めます: $home" >&2
  exit 1
fi

# rustcは後に書いたものから当てはまるかを見るので、広いもの(ホーム)を先に置く。
prefixes=()
for pair in \
  "$home=~" "$home_physical=~" \
  "$cargo_home=cargo-home" "$cargo_home_physical=cargo-home" \
  "$root=." "$root_physical=."; do
  [[ " ${prefixes[*]-} " == *" --remap-path-prefix=$pair "* ]] || prefixes+=("--remap-path-prefix=$pair")
done
# 空白を含むパスでも割れないよう、区切りが0x1fのCARGO_ENCODED_RUSTFLAGSで渡す。
CARGO_ENCODED_RUSTFLAGS="$(IFS=$'\x1f'; echo "${prefixes[*]}")"
export CARGO_ENCODED_RUSTFLAGS

npm --prefix "$root/frontend" ci
cd "$root/crates/scitl-tauri"
npm ci
npx tauri build "$@"

# 配布するバイナリに、置き換えたはずのパスが残っていないかを確かめる。
binary="$root/target/release/scitl"
if [[ ! -f "$binary" ]]; then
  echo "検査するバイナリがありません: $binary(--target・CARGO_TARGET_DIRには対応していません)" >&2
  exit 1
fi
found=0
for path in "$home" "$home_physical" "$cargo_home" "$cargo_home_physical" "$root" "$root_physical"; do
  if LC_ALL=C grep -q -a -F -- "$path/" "$binary"; then
    echo "バイナリに絶対パスが残っています: $path/" >&2
    found=1
  fi
done
if [[ "$found" != 0 ]]; then
  exit 1
fi
echo "絶対パスは残っていません: $binary"
