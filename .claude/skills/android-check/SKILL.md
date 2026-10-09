---
name: android-check
description: SCITLをAndroidのエミュレーター(と実機)で動かして見た目と振る舞いを確かめる手順。AVDの用意(幅360dpと411dp)、エミュレーターの起動(ウィンドウ無しを含む)、APKを作って入れる・`tauri android dev`で開発サーバーの画面を読む、`adb`でのスクリーンショット・タップ・文字入力・「戻る」・スワイプ、WebViewのDevTools(CDP)で実寸を測る`cdp-eval.mjs`、実機での確認の手順。画面を触ってAndroidでの見た目を確かめるとき(ui.md 7節)、指で操作する端末の決まり(ui.md 5節)を変えたとき、Androidでの振る舞いを確かめるとき、`chrome://inspect`・`adb`・エミュレーターを使うときに開く。
---

# Androidのエミュレーターで確かめる

見た目の確認の位置付け(デスクトップの確認・ハーネスとの使い分け)は`docs/spec/ui.md` 7節。
ビルドに要る道具と環境変数は`docs/spec/architecture/tech-stack.md`「Androidのビルド」。
ここでは、Claudeがユーザーに頼まずに、エミュレーターを動かして撮り・触り・測る手順を書く。

コマンドは`$ANDROID_HOME/emulator`・`$ANDROID_HOME/platform-tools`・
`$ANDROID_HOME/cmdline-tools/latest/bin`にPATHが通っている前提で書く。エミュレーターが2台以上
動いているときは、`adb`に必ず`-s <シリアル>`(`adb devices`の左の列。`emulator-5554`等)を付ける。

## 1. AVDを用意する

幅の違う2つを使う。CSSの幅(dp)は「画面の横の画素数 ÷ 密度(dpi) × 160」。

| AVD名 | 端末定義 | 画面 | CSSの幅×高さ | devicePixelRatio |
|---|---|---|---|---|
| `Small_Phone_API_37` | `small_phone` | 720×1280、320dpi | 360×640 | 2 |
| `Medium_Phone_API_37` | `medium_phone` | 1080×2400、420dpi | 411×914 | 2.625 |

```sh
sdkmanager "system-images;android-37.0;google_apis_playstore;x86_64"   # 未導入なら
echo no | avdmanager create avd -n Small_Phone_API_37 \
  -k "system-images;android-37.0;google_apis_playstore;x86_64" -d small_phone
echo no | avdmanager create avd -n Medium_Phone_API_37 \
  -k "system-images;android-37.0;google_apis_playstore;x86_64" -d medium_phone
emulator -list-avds
```

Android Studioの Device Manager で作ったものでもよい(Medium PhoneはAndroid Studioの既定で、AVD名は
`Medium_Phone_API_37.0`になる。以下のAVD名は`emulator -list-avds`の表示に読み替える)。
Google Playの入ったイメージを使うのは、AndroidのWebViewをPlayストアから更新できるため
(WebViewの版は`adb shell dumpsys package com.google.android.webview | grep versionName`)。

## 2. エミュレーターを起動する

```sh
emulator -avd Medium_Phone_API_37 &                                   # ウィンドウ有り(emulator-5554)
emulator -avd Small_Phone_API_37 -no-window -no-audio -no-boot-anim &   # ウィンドウ無し(emulator-5556)
# 起動し終わるまで待つ(1になれば済み)
until [ "$(adb -s emulator-5556 shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 3; done
```

- 2台を同時に立てられる。シリアルは立てた順に`emulator-5554`・`emulator-5556`…になる。以下の例は
  Small Phone(`emulator-5556`)で書く。
  どれがどのAVDかは`adb -s <シリアル> emu avd name`
- ウィンドウ無し(`-no-window`)でも、撮る・触る・測るは下の手順のとおりにできる
- x86_64のイメージはハードウェアの仮想化(Linuxでは`/dev/kvm`)が要る。クラウドのセッションで
  動くかは確かめていない(`ls -l /dev/kvm`が無ければ動かない見込み)
- 終えるときは`adb -s <シリアル> emu kill`

## 3. アプリを入れる

`crates/scitl-tauri`で行う。**確かめる前に、今のブランチからビルドし直す**。エミュレーターに前の
APKが残っていると、直したはずの箇所を古いまま見ることになる(入れた時刻は
`adb shell dumpsys package net.niigo.scitl | grep lastUpdateTime`で分かる)。

### APKを作って入れる(既定)

```sh
npm --prefix ../../frontend ci && npm ci
npx tauri android build --debug --apk --target x86_64
adb -s emulator-5556 install -r gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
adb -s emulator-5556 shell am start -n net.niigo.scitl/.MainActivity
```

同じAPKを2台へ入れられる。入れ直してもアプリのデータは残る(消すなら`adb shell pm clear net.niigo.scitl`)。

### 開発サーバーの画面を読む(`tauri android dev`)

```sh
npx tauri android dev Small_Phone_API_37
```

- 端末は**AVD名**で指定する。シリアル(`emulator-5556`)は一致せず、aarch64向けに
  ビルドしたうえでAndroid Studioを開こうとする。一致したかは最初の出力の
  `Detected connected device: <名前> ... with target "x86_64-linux-android"`で分かる
- tauri-cliは`adb reverse tcp:1420 tcp:1420`を張る。画面は`http://tauri.localhost/`で開き、Tauriが
  Rust側から開発サーバーへ中継する(遷移の判定はこれを通す。`architecture/webview-boundary.md`「Android」)。
  HMRはViteのクライアントが`ws://localhost:1420`へ直接つなぎ直して効く(コンソールに`[vite] connected.`)
- 開発サーバーのポートは1つなので、同時に読めるのは1台だけ
- 止めたら`adb reverse --remove-all`。入ったのは開発用のAPKなので、APKの確認に戻るときは入れ直す

## 4. 撮る・触る・測る

### スクリーンショット

```sh
adb -s emulator-5556 exec-out screencap -p > <スクラッチパッド>/shot.png
```

撮った画像はリポジトリの外(Claudeならスクラッチパッド)へ書く(リポジトリの中に置くと、無視の
設定に当たらずコミットに紛れ込む)。Readで開けば、Claudeがそのまま見られる。画像の座標は端末の画素で、CSSの値に
devicePixelRatioを掛けたもの(1節の表)。

### 触る

座標は端末の画素で指定する(スクリーンショットの座標と同じ)。

```sh
adb -s emulator-5556 shell input tap 360 1100                  # 入力欄等を押す
adb -s emulator-5556 shell input text 'hello'                   # フォーカスのある欄へ打つ
adb -s emulator-5556 shell input keyevent KEYCODE_BACK          # 「戻る」
adb -s emulator-5556 shell input swipe 5 640 500 640 300        # 左端から右へ300msでスワイプ
```

- 欄を押すとソフトキーボード(Gboard)が出る。キーボードが出ているかは
  `adb shell dumpsys input_method | grep mInputShown`
- `input text`は英数字だけを送れる(空白は`%s`)。日本語の変換・確定のEnterは確かめられないので実機で見る
- ジェスチャーナビゲーションでは、画面の左右の端からのスワイプはOSの「戻る」にも取られる

### 実寸を測る(WebViewのDevTools)

デバッグビルドはWebViewのデバッグが有効なので、Chrome DevTools Protocolで画面の中の値を読める。
`cdp-eval.mjs`はリポジトリ直下から呼ぶ形で書く。

```sh
P=$(adb -s emulator-5556 shell pidof net.niigo.scitl | tr -d '\r')
adb -s emulator-5556 forward tcp:9223 localabstract:webview_devtools_remote_$P
curl -s localhost:9223/json                 # 画面のURL。空なら読み込めていない
node .claude/skills/android-check/cdp-eval.mjs 9223 \
  '({ w: innerWidth, h: innerHeight, vv: visualViewport.height, dpr: devicePixelRatio, coarse: matchMedia("(pointer: coarse)").matches })'
```

- ソケット名にはプロセスIDが入る。アプリを起動し直したら転送し直す(古いソケットへの転送は
  `other side closed`で失敗する)
- 安全領域の値は`env()`をスクリプトから読めないので、測る要素を置いて測る:
  `(() => { const p = document.createElement("div"); p.style.cssText = "position:fixed;top:env(safe-area-inset-top);bottom:env(safe-area-inset-bottom)"; document.body.append(p); const r = p.getBoundingClientRect(); p.remove(); return { top: r.top, bottom: innerHeight - r.bottom }; })()`
  (API 37のエミュレーターでは上24px・下24px)
- 人が見るときは、デスクトップのChromeの`chrome://inspect/#devices`に同じWebViewが出る
- 終えたら`adb forward --remove-all`

## 5. 実機で確かめる

エミュレーターでは確かめにくいもの(日本語IMEの確定と送信の取り違え、指での押しやすさ、端末ごとの
ノッチとナビゲーション)だけを実機で見る。どれを見るかは各Issueの「実機」の節にある。

1. 端末の「開発者向けオプション」で「USBデバッグ」を有効にし、USBでつなぐ。`adb devices`に出て
   `device`になれば済み(`unauthorized`なら端末側で許可する)
2. `npx tauri android build --debug --apk --target aarch64`で作り、`adb -s <シリアル> install -r`で入れる
3. 撮る・測るは4節と同じ(`-s`に実機のシリアルを渡す)。押す・打つは手で行う

実機での`tauri android dev`(`npx tauri android dev <モデル名>`)は確かめていない。
