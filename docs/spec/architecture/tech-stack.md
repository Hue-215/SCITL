# 技術選定

| 領域 | 選定 | 理由 |
|---|---|---|
| フロントエンド | React + TypeScript + Vite | 普及度が高く(月間DL数でReactは代替候補の約40倍)、エコシステムが厚い。WebViewが信頼できないモデル出力を描画する境界であることは他フレームワークでも変わらないため、Markdownサニタイズ・多言語化の枯れた部品が揃うJS側を選ぶ |
| データベース | SQLite | 「データを特定ソフトに依存させない」(`../principles.md` 1節)に合い、Rustから扱うライブラリも揃っている |
| SQLiteドライバ | `rusqlite` | 同期API。単一プロセス内は明示的な排他制御で足り、非同期ランタイムへの結合を避ける(`concurrency.md`「同期(DB)と非同期(オーケストレーション)の境界」) |
| マイグレーション | 自前(`db::migrate_to`) | 版番号で管理する通常のマイグレーションは、番号順のSQLと`user_version`だけで足りる。GUIとCLIが同時に開いても二重に適用しないよう、版の読み取りを適用と同じ即時トランザクションに収める必要があり(`../data-model/tables.md`「マイグレーション」)、既存のクレート(`rusqlite_migration`)は版をトランザクションの外で読むため使わない |
| 秘密情報ストア | `keyring-core` + OSごとの保存先クレート | keyringの現行の構成。保存先は`network-secrets.md`「保存先の選び方」 |
| Tauriバージョン | Tauri 2.x | 権限・CSPの設定機構がこのバージョン系列を前提にしている |
| LLMプロバイダ第一弾 | OpenAI互換チャットコンプリーションAPI | クラウド本家に加え、ローカル推論サーバー(llama.cpp/LM Studio/Ollama等)の多くが対応。`../principles.md` 1節「クラウド/ローカルLLMの自由な切替」を安く検証できる |
| 設定ファイル形式 | TOML | 秘密情報は含まず参照のみを持つ(`network-secrets.md`「秘密情報」) |
| 言語ファイル形式 | JSON | フロントエンド(Vite)が追加プラグイン無しに読み込める |
| 多重起動の防止(デスクトップのみ) | `tauri-plugin-single-instance` | 公式プラグインで、通信はローカルのIPCだけ。自前で持つとOSごとのIPCを2通り書くことになり、攻撃面も保守量も増える(`concurrency.md`「多重起動の防止」) |
| ネイティブのダイアログ | `tauri-plugin-dialog` | 公式プラグインで、デスクトップ(`rfd`)とAndroid(Kotlin側)の両方を持つ。画面(WebView)では作れない確認と、添付の選択画面・エクスポートの保存画面を、Rust側から出すために使う(`webview-boundary.md`「CSP / Tauri権限設定」・`attachments.md`「受け取り方」・`export.md`「Androidでの書き出し先」)。依存として`tauri-plugin-fs`も入る。どちらも画面に権限(capabilities)を与えない |
| リンクをOSへ渡す(Androidのみ) | `tauri-plugin-opener` | 公式プラグイン。`open`クレートはAndroidで動かないので、`ACTION_VIEW`のIntentで渡す(`webview-boundary.md`の外部リンクの項)。デスクトップは`open`クレートのまま。2.5に留める(Tauri 2.11のまま入る版)。画面に権限を与えない |
| Androidの`content://`のURIを開く(Androidのみ) | `tauri-plugin-fs` | 公式プラグインで、選択画面・保存画面が返すURIをAndroidのContentResolverでファイル記述子として開く(読み込み用・書き込み用)。Rust側から開くためだけに登録し、画面に権限を与えない(`attachments.md`「受け取り方」・`export.md`「Androidでの書き出し先」) |
| HTTPSの証明書の検証の初期化(Androidのみ) | `rustls-platform-verifier`・`jni` 0.22 | reqwestが証明書の検証に使うクレートへ、JNIの参照を渡す(`network-secrets.md`「Androidの信頼ルート」)。どちらも既にreqwestの依存として入っており、版はreqwestの引き込むものに合わせる。`jni`はTauri(tao・wry)の使う0.21と同居する |
| 秘密情報の保存先(Androidのみ) | `android-native-keyring-store`・`ndk-context` | `keyring-core`と同じ組織の保存先で、Keystoreの鍵で暗号化してSharedPreferencesに置く(`network-secrets.md`「Androidの保存先」)。Kotlinの部品を持たず、JavaVMとContextを`ndk-context`から取る。`jni`はTauriと同じ0.21を使い、ほかの依存(`regex`・`tracing`等)も既にある版で足りる |
| クリップボードの画像を読む(デスクトップのみ) | `tauri-plugin-clipboard-manager` | 公式プラグイン(`arboard`)。WebKitGTKは画像だけが載ったクリップボードを画面に渡さないので、画面ではなくRust側で読む(`attachments.md`「受け取り方」)。Android側は文字しか読めないので登録しない。2.4はTauri 2.12を要求するので2.3に留める。Linuxのクリップボードのために`wl-clipboard-rs`(Wayland)と`x11rb`が入り、`hashbrown`(0.15)・`foldhash`(0.1)・proc-macro経由の`quick-xml`(0.41)が既存と別の版で同居する |
| zipを書く | `zip` 8.6 | Androidで、エクスポートを1つのzipにまとめて保存画面で選んだ場所へ書く(`export.md`「Androidでの書き出し先」)。書くのはdeflateだけで、既定のfeature(ほかの圧縮方式・暗号化)は切る。新しく入るのは`zip`と`typed-path`だけで、`crc32fast`・`indexmap`・`memchr`は既にある版、deflateの`flate2`(`rust_backend`)は`image` → `png`の依存として既にあるものを使う。9.0はリリースされたばかり(2026-10)なので8に留める |

トークン数の見積もりは、現状は文字数からのフォールバック(`llm::token_estimate`)だけを持つ
(Issue #66)。トークナイザはモデルごとに違い、クレートを足しても登録されたモデルに合う保証が
無い。推論サーバーの数え上げのエンドポイントはサーバーごとに方言があり、OpenAI互換APIには無い。
どちらも実行時依存か通信の追加に当たるわりに、多めに見積もったときの損は古い発言が早めに
落ちるだけなので、見合わない。見積もった値は履歴の間引き(`orchestration::history_trim`)が
コンテキスト長(`llm-adapter.md`の能力)と比べて使う。

## 対象のOS

- **デスクトップ**: LinuxとWindows。macOSは対象にしない(資格情報の保存先が無い。`network-secrets.md`)
- **Android**: 対応を進めている(Issue #440)。Android 10以上(「AndroidのSDKの版」)。
  APKを直接入れて使う形で、ストアでの配布は考えない。
  添付・エクスポートのフォルダを開く操作は、コンパイルは通るが動かない(`open`クレートが
  デスクトップの`xdg-open`等を探して失敗を返す。open 5.4のソースで確認、2026-10)。添付のほうは
  開く操作を出さない(`attachments.md`「画面での開き方」)。本文中のリンクは
  `tauri-plugin-opener`で開く(Issue #492)。iOSは対象にしない
  - エクスポートは、Androidではフォルダを開く操作を出さず、保存画面で選んだ場所へzipで書く
    (`export.md`「Androidでの書き出し先」)

## Androidのビルド

`crates/scitl-tauri`は本体をライブラリ(`lib.rs`の`run`)に置き、デスクトップの実行ファイル(`main.rs`)と
AndroidのActivityの両方から呼ぶ。デスクトップにしか無いもの(多重起動の防止等)は`cfg(desktop)`で外す。
外し漏れはCIの`rust-android`ジョブ(`aarch64-linux-android`向けのclippy)で拾う。手元で確かめるときは、
同じジョブの`CC_aarch64_linux_android`・`AR_aarch64_linux_android`を設定して
`cargo clippy -p scitl-core -p scitl-tauri --target aarch64-linux-android`を走らせる。

Androidのプロジェクト(`crates/scitl-tauri/gen/android`)は`tauri android init`で作り、手を入れる前提で
リポジトリに置く。`tauri android init`を走らせ直すと手を入れた箇所が上書きされるので、作り直さない。
Gradleのwrapper(`gradle-wrapper.jar`)は実行されるバイナリなので、CIがGradleの公開する
チェックサムと照合する。

要る道具:

- **Android Studio**(SDKとエミュレーター、同梱のJDK)。SDK Managerで「NDK (Side by side)」と
  「Android SDK Command-line Tools」も入れる
- 環境変数: `ANDROID_HOME`(SDK)、`NDK_HOME`(`$ANDROID_HOME/ndk/<版>`)、`JAVA_HOME`(Android Studio同梱の
  JDK。Linuxなら`<Android Studio>/jbr`、Windowsなら`C:\Program Files\Android\Android Studio\jbr`)。
  JDKはGradle(8.14.3)が動く版にする。Gradle 8.14はJava 24までで、Java 25だとビルドスクリプトの段階で
  `Unsupported class file major version 69`で止まる。Android Studioの設定でGradleのJDKに新しい版を
  選んでいても、tauri-cliは`JAVA_HOME`を使う
- Rustのターゲット: `aarch64-linux-android`(実機)、`x86_64-linux-android`(多くのエミュレーター)。
  `rustup target add aarch64-linux-android x86_64-linux-android`で入れる。
  ABIを指定せずにAPKを作ると、`armv7-linux-androideabi`・`i686-linux-android`も要る
  (`gen/android`の既定は4つのABIをまとめたAPK)
- C/C++のコードを含むクレート(`rusqlite`の`bundled`のSQLite、rustlsの暗号の実装の`aws-lc-sys`)は、
  NDKのclangでビルドされる。`aws-lc-sys`はCコンパイラだけでビルドでき、cmakeは要らない
  (aws-lc-sys 0.45・NDK 30.0で、x86_64向けのAPKのビルドで確認、2026-10)

エミュレーターで起動する(`crates/scitl-tauri`で、エミュレーターを先に立ち上げておく):

- `npm --prefix ../../frontend ci`と`npm ci`のあと、`npx tauri android build --debug --apk --target x86_64`で
  画面を埋め込んだAPKを作り、`adb install`で入れる。実機なら`--target aarch64`
- `npx tauri android dev <AVD名>`は、Viteの開発サーバーの画面を読む(デスクトップの`tauri dev`と同じ)。
  画面は`http://tauri.localhost`で開き、Tauriが開発サーバーへ中継する。端末の指定の仕方と、見た目の
  確かめ方は`android-check`スキル
- 診断(`diagnostics::report`)はタグ`SCITL`、panicの文言と依存のクレートが標準エラーへ書いたものは
  タグ`RustStdoutStderr`でlogcatに出るので、`adb logcat -s SCITL RustStdoutStderr`で両方を読む
  (`sanitize.md`「無害化」の表の下)

## AndroidのSDKの版

| 値 | 版 | 書く場所 |
|---|---|---|
| minSdk(入れられる最も古い版) | 29(Android 10) | `tauri.conf.json`の`bundle.android.minSdkVersion` |
| targetSdk(どの版の動き方の決まりに従うか) | 36(Android 16) | `gen/android/app/build.gradle.kts` |
| compileSdk | 36 | `gen/android/app/build.gradle.kts`。targetSdk以上にする |

**minSdkを29にする理由**: AndroidのWebViewはChromeと同じ版の系列で更新され、Android 7.xでは
Chrome 119相当、Android 8・9ではChrome 138相当(2025-08)で更新が止まった。画面はモデルの出力を
描画する境界(`webview-boundary.md`)なので、既知の脆弱性が直らないWebViewでは動かさない。
WebViewの更新が止まる版が上がったら、minSdkもそこまで上げる。依存が求める下限はこれより低い
(Tauriの本体21、`tauri-plugin-dialog`・`tauri-plugin-opener`24、`tauri-plugin-fs`21、
`rustls-platform-verifier-android`22。2026-10)。ライブラリの下限がアプリより高いとGradleの
マニフェストの統合が止まるので、Gradleに組み込んだ依存の超過はビルドで分かる。

**minSdkの書く場所**: tauri-cliはRust側のビルドで、NDKのclangを選ぶ版(`aarch64-linux-android29-clang`等)に
`tauri.conf.json`の値を使う。APKの下限とネイティブのコードの版が食い違わないよう、Gradleと
CIの`rust-android`ジョブも同じ値を読む。GradleとCIが読むのは`tauri.conf.json`だけなので、
minSdkを`tauri.android.conf.json`や`--config`で上書きしない(tauri-cliだけが上書き後の値を使い、
食い違う)。`tauri.conf.json`にtargetSdkの項目は無いので、targetSdkとcompileSdkはGradleに置く。

**targetSdkを36に留める理由**: APKを直接渡すので、Google Playのtarget SDKの下限には縛られない。
37以上にすると、Android 17の端末ではLAN上の機器への通信に実行時の権限`ACCESS_LOCAL_NETWORK`
(権限のグループは「付近のデバイス」)が要る。36以下なら`INTERNET`だけで暗黙に許される
(一時的な措置とされている)。許可を求める処理はKotlinで書くことになる(RustからはOSの許可の
ダイアログを出せず、公式に汎用の権限のプラグインも無い)ので、ローカルの推論サーバーへつなぐ
Android対応の最初の段階では上げない。36以下の間は、`ACCESS_LOCAL_NETWORK`をマニフェストに
書かず、実行時にも求めない(公式文書の指示。https://developer.android.com/privacy-and-security/local-network-permission)。

targetSdk 36で既に掛かっている決まり(Android 16以上の端末):

- 画面がステータスバー・ナビゲーションバーの下まで広がり、オプトアウトできない(edge-to-edge)
- 「戻る」の予測アニメーションが既定で有効になり、`onBackPressed`は呼ばれず、`KEYCODE_BACK`も
  届かない。「戻る」を受けるにはandroidxの`OnBackPressedCallback`を使う(Tauri本体がこれで受けている)。
  マニフェストの`android:enableOnBackInvokedCallback="false"`による一時的なオプトアウトもあるが、使わない。
  Tauri本体(`AppPlugin`)の受け手は、画面に受け手があれば画面へ知らせ、無ければWebViewの履歴を戻るか
  `Activity.onBackPressed()`へ渡す(wryの受け手は、Tauriの`TauriActivity`が`handleBackNavigation`を
  切るので登録されない)。`../ui.md`「指で操作する端末」の「戻る」

targetSdkを37以上へ上げるときに見直すこと:

- **ローカルネットワークの権限**: マニフェストでの宣言と、実行時に許可を求める処理(Tauriの
  プラグインの権限の仕組み`@Permission`を使うKotlin)。求める時機(プロバイダーの登録時か、
  プライベートIPへの初めての送信時か)。拒否・後からの取り消し・使っていないアプリの権限の
  自動リセットのどれでも、通信の失敗として画面に出すこと。拒否されているときのTCPの接続は
  多くがタイムアウトで失敗する(公式文書)ので、失敗の種類の出し方も見直す
- **端末の中の推論サーバー**: Android 16の動作の変更の文書が挙げるローカルネットワークの範囲
  (ブロードキャストできるインターフェースのリンクローカル・CGNAT・プライベートIP、マルチキャスト)に
  ループバック(`127.0.0.0/8`)は含まれないが、対象外とする明記は無い(2026-10。別の仕事用
  プロファイルとの間のループバックを止める変更は別にある)。上げる前に、権限の無い状態で
  エミュレーターから確かめる
- **証明書の検証**: Certificate Transparencyの検査が既定で有効になる。HTTPSの検証はAndroidの
  証明書の検証を呼ぶ(`rustls-platform-verifier`)ので、この検査が掛かるか、掛かって困る
  通信先が無いかを確かめる
- 大きい画面(最小幅600dp以上)では向き・縦横比・サイズ変更の制限が無視され、36で使えた
  オプトアウトが無くなる。SCITLは制限を掛けていないので影響しない見込み

## ライセンス

- **MPL-2.0は許容する**(ファイル単位の弱いコピーレフトで、SCITL自身のコードのライセンスを縛らない)。
  実行バイナリに入るのは`option-ext`(`dirs`経由。Tauri自身も使う)だけで、`selectors`・`cssparser`・
  `dtoa-short`はビルド時のみ。npmのMPL-2.0は開発依存の`lightningcss`だけ。GPL・LGPL・AGPLは無い。
  同梱フォントはSIL OFL 1.1(同梱は可、フォント自体はOFLのまま)。同梱アイコン(Material Symbolsの
  SVGの形を`frontend/src/Icon.tsx`に写したもの)はApache-2.0で、配布物の第三者ライセンスの一覧に
  ライセンス文(`frontend/src/icons/MaterialSymbols-LICENSE.txt`)を入れる(`scripts/assemble-dist.mjs`)。
  npmのパッケージとしては入れていない(写し元は`@material-symbols/svg-400`)。ライセンスはMIT予定なので、
  コピーレフトのライブラリは使わない
- **BSL-1.0(Boost Software License)は許容する**(MITと同じ許容型で、コピーレフトではない。バイナリでの
  配布には表示も求めない)。入るのはWindowsのクリップボードを読む`clipboard-win`と、その依存の`error-code`
  (`tauri-plugin-clipboard-manager`経由。Issue #476)
- **AndroidのAPKには、Gradleで解決する依存も入る**(androidx・Material・Kotlinの標準ライブラリ等と、
  `rustls-platform-verifier`のKotlinの部品)。cargo-aboutの一覧(`about.toml`の`targets`)はAndroidを
  対象にしておらず、Gradleの依存はそもそも一覧に現れない。Androidを配布するときに、Gradleの依存の
  一覧を別に組み立てる。Kotlinの部品はクレート`rustls-platform-verifier-android`(MIT OR Apache-2.0)の
  中身なので、そのクレートの項で足りる。ただしパッケージにライセンスファイルが無い(0.1.1)ので、
  Androidを対象に足すときに`licenses/`のSUPPLIEDに入れる
- **Windowsの実行ファイルには、MicrosoftのWebView2 SDKのローダーが入る**(`webview2-com-sys`が
  `WebView2LoaderStatic.lib`を静的にリンクする)。SDKのライセンスはBSD-3-Clauseと同じ形の条項で、
  バイナリでの配布にも著作権表示とライセンス文の同梱を求める。クレートのライセンス(MIT)とは別に、
  配布物の第三者ライセンスの一覧に載せる(`licenses/README.md`)

## 依存の脆弱性

- **GitHubのDependabotアラートで知らせる**。`Cargo.lock`と`frontend/package-lock.json`の依存に既知の
  脆弱性が公開されると通知が来る。自動でPRを作る機能(security updates)は使わない。依存の更新は、
  実行時依存の変化をOpusで見る条件(CLAUDE.md)や、版を留めている依存
  (`concurrency.md`「多重起動の防止」のプラグイン)があり、機械的には上げられない
- CIでは走らせない。PRと関係なく新しい脆弱性が公開されただけで落ち、無関係な変更を止めるため
- リリースの前に、開いているアラートが無いことを確かめる(`.claude/skills/release-build`「リリースの流れ」)。
  上げられないもの(GTK3のRustバインディングが0.18系で止まっているため、Linuxの`glib`は0.18系に留まる等)は、
  影響する箇所が呼ばれないことを確かめ、理由を添えて閉じる(dismiss)
