# データモデル: テーブルと接続

SQLiteを使う(理由は`../architecture/tech-stack.md`)。「物理削除しない」「状態は日時カラムの
有無で表現する」(`../principles.md` 2節)をスキーマで守る。

## 1. 型と形式

- **タイムスタンプ列** (`created_at` / `updated_at` / `archived_at` / `deleted_at` /
  `done_at`): `TEXT`、ISO8601のUTC(`YYYY-MM-DDTHH:MM:SSZ`)。辞書順=時系列順になり
  索引がそのまま効く。人間が直接読めるため`../principles.md` 1節の
  「データを特定ソフトに依存させない」にも合う。生成は core の1つのヘルパに集約する
- **`deadline` は日付のみ** (`TEXT`, `YYYY-MM-DD`)。タイムスタンプと意図的に別形式にする。
  日時型で持つとタイムゾーンによって締切が前後にずれる。モデルが「明日」等を日付に落とす基準は、
  モデルへ渡す送信日時の時差で伝える(`../architecture/prompt-shape.md`「ユーザー発言の送信日時は本文と分けて運ぶ」)
- **`title` は `TEXT NULL`**(`../principles.md` 2節「未設定はnullに統一」に従う。
  空文字を未設定の意味には使わない)。
  `title IS NULL` は「未設定」を意味するだけで、専用の自動命名処理の対象という意味は持たない
  (タイトルはモデルが `update_task` ツールで、または画面・CLIの名前変更で設定する。`../tools.md` 2節)。未設定のまま残った
  場合の画面表示(一覧・ヘッダー)は表示側のフォールバックで扱い、`title` には書き込まない
- 主キーは `INTEGER PRIMARY KEY`(SQLiteのrowidエイリアス)

## 2. テーブル

`messages`・`turn_transcripts`・`transcript_blobs`は`messages.md`にある。索引(3節)はすべてのテーブルの分をここに置く。

### tasks

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| title | TEXT | NULL可。NULL=未設定(表示側でフォールバック)。空文字列は書かない。長さの上限は`../tools.md` 2節 |
| description | TEXT | NULL可。NULL=未設定。空文字列は書かない。長さの上限は`../tools.md` 2節 |
| deadline | TEXT | `YYYY-MM-DD`。NULL可 |
| archived_at | TEXT | ISO8601。NULL=未アーカイブ。再度アーカイブしても上書きしない |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| created_at | TEXT | ISO8601。NOT NULL |
| updated_at | TEXT | ISO8601。NOT NULL。配下の工程への更新操作でも更新する |

優先度カラムは持たない(`../principles.md` 2節)。

`updated_at` は「タスクが最後に更新操作を受けた日時」で、工程への操作も含める(工程は
タスクの一部で、画面上もタスクの中に見える)。値が実際に変わったかは比べない(同じ値での
更新でも進む)。工程側には `updated_at` 列を持たない。

### task_steps

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| task_id | INTEGER | NOT NULL, `REFERENCES tasks(id)` |
| description | TEXT | NOT NULL。前後の空白を落として保存し、空は書かない。長さと件数の上限は`../tools.md` 2節 |
| done_at | TEXT | ISO8601。NULL=未完了 |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| order_index | INTEGER | NOT NULL |
| created_at | TEXT | ISO8601。NOT NULL |

サブタスクという独立した概念は作らない(`../principles.md` 2節)。

**論理削除の伝播について**: タスクを論理削除しても、配下の工程の `deleted_at` は
書き換えない。表示時に親の `deleted_at` を見て絞り込む。伝播させると、タスク削除の
取り消し時に「元から削除されていた工程」と「タスク削除に巻き込まれた工程」を区別できず、
論理削除を選んだ最大の理由(取り消せること)が失われるため。削除の記録(`messages.md`「応答生成以外の
経路での操作の記録」)も削除したタスクの会話に置くので、取り消せばタスクと一緒に戻る。

### attachments(添付ファイル)

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| message_id | INTEGER | NOT NULL, `REFERENCES messages(id)` |
| original_name | TEXT | NOT NULL |
| mime_type | TEXT | NOT NULL |
| kind | TEXT | NOT NULL, `CHECK (kind IN ('text','image','other'))` |
| size_bytes | INTEGER | NOT NULL |
| content_text | TEXT | NULL可(テキスト添付の本文実体) |
| file_hash | TEXT | NULL可(画像・その他添付の実体を指すハッシュ) |
| created_at | TEXT | ISO8601。NOT NULL |

`CHECK ((content_text IS NOT NULL) <> (file_hash IS NOT NULL))` —
テキスト添付とそれ以外は排他であることを制約で表現する。

外部キー制約は宣言する。ただし「物理削除しない」方針のため実際には発火しない
(意図の記録+保険)。

- `file_hash` は実体のSHA-256の小文字16進(64桁)。実体はアプリのデータディレクトリの
  `attachments/<file_hash>` に置き、同じ内容の添付は実体を共有する(`../architecture/attachments.md`)。
  実体を消してよいのは、同じ `file_hash` を指す行が1つも無くなったときだけ。送った形の保存
  (`turn_transcripts`)も同じハッシュで画像を指すが、指すのは添付の行が持つハッシュだけで、
  添付の行は消さないので、この条件は添付の行だけで判定できる
- `mime_type` は中身の先頭バイトから決めた値(`attachments::classify`)。拡張子からは決めない
- 画像は預かる時点で正規化する(`../architecture/attachments.md`)。`mime_type`・`size_bytes`・`file_hash` は
  正規化した後の実体のもので、元のファイルのものではない
- 編集で新しい発言へ引き継ぐときは行を写し、実体は共有する(`db::attachments::copy_to_message`)

## 3. 索引

- `messages(task_id, created_at)` — 会話単位の発言取得(支配的クエリ)
- `messages(turn_id, attempt_no)` — ターン単位のグルーピング
- `task_steps(task_id, order_index)`
- `attachments(message_id)`
- `attachments(file_hash)` — 実体の重複排除の判定
- `turn_transcripts(task_id)` — 会話ごとの送った形の取得

## 4. 接続時のPRAGMA

- `journal_mode = WAL` — 永続設定。書き込みは別ファイルに追記され、読み手は書き込み中でも
  止まらずに読める。GUIとCLIが**別プロセスとして同じDBファイルを触る**前提の
  ため必須
- `foreign_keys = ON` — SQLiteは既定でOFF。接続ごとに毎回設定する
- `busy_timeout` — 別プロセスが書き込み中のとき、失敗せずに待つ上限

### 複数プロセスからの書き込みの排他

GUIとCLIからの同時書き込みは、SQLite自身のロックで直列化する。ファイルロック等の
別の仕組みは持たない。GUIは原則1つしか起動しない(`../architecture/concurrency.md`「多重起動の防止」)が、
防止はベストエフォートなので、GUI同士の同時書き込みもSQLiteのロックで扱える前提を崩さない。

- **読んで判断してから書く操作は、判断に使う読み取りから書き込みまでを1つのトランザクション
  (`db::in_transaction`)に収める**。存在の確認、工程の重複除外、`attempt_no`の採番、
  更新系ツール1回分の実行(対象の確認から変更後の全体の読み直しまで)がこれに当たる。
  確認をトランザクションの外で済ませると、確認から書き込みまでの間に別プロセスが書ける
- このトランザクションは **`BEGIN IMMEDIATE`** で始める。WALでは、既定の`DEFERRED`で始めて
  読んだあとに書こうとした時点で別プロセスが先に書いていると、`busy_timeout`を待たずに
  `SQLITE_BUSY`で失敗する(読んだ時点の版が古くなっているため、待っても書けない)。
  `IMMEDIATE`は開始時に書き込みの権利を取るので、待ちは`busy_timeout`が受け持てる。
  入れ子で呼ばれた`in_transaction`は外側の開始方法を引き継ぐので、トランザクションは
  すべて`in_transaction`で始める
- **トランザクションは同期のDB関数の中で閉じ、LLM・外部ツールの呼び出しを跨がない**。
  跨ぐと、応答を待つ間ずっと他プロセスの書き込みを止め、`busy_timeout`を超えて失敗させる
- 開くときのWALへの切り替え(`PRAGMA journal_mode=WAL`)だけは`busy_timeout`が効かない。
  読み取りロックを持ったまま排他ロックへ上げるため、作成直後のDBを複数のプロセスが同時に
  開くと、SQLiteはデッドロックを避けて待たずに`SQLITE_BUSY`を返す。`db::open`は
  この文だけを、`busy_timeout`と同じ上限まで間を置いて送り直す(Issue #323)
- プロセス内では、これに加えて接続を包む`Mutex`(`db::SharedConnection`)が1本ずつに絞る
- 読むだけの処理には`in_transaction`を使わない。書き込みの権利を取って他プロセスを待たせる
  だけになる(WALでは書き込み中も読めるので、読む側は待たない)。複数の文で読むと間に
  別プロセスの書き込みが挟まりうるが、今の読み取り(履歴・画面の表示)は次に
  読み直せば揃うので、1つの時点には揃えない

DBのロックが守るのは1回の書き込みの整合性まで。応答生成中の会話に別の書き込みを入れない
排他(`in_flight.rs`)はLLMの応答を跨ぐため、同じプロセスの中でしか効かない。別プロセスとの
関係は次の通り。

- タスク・工程の操作(CLI #23)は、応答生成中の会話に操作の記録が挟まり
  うる。`messages.md`「応答生成以外の経路での操作の記録」の通り。生成中のタスクが別プロセスから
  削除されると、その応答は削除済みのタスクの会話に書かれる(画面には出ず、削除を
  取り消せば会話と一緒に戻る)
- 応答生成を行うのはGUIと`scitl-debug-cli`だけで(`scitl-cli`は行わない。`../architecture/cli.md`)、**この2つが同じ会話で同時に
  生成することは断らない**(Issue #239)。断るにはDBに生成中の印を持つことになるが、印を立てた
  プロセスが落ちると印が残り、それを見分けて外す仕組みまで要る。`scitl-debug-cli`は開発者が
  手で動かすもので、同じ会話を画面と同時に動かさないことは使う側に任せられる。同時に生成した
  場合、2つのターンは互いを読まないまま応答し、どちらも会話に残る。画面にどう並ぶか、その後の
  ターンでモデルへどう並ぶかは確かめていない。編集・再試行・発言の削除を別プロセスの生成中に行った
  場合も同じく断られない

## 5. マイグレーション

`migrations/0001_init.sql` から番号順で管理し、適用済みの数を `user_version` に持つ
(`db::migrate_to`)。今の列の有無から適用するものを推測する方式は採らない。変更が積み重なる
ほど、適用の順序と前提が読めなくなるため。

**版の読み取りから適用・版の書き込みまでを1つの`BEGIN IMMEDIATE`のトランザクションに収める**
(`db::in_transaction`)。スキーマを更新した版を起動した直後にGUIとCLIが同時に開くと、版を
トランザクションの外で読んだ側は、先に適用された版を知らないまま同じマイグレーションを
もう一度適用する(DDLのエラーで開けなくなるか、冪等でない更新が二重に掛かる)。今より新しい
版のDB(新しいビルドで開いたもの)は開かない。

**CHECK制約はCREATE TABLE時点で入れる**: SQLiteは `ALTER TABLE ADD CONSTRAINT` を
持たず、後から制約を追加するにはテーブル再構築(新テーブル作成→全行コピー→差し替え)が
必要になる。各テーブルの制約(`messages.md`の分を含む)は、そのテーブルを作るマイグレーションの
`CREATE TABLE` に含める。作った後に加わった不変条件は、再構築の代わりに `BEFORE INSERT`/`BEFORE UPDATE` の
トリガーで同じ条件を表す(例: `0002_tool_execution_role.sql`)。再構築は外部キーの一時
無効化を伴い、マイグレーションのトランザクション内では行えないため。

## 6. DBと設定ファイルの境界、秘密情報

次はDBではなく設定ファイル(`config.toml`。`config.rs`)に置く。モデルの切替やプロンプトの調整を、
DBを介さずに手軽に行えるようにするため。

- システムプロンプト(基本・タスクチャット用)と、聞き取りの開始の発言
- 登録したLLMプロバイダー(名前・API形式・接続先・モデルと能力の手動設定・思考の強さ)と、選択中のプロバイダー・モデル
- 登録した外部ツールサーバー(接続方式・接続先・有効化の状態・有効にしたツール)
- 数値の設定(応答のタイムアウト、ツール呼び出しの上限回数・タイムアウト)と表示言語

外部ツールサーバーが提供するツールの一覧と、モデルの能力の自動検出の結果は、どちらにも書かず
メモリにだけ持つ(`../tools.md` 4.5節、`../architecture/llm-adapter.md`)。

秘密情報(APIキー、外部ツールサーバーに渡すヘッダーの値)は、どちらにも平文で置かず
OSの資格情報ストアに置き、設定ファイルには参照だけを持つ(`../architecture/network-secrets.md`「秘密情報」)。
