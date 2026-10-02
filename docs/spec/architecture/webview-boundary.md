# WebViewとの境界

## IPCコマンドの設計

フロントエンドは秘密情報を一切受け取らず、DB/ネットワークに直接触れない。
コマンド名は「デシリアライズ→coreを1つ呼ぶ→シリアライズ」の単一の機能に対応する
narrow な verb-noun とし、`run_query` のような汎用コマンドは作らない(コマンド名を
見るだけで権限範囲が監査できる状態を保つ)。

コマンドが受け渡す型はRust側を正本とし、TypeScriptの定義(`frontend/src/bindings/`)は
`ts-rs`で生成する。手で写すと、Rust側の変更に追従し損ねても型検査が通ってしまうため。
生成は`cargo test`が行い、CIは生成し直した結果とコミット済みの生成物が一致することを確かめる。

コマンドの失敗は`commands::CommandError`の1つの型で返し、画面へ渡す形(`CoreError`の表示文の
文字列)をそこだけで決める。各コマンドは`?`で返すだけにする。

`get_current_task_detail` のようなモデル向けツール(引数なし、対象はターン開始時に
オーケストレーション側が固定)と、フロントエンド向けIPCコマンドの `task_id` 引数
(フロントエンドが表示中のタスクとして渡す)は**区別されるべき別の境界**であり、
ツール登録箇所にその旨のコメントを残す。

## CSP / Tauri権限設定

この見出しの内容を変える差分は、Opusでレビューする(CLAUDE.md「Opusでレビューする条件」)。

- `default-src 'self'` / `img-src 'self' data:`(外部画像を読み込ませない。
  本文中の画像記法をリンクへ変換する自前処理と合わせた多層防御。片方が破れても止まる)/
  `script-src 'self'`(CDN・inline eval不可)
- `connect-src ipc: http://ipc.localhost`。全通信はRust側で行う設計なので、WebViewからの
  外部接続は本来ゼロのはず。許可しているのはRust側へのIPCの窓口(Linux・macOSは`ipc:`、
  Windowsは`http://ipc.localhost`。`useHttpsScheme`を有効にしたら`https://`に替える)だけで、これ以外に広げる必要が出たら「Rustが全通信を担う」
  境界が破れた合図。IPCの窓口を塞ぐと、Tauriは`postMessage`へ黙って切り替えて動き続けるが、
  そちらは本文を必ずJSONにするため、生のバイト列を受け取るコマンド(添付の`stage_attachment`)
  だけが本番ビルドで失敗する。開発時(devUrl)はこの差が表に出ないので、本番ビルドで確かめる
- 開発時のVite HMRはWebSocketを使うため、Tauri 2の `devCsp` を本番CSPと分離して設定する
  (開発と本番で同じCSPにしようとして本番を緩めるのが典型的な失敗)
- **自前コマンドは、既定ではcapabilitiesに関係なくWebViewから到達できる**。ACLに載せるには、
  `build.rs`で`tauri_build::AppManifest::commands`にコマンドを並べ、capabilitiesで許可する
  (tauri-build 2.6で確認)。今はウィンドウが1つで、そのウィンドウがすべてのコマンドを使うので
  載せていない。ウィンドウやWebViewを足すときは載せる。実際の制御点は「必要なコマンドだけ
  登録する」+ CSP + Isolationパターン(IPCを仲介する隔離iframe。多層防御として採否を検討する)
- **WebViewを乗っ取られても、ファイルを読ませない・コマンドを実行させない**。パスを受け取る
  コマンドを持たず(`attachments.md`「受け取り方」・`export.md`)、外部ツールサーバーを子プロセスとして
  起動する方式も持たない(`../tools.md`「外部(MCP)ツールの公開」)。一方、プロバイダーやMCPサーバーの
  登録はIPCで行うので、攻撃者のURLを登録して選ばせ、会話を送らせることはできる。WebViewが既に
  見えている会話のデータを持ち出せる経路として受け入れている。塞ぐには、新しい通信先の登録を
  ネイティブのダイアログで確かめる必要がある
- **Tauriのドロップの受け口を切らない**(`dragDropEnabled`は既定の有効のまま)。窓に落とした
  ファイルのパスはRust側にだけ届き、画面へは名前だけを知らせ、画面が受け付けたものをRust側が読む
  (`attachments.md`「受け取り方」)。切ってWebView標準のドロップにすると、落としたフォルダ以下をWebViewが読めるように
  なり(`webkitGetAsEntry`)、WindowsのWebView2で外からのドロップを止める設定
  (`SetAllowExternalDrop(false)`)も外れる。受け口はパスを`tauri://drag-enter`・`tauri://drag-drop`の
  イベントとしても出すが、画面はイベントを聞く権限を持たない(下の「途中経過の通知」の項)ので、
  WebViewには届かない。知らせ先を受け取る`watch_dropped_files`はChannelだけを、読ませる
  `stage_dropped_file`はドロップの番号と並びの位置だけを引数に取り、パスを受け取らない。WebViewを
  乗っ取られても、読めるのは利用者が最後に落としたファイルだけになる
  - 受け口は、落としたフォルダをassetプロトコルの許可範囲にも足す(Tauri 2.11で確認)。今は
    `protocol-asset`のfeatureを有効にしていないので害は無いが、**有効にすると、落としたフォルダ以下を
    WebViewが読めるようになる**。有効にするなら、この扱いを先に見直す
  - 残っているおそれ(未確認): Linux(WebKitGTK)の受け口は、窓の中から始まったドラッグと外から来た
    ドラッグを区別せず、`text/uri-list`のURIをパスとして渡す。乗っ取られた画面が、掴ませた要素の
    ドラッグに`file://`のURIを載せ、利用者がそれを窓の中で落とすと、利用者が選んでいないファイルを
    読ませられるかもしれない。WebKitGTKが画面から`file://`のURIを載せさせるかは実機で確かめていない。
    正規の画面は本文のリンクに`href`を持たせない(`Markdown.tsx`)ので、乗っ取られていることが前提になる
- **WebViewのクリップボードの設定を有効にしない**(Tauriの`enable_clipboard_access`。既定の無効のまま)。
  無効なら、画面がClipboard APIでクリップボードを読めるのはキー操作の貼り付けの中だけになる(WebKitGTK
  2.52.6で確認。貼り付けの画像の読み直しはこれに頼る。`attachments.md`「受け取り方」)。有効にすると、
  乗っ取られた画面がボタンのクリックだけでクリップボードの画像やファイルのURI(`text/uri-list`)を
  読めるようになり、WindowsではWebView2の読み取りの許可も自動で通る
- 外部リンクはWebViewから直接開かせない。確認ダイアログに出す判定(スキーム許可リスト・
  ホモグラフ・ユーザー情報)はRust側の `inspect_link` が返し、開く側の `open_confirmed_link` は
  同じ判定を**Rust側でやり直し**、許可された形に正規化したURLだけをOSに委譲する
  (WebViewの検査結果を信用しない)。ただし「確認ダイアログを経たこと」自体はRust側では
  検証できない。WebView内でスクリプトを実行されると、許可リスト内のURLは確認なしで
  開かれうる。これを塞ぐには確認をRust側のネイティブダイアログで出す必要があり、採否は別途検討する
- 応答の途中経過の通知は、コマンド引数の`ipc::Channel`で行い(`concurrency.md`「プロセス間の受け渡し」)、グローバルなイベント
  (`emit`/`listen`)は使わない。`listen`には`core:event`の権限を足す必要があり、WebViewが
  聞けるイベントの範囲も広がる。Channelの大きなペイロードの取得(`plugin:__TAURI_CHANNEL__|fetch`)は
  権限の検査の対象外で、capabilitiesを足さずに届く(Tauri 2.11で確認)。Tauriを更新して
  これが通らなくなると、8KBを超える通知から先の途中経過がそのターンの間止まる(完了後の
  読み直しでは出る)
- Tauriのupdaterプラグインを有効化しない(`../principles.md` 1節「ローカル完結」)
- 多重起動の防止(`tauri-plugin-single-instance`)はJSのAPIを持たず、capabilitiesに権限を
  足さない。`deep-link`のfeatureは有効にしない(`concurrency.md`「多重起動の防止」)

## 画面側で持つ処理

ライブラリに任せず画面側で持つもの: 思考とツール呼び出しの時系列マージ表示、編集/再試行に伴う
論理削除と再生成のフロー制御(タスクごとに独立した応答待ち管理を含む)。
