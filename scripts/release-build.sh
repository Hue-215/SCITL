#!/usr/bin/env bash
# 配布用のビルド(Linux)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
#
# ビルドした人の絶対パス(ホームディレクトリ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す。`strip = true`では
# シンボルとデバッグ情報しか消えない。Cargoの`trim-paths`が安定版に入ったら、
# `[profile.release]`に置く形に替える。`.cargo/config.toml`の`rustflags`は環境変数を
# 展開できないので、ここで渡す。
#
# 続けて、CLIも同じ置き換えを付けてビルドし、GUI・CLI・ライセンス類を`target/dist`の
# tar.gzにまとめる。配布物は実行ファイルだけで、WebKitGTK等は利用者の環境のものを使う。
#
# 引数はそのまま`tauri build`に渡す。束ねずに実行ファイルだけを作るので、`--bundles`・`--no-bundle`は
# 受け付けない。`--target`と`CARGO_TARGET_DIR`には対応しない(検査するバイナリの場所が変わるため)。
set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -b* | --bundles | --bundles=* | --no-bundle)
      echo "束ねずに実行ファイルだけを作ります。$arg を外してください" >&2
      exit 2
      ;;
  esac
done

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

# 第三者ライセンスの一覧を作れるか(道具の有無、許容していないライセンスの依存)を、時間のかかる
# ビルドの前に確かめる。前の配布物は、失敗したときに今回のものと取り違えないよう先に消す。
node "$root/scripts/assemble-dist.mjs" --check-licenses
rm -rf "$root/target/dist"

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

# 前のビルドの実行ファイルを、今回のものと取り違えないよう先に消す(`--target`等で出力先が
# 変わると、前のものが残ったまま検査を通る)。
binaries=("$root/target/release/scitl" "$root/target/release/scitl-cli")
rm -f "${binaries[@]}"

npm --prefix "$root/frontend" ci
cd "$root/crates/scitl-tauri"
npm ci
npx tauri build --no-bundle "$@"
# `tauri build`はGUIしか作らない。配布物に入れるCLIも、同じ置き換えを付けて作る。
cargo build --release --locked -p scitl-cli
unset CARGO_ENCODED_RUSTFLAGS

for binary in "${binaries[@]}"; do
  if [[ ! -f "$binary" ]]; then
    echo "検査するバイナリがありません: $binary(--target・CARGO_TARGET_DIRには対応していません)" >&2
    exit 1
  fi
done

# 配布物のフォルダを組み立てる。tar.gzまで作り終えずに抜けたら(検査に落ちた・中断した)、
# 検査していないものが残らないようフォルダを消す。
name="$(node "$root/scripts/assemble-dist.mjs" "${binaries[@]}")"
if [[ -z "$name" ]]; then
  echo "配布物のフォルダの名前を受け取れませんでした" >&2
  exit 1
fi
dist="$root/target/dist/$name"
found_list=""
completed=""
trap 'rm -f "$found_list"; [[ -n "$completed" ]] || rm -rf "$dist" "$dist.tar.gz"' EXIT
found_list="$(mktemp)"

# 組み立てた配布物に、置き換えたはずのパスが残っていないかを確かめる。
found=0
for path in "$home" "$home_physical" "$cargo_home" "$cargo_home_physical" "$root" "$root_physical"; do
  # grepは見つからないと1、読めないファイルがあると2で終わる。2は検査の漏れなので止める。
  status=0
  LC_ALL=C grep -r -l -Z -a -F -- "$path/" "$dist" >"$found_list" || status=$?
  if [[ "$status" -gt 1 ]]; then
    echo "検査できないファイルがあります(上のgrepのエラー)" >&2
    exit 1
  fi
  while IFS= read -r -d '' file; do
    echo "絶対パスが残っています: $path/($file)" >&2
    found=1
  done <"$found_list"
done
if [[ "$found" != 0 ]]; then
  exit 1
fi
echo "絶対パスは残っていません: $dist"

# tar.gzにまとめる(実行の許可を保つため、zipではなくtarにする)。tarはファイルの持ち主の名前と、
# ビルドした人のumaskで決まった権限を記録するので、持ち主はrootに、権限は所有者が読み書き、
# グループ・他人が読める形(実行の許可は、元から実行できるものとディレクトリにだけ付ける)に揃える。
tar -C "$root/target/dist" --owner=0 --group=0 --numeric-owner --mode='u+rwX,go+rX,go-w' \
  -czf "$dist.tar.gz" "$name"
completed=1
echo "配布物: $dist.tar.gz"
