#!/usr/bin/env bash
# Androidの配布用のビルド(署名したAPK)。手順と確かめ方は.claude/skills/release-build/SKILL.md 7節。
#
# デスクトップ向け(release-build.sh)と同じく、ビルドした人の絶対パスをネイティブのライブラリ(.so)から
# 外してビルドし、出来たAPKの中身(署名・入っているABI・第三者ライセンスの一覧・残ったパス)を
# 確かめて、`target/dist`に置く。APKはビルドする側のOSによらないので、Linuxでだけ作る。
#
#   scripts/release-build-android.sh [--target aarch64|x86_64]
#
# 配布するのは実機向けの`aarch64`(既定)だけ。`x86_64`は、エミュレーターでリリースのAPKを確かめるためのもの。
# 署名の鍵は環境変数で渡す(`gen/android/app/build.gradle.kts`が読む)。
#
#   SCITL_ANDROID_KEYSTORE           キーストア(PKCS12)のファイルの絶対パス
#   SCITL_ANDROID_KEYSTORE_PASSWORD  キーストアのパスワード
#   SCITL_ANDROID_KEY_ALIAS          鍵の別名
set -euo pipefail

target=aarch64
case "$#:${1:-}:${2:-}" in
  0::) ;;
  2:--target:aarch64 | 2:--target:x86_64) target="$2" ;;
  *)
    echo "usage: scripts/release-build-android.sh [--target aarch64|x86_64]" >&2
    exit 2
    ;;
esac
# 配布物の名前に付けるCPUと、APKの中のライブラリのフォルダ。
case "$target" in
  aarch64) cpu=arm64 abi=arm64-v8a ;;
  x86_64) cpu=x86_64 abi=x86_64 ;;
esac

for name in SCITL_ANDROID_KEYSTORE SCITL_ANDROID_KEYSTORE_PASSWORD SCITL_ANDROID_KEY_ALIAS ANDROID_HOME NDK_HOME; do
  if [[ -z "${!name:-}" ]]; then
    echo "環境変数 $name を設定してください" >&2
    exit 1
  fi
done
# Gradleは相対パスを`gen/android/app`から辿るので、取り違えないよう絶対パスだけを受け付ける。
if [[ "$SCITL_ANDROID_KEYSTORE" != /* || ! -f "$SCITL_ANDROID_KEYSTORE" ]]; then
  echo "SCITL_ANDROID_KEYSTORE は、キーストアのファイルの絶対パスにしてください: $SCITL_ANDROID_KEYSTORE" >&2
  exit 1
fi
apksigner="$(find "$ANDROID_HOME/build-tools" -mindepth 2 -maxdepth 2 -name apksigner | sort -V | tail -1)"
if [[ -z "$apksigner" ]]; then
  echo "apksigner がありません(SDK Managerで「Android SDK Build-Tools」を入れてください)" >&2
  exit 1
fi

# 置き換えるパス(`root`・`home`・`cargo_home`)と、置き換えの指定・検査を読み込む。
# shellcheck source=scripts/release-common.sh
source "$(dirname "$0")/release-common.sh"

# 第三者ライセンスの一覧を作れるか(道具の有無、許容していないライセンスの依存)を、時間のかかる
# ビルドの前に確かめる。
node "$root/scripts/assemble-dist.mjs" --check-licenses
name="$(node "$root/scripts/assemble-dist.mjs" --android-name "$cpu")"
if [[ -z "$name" ]]; then
  echo "配布物の名前を受け取れませんでした" >&2
  exit 1
fi

# 前の配布物と前のビルドのAPKを、今回のものと取り違えないよう先に消す。`target/dist`のほかの
# 配布物(デスクトップ向け)には触らない。
dist="$root/target/dist/$name.apk"
built="$root/crates/scitl-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk"
rm -f "$dist" "$built"

export_remap_flags
npm --prefix "$root/frontend" ci
cd "$root/crates/scitl-tauri"
npm ci
# 置き換えの指定は、tauri-cli → Gradle → tauri-cli → cargoと、環境変数のまま届く。
npx tauri android build --apk --target "$target"
unset CARGO_ENCODED_RUSTFLAGS

if [[ ! -f "$built" ]]; then
  echo "署名したAPKがありません: $built" >&2
  exit 1
fi

# 署名を確かめ、署名した鍵の証明書を表示する(控えてある指紋と見比べる)。
"$apksigner" verify --print-certs "$built"

# APKはzipで、中のファイルは圧縮されているので、展開してから中身を確かめる。
unpacked="$(mktemp -d)"
trap 'rm -rf "$unpacked"' EXIT
unzip -q "$built" -d "$unpacked"

# 入っているネイティブのライブラリが、頼んだABIのものだけか。
abis="$(find "$unpacked/lib" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ')"
if [[ "$abis" != "$abi " ]]; then
  echo "APKに入っているABIが想定と違います: $abis(想定: $abi)" >&2
  exit 1
fi
for file in LICENSE THIRD-PARTY-LICENSES/rust.txt THIRD-PARTY-LICENSES/frontend.txt THIRD-PARTY-LICENSES/android.txt; do
  if [[ ! -s "$unpacked/assets/licenses/$file" ]]; then
    echo "APKにライセンスの一覧が入っていません: assets/licenses/$file" >&2
    exit 1
  fi
done
check_no_absolute_paths "$unpacked"
echo "絶対パスは残っていません"

mkdir -p "$root/target/dist"
cp "$built" "$dist"
echo "配布物: $dist"
