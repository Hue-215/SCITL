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
| ネイティブのダイアログ | `tauri-plugin-dialog` | 公式プラグインで、デスクトップ(`rfd`)とAndroid(Kotlin側)の両方を持つ。画面(WebView)では作れない確認と、添付の選択画面を、Rust側から出すために使う(`webview-boundary.md`「CSP / Tauri権限設定」・`attachments.md`「受け取り方」)。依存として`tauri-plugin-fs`も入る。どちらも画面に権限(capabilities)を与えない |
| リンクをOSへ渡す(Androidのみ) | `tauri-plugin-opener` | 公式プラグイン。`open`クレートはAndroidで動かないので、`ACTION_VIEW`のIntentで渡す(`webview-boundary.md`の外部リンクの項)。デスクトップは`open`クレートのまま。2.5に留める(Tauri 2.11のまま入る版)。画面に権限を与えない |
| Androidの`content://`のURIを開く(Androidのみ) | `tauri-plugin-fs` | 公式プラグインで、選択画面が返すURIをAndroidのContentResolverでファイル記述子として開く。Rust側から開くためだけに登録し、画面に権限を与えない(`attachments.md`「受け取り方」) |
| クリップボードの画像を読む(デスクトップのみ) | `tauri-plugin-clipboard-manager` | 公式プラグイン(`arboard`)。WebKitGTKは画像だけが載ったクリップボードを画面に渡さないので、画面ではなくRust側で読む(`attachments.md`「受け取り方」)。Android側は文字しか読めないので登録しない。2.4はTauri 2.12を要求するので2.3に留める。Linuxのクリップボードのために`wl-clipboard-rs`(Wayland)と`x11rb`が入り、`hashbrown`(0.15)・`foldhash`(0.1)・proc-macro経由の`quick-xml`(0.41)が既存と別の版で同居する |

トークン数の見積もりは、現状は文字数からのフォールバック(`llm::token_estimate`)だけを持つ
(Issue #66)。トークナイザはモデルごとに違い、クレートを足しても登録されたモデルに合う保証が
無い。推論サーバーの数え上げのエンドポイントはサーバーごとに方言があり、OpenAI互換APIには無い。
どちらも実行時依存か通信の追加に当たるわりに、多めに見積もったときの損は古い発言が早めに
落ちるだけなので、見合わない。見積もった値は履歴の間引き(`orchestration::history_trim`)が
コンテキスト長(`llm-adapter.md`の能力)と比べて使う。

## 対象のOS

- **デスクトップ**: LinuxとWindows。macOSは対象にしない(資格情報の保存先が無い。`network-secrets.md`)
- **Android**: 対応を進めている(Issue #440)。APKを直接入れて使う形で、ストアでの配布は考えない。
  添付・エクスポートのフォルダを開く操作は、コンパイルは通るが動かない(`open`クレートが
  デスクトップの`xdg-open`等を探して失敗を返す。open 5.4のソースで確認、2026-10)。本文中のリンクは
  `tauri-plugin-opener`で開く(Issue #492)。iOSは対象にしない

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
- `npx tauri android dev`は、Viteの開発サーバーの画面を読む(デスクトップの`tauri dev`と同じ)
- 診断(`diagnostics::report`)はタグ`SCITL`、panicの文言と依存のクレートが標準エラーへ書いたものは
  タグ`RustStdoutStderr`でlogcatに出るので、`adb logcat -s SCITL RustStdoutStderr`で両方を読む
  (`sanitize.md`「無害化」の表の下)

## ライセンス

- **MPL-2.0は許容する**(ファイル単位の弱いコピーレフトで、SCITL自身のコードのライセンスを縛らない)。
  実行バイナリに入るのは`option-ext`(`dirs`経由。Tauri自身も使う)だけで、`selectors`・`cssparser`・
  `dtoa-short`はビルド時のみ。npmのMPL-2.0は開発依存の`lightningcss`だけ。GPL・LGPL・AGPLは無い。
  同梱フォントはSIL OFL 1.1(同梱は可、フォント自体はOFLのまま)。ライセンスはMIT予定なので、
  コピーレフトのライブラリは使わない
- **BSL-1.0(Boost Software License)は許容する**(MITと同じ許容型で、コピーレフトではない。バイナリでの
  配布には表示も求めない)。入るのはWindowsのクリップボードを読む`clipboard-win`と、その依存の`error-code`
  (`tauri-plugin-clipboard-manager`経由。Issue #476)
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
