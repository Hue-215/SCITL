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

**変更のたびにOpusレビュー必須**(CLAUDE.mdの条件。設定ファイル自体がセキュリティ境界のため)。

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
- **自前コマンドはcapabilities/permissionsでは絞れない**(capabilities/permissionsが
  規定するのは主にプラグインのコマンドで、`invoke_handler` に登録した自前コマンドは
  既定でWebViewから到達可能)。実際の制御点は「必要なコマンドだけ登録する」+ CSP +
  Isolationパターン(IPCを仲介する隔離iframe。多層防御として採否を検討する)
- 外部リンクはWebViewから直接開かせない。確認ダイアログに出す判定(スキーム許可リスト・
  ホモグラフ・ユーザー情報)はRust側の `inspect_link` が返し、開く側の `open_confirmed_link` は
  同じ判定を**Rust側でやり直し**、許可された形に正規化したURLだけをOSに委譲する
  (WebViewの検査結果を信用しない)。ただし「確認ダイアログを経たこと」自体はRust側では
  検証できない。WebView内でスクリプトを実行されると、許可リスト内のURLは確認なしで
  開かれうる。これを塞ぐには確認をRust側のネイティブダイアログで出す必要があり、採否は別途検討する
- 応答の途中経過の通知は、コマンド引数の`ipc::Channel`で行い(3節)、グローバルなイベント
  (`emit`/`listen`)は使わない。`listen`には`core:event`の権限を足す必要があり、WebViewが
  聞けるイベントの範囲も広がる。Channelの大きなペイロードの取得(`plugin:__TAURI_CHANNEL__|fetch`)は
  権限の検査の対象外で、capabilitiesを足さずに届く(Tauri 2.11で確認)。Tauriを更新して
  これが通らなくなると、8KBを超える通知から先の途中経過がそのターンの間止まる(完了後の
  読み直しでは出る)
- Tauriのupdaterプラグインを有効化しない(`../principles.md` 1節「独自の判断で
  通信先を増やさない」)
- 多重起動の防止(`tauri-plugin-single-instance`)はJSのAPIを持たず、capabilitiesに権限を
  足さない。`deep-link`のfeatureは有効にしない(2節「多重起動の防止」)

## フロントエンド固有の注意点

- 引き続き自前で持つ必要があるもの: 思考とツール呼び出しの時系列マージ表示、
  編集/再試行に伴う論理削除と再生成のフロー制御(タスクごとに独立した応答待ち管理を含む)
