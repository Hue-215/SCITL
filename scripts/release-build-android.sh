#!/usr/bin/env bash
# Androidの配布用のビルド(署名したAPK)。手順と確かめ方は.claude/skills/release-build/SKILL.md 7節。
#
# デスクトップ向け(release-build.sh)と同じく、ビルドした人の絶対パスをネイティブのライブラリ(.so)から
# 外してビルドし、出来たAPKの中身(入っているABI・第三者ライセンスの一覧・残ったパス)を確かめてから、
# 署名して`target/dist`に置く。APKはビルドする側のOSによらないので、Linuxでだけ作る。
#
#   scripts/release-build-android.sh [--target aarch64|x86_64]
#
# 配布するのは実機向けの`aarch64`(既定)だけ。`x86_64`は、エミュレーターでリリースのAPKを確かめるためのもの。
# 署名の鍵は環境変数で渡す。パスワードは、署名する`apksigner`にだけ渡す(npm・cargo・Gradleと、
# ビルドの後も残るGradleのデーモンには渡さない)。
#
#   SCITL_ANDROID_KEYSTORE           キーストア(PKCS12)のファイルの絶対パス
#   SCITL_ANDROID_KEYSTORE_PASSWORD  キーストアのパスワード
#   SCITL_ANDROID_KEY_ALIAS          鍵の別名
set -euo pipefail

# 本番の署名の鍵の証明書の指紋(SHA-256)。Androidは、同じ鍵で署名したAPKだけを上書きとして
# 受け付けるので、配布するAPKがこの鍵で署名されていなければ失敗にする。指紋は公開してよい情報
# (配ったAPKから誰でも読める)。鍵そのものはリポジトリに置かない。
release_certificate=88f470c4a4f70d038c0112677a68d0ffc41f64be6ff5b1919b12e3192989b130

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
# 途中で作業ディレクトリを移るので、取り違えないよう絶対パスだけを受け付ける。
if [[ "$SCITL_ANDROID_KEYSTORE" != /* || ! -f "$SCITL_ANDROID_KEYSTORE" ]]; then
  echo "SCITL_ANDROID_KEYSTORE は、キーストアのファイルの絶対パスにしてください: $SCITL_ANDROID_KEYSTORE" >&2
  exit 1
fi
# パスワードは、ここから先で起動するプロセスの環境変数に載せない。
keystore_password="$SCITL_ANDROID_KEYSTORE_PASSWORD"
unset SCITL_ANDROID_KEYSTORE_PASSWORD
build_tools="$(find "$ANDROID_HOME/build-tools" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort -V | tail -1 || true)"
apksigner="$build_tools/apksigner"
zipalign="$build_tools/zipalign"
if [[ -z "$build_tools" || ! -x "$apksigner" || ! -x "$zipalign" ]]; then
  echo "apksigner・zipalign がありません(SDK Managerで「Android SDK Build-Tools」を入れてください)" >&2
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
built="$root/crates/scitl-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk"
rm -f "$dist" "$built"

export_remap_flags
npm --prefix "$root/frontend" ci
cd "$root/crates/scitl-tauri"
npm ci
# 置き換えの指定は、tauri-cli → Gradle → tauri-cli → cargoと、環境変数のまま届く。
npx tauri android build --apk --target "$target"
unset CARGO_ENCODED_RUSTFLAGS

if [[ ! -f "$built" ]]; then
  echo "ビルドしたAPKがありません: $built" >&2
  exit 1
fi

# APKはzipで、中のファイルは圧縮されているので、展開してから中身を確かめる。
# 署名まで終えずに抜けたら、検査していない・署名の無いものが配布物の置き場所に残らないようにする。
unpacked="$(mktemp -d)"
completed=""
trap 'rm -rf "$unpacked"; [[ -n "$completed" ]] || rm -f "$dist"' EXIT
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

# 署名する。署名はAPKの中のファイルを変えないので、上の検査は署名したものにも当てはまる。
# 署名の前に、中のファイルの位置が揃っていること(署名の後には直せない)を確かめる。
"$zipalign" -c 4 "$built"
mkdir -p "$root/target/dist"
SCITL_ANDROID_KEYSTORE_PASSWORD="$keystore_password" "$apksigner" sign \
  --ks "$SCITL_ANDROID_KEYSTORE" --ks-type PKCS12 --ks-key-alias "$SCITL_ANDROID_KEY_ALIAS" \
  --ks-pass env:SCITL_ANDROID_KEYSTORE_PASSWORD --key-pass env:SCITL_ANDROID_KEYSTORE_PASSWORD \
  --out "$dist" "$built"
rm -f "$dist.idsig"
# 署名を確かめ、署名した鍵が本番のものかを見る。エミュレーターで確かめるためのAPK(x86_64)は
# 配布しないので、別の鍵でも通す。
certificates="$("$apksigner" verify --print-certs "$dist" | sed -n 's/^Signer #[0-9]* certificate SHA-256 digest: //p')"
if [[ "$certificates" == "$release_certificate" ]]; then
  echo "本番の鍵で署名されています"
elif [[ "$target" == aarch64 ]]; then
  echo "本番の鍵で署名されていません(署名した鍵の証明書のSHA-256: ${certificates:-読み取れない})" >&2
  exit 1
else
  echo "本番の鍵ではない鍵で署名されています。このAPKは配布しないでください: $certificates"
fi
completed=1
echo "配布物: $dist"
