#!/usr/bin/env bash
# 配布用のビルド(Linux)。手順と確かめ方は.claude/skills/release-build/SKILL.md。
#
# ビルドした人の絶対パス(ホームディレクトリ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す(release-common.sh)。`strip = true`では
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

# 置き換えるパス(`root`・`home`・`cargo_home`)と、置き換えの指定・検査を読み込む。
# shellcheck source=scripts/release-common.sh
source "$(dirname "$0")/release-common.sh"

# 第三者ライセンスの一覧を作れるか(道具の有無、許容していないライセンスの依存)を、時間のかかる
# ビルドの前に確かめる。前の配布物は、失敗したときに今回のものと取り違えないよう先に消す。
node "$root/scripts/assemble-dist.mjs" --check-licenses
rm -rf "$root/target/dist"

export_remap_flags

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

# 配布物のフォルダを組み立てる。tar.gzまで作り終えずに抜けたら(組み立て・検査に落ちた・中断した)、
# 検査していないものが残らないよう、配布物の置き場所ごと消す(前の配布物はビルドの前に消してある)。
completed=""
trap '[[ -n "$completed" ]] || rm -rf "$root/target/dist"' EXIT
name="$(node "$root/scripts/assemble-dist.mjs" "${binaries[@]}")"
if [[ -z "$name" ]]; then
  echo "配布物のフォルダの名前を受け取れませんでした" >&2
  exit 1
fi
dist="$root/target/dist/$name"

# 組み立てた配布物に、置き換えたはずのパスが残っていないかを確かめる。
check_no_absolute_paths "$dist"
echo "絶対パスは残っていません: $dist"

# tar.gzにまとめる(実行の許可を保つため、zipではなくtarにする)。tarはファイルの持ち主の名前と、
# ビルドした人のumaskで決まった権限を記録するので、持ち主はrootに、権限は所有者が読み書き、
# グループ・他人が読める形(実行の許可は、元から実行できるものとディレクトリにだけ付ける)に揃える。
tar -C "$root/target/dist" --owner=0 --group=0 --numeric-owner --mode='u+rwX,go+rX,go-w' \
  -czf "$dist.tar.gz" "$name"
completed=1
echo "配布物: $dist.tar.gz"
