# データモデル(確定スキーマ)

`../legacy/data-model.md` は旧実装の記録であり、この文書が再構築における正とする。
食い違いがある場合は必ずこの文書に従う(例: `title` の未設定表現)。

引き続きSQLiteを採用する(`../principles.md` 1節「データを特定ソフトに依存させない」、
`../legacy/data-model.md` 冒頭)。「物理削除しない」「状態は日時カラムの有無で表現する」
という上位方針(`../legacy/data-model.md` 1節)は維持する。

## 1. 型と形式

- **タイムスタンプ列** (`created_at` / `updated_at` / `archived_at` / `deleted_at` /
  `done_at`): `TEXT`、ISO8601のUTC(`YYYY-MM-DDTHH:MM:SSZ`)。辞書順=時系列順になり
  索引がそのまま効く。人間が直接読めるため`../principles.md` 1節の
  「データを特定ソフトに依存させない」にも合う。生成は core の1つのヘルパに集約する
- **`deadline` は日付のみ** (`TEXT`, `YYYY-MM-DD`)。タイムスタンプと意図的に別形式にする。
  日時型で持つとタイムゾーンによって締切が前後にずれる
- **`title` は `TEXT NULL`**(`../principles.md` 2節「未設定はnullに統一」に従う。
  `../legacy/data-model.md` の「空文字が未設定」は旧実装の記録であり本スキーマでは採らない)。
  `title IS NULL` は「未設定」を意味するだけで、専用の自動命名処理の対象という意味は持たない
  (タイトルはモデルが `update_task` ツールで設定する。`tools.md` 2節)。未設定のまま残った
  場合の画面表示(一覧・ヘッダー)は表示側のフォールバックで扱い、`title` には書き込まない
- 主キーは `INTEGER PRIMARY KEY`(SQLiteのrowidエイリアス)

## 2. テーブル

### tasks

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| title | TEXT | NULL可。NULL=未設定(表示側でフォールバック) |
| description | TEXT | NULL可。NULL=未設定。空文字列は書かない |
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
| description | TEXT | NOT NULL。前後の空白を落として保存し、空は書かない |
| done_at | TEXT | ISO8601。NULL=未完了 |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| order_index | INTEGER | NOT NULL |
| created_at | TEXT | ISO8601。NOT NULL |

サブタスクという独立した概念は作らない(`../principles.md` 2節)。

**論理削除の伝播について**: タスクを論理削除しても、配下の工程の `deleted_at` は
書き換えない。表示時に親の `deleted_at` を見て絞り込む。伝播させると、タスク削除の
取り消し時に「元から削除されていた工程」と「タスク削除に巻き込まれた工程」を区別できず、
論理削除を選んだ最大の理由(取り消せること)が失われるため。

### messages(発言・ツール実行記録)

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| task_id | INTEGER | NULL可。NULL=総合チャット |
| role | TEXT | NOT NULL, `CHECK (role IN ('user','assistant','tool','error'))` |
| content | TEXT | NOT NULL |
| kind | TEXT | NOT NULL, `CHECK (kind IN ('normal','tool_execution'))` |
| source | TEXT | NULL=内部。外部(MCP)経由には印を付ける |
| reasoning | TEXT | NULL可。表示・エクスポート専用、APIには送らない |
| error_kind | TEXT | NULL可。`role='error'`のときのみ非NULLで、安定した種別コードを持つ(例: `no_model`, `context_exceeded`) |
| error_detail | TEXT | NULL可。`role='error'`の行だけが持てる失敗の詳細(下記「エラー発言の詳細」)。空文字は不可 |
| turn_id | TEXT | NULL可(下記「ターン境界」参照) |
| attempt_no | INTEGER | NULL可 |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| created_at | TEXT | ISO8601。NOT NULL |

`CHECK (kind <> 'tool_execution' OR json_valid(content))` — ツール実行記録の `content` は
構造化データ(JSON)であることを制約で保証する。

`CHECK ((role = 'error') = (error_kind IS NOT NULL))` — エラー性を表すフラグは `role='error'`
の1箇所に集約し、`error_kind` はその種別の記録専用にする。両方でエラー性を表すと
「これはエラーか」の答えが2箇所に散るため。`error_kind` の値そのものにはCHECKを付けない
(閉じた集合の`role`・`kind`と異なり種別は今後増えるため、SQL側にも列挙を置くと同じ判断が
2箇所に散る)。

**エラー発言の`content`**: ユーザー向けの定型文言だけを入れる(`content`はNOT NULL、
エクスポートにもそのまま乗る)。APIから返る生の詳細は種別によらず`content`に混ぜない。

**エラー発言の詳細(`error_detail`)**(Issue #159): 失敗の原因を後から追うための情報。
HTTPエラーなら状態コードとプロバイダーの応答本文、HTTP応答を伴わない失敗なら通信の失敗の
文言を原因の連鎖まで連ねたもの(接続の拒否・タイムアウト・証明書エラーを区別できるように
する)、内部エラーならその種類を表す短い識別子を持つ。通信の失敗の詳細は要求URLを含めないが、
接続先のホスト名・証明書に書かれた名前・解釈できなかった応答本文の断片は含みうる。どれも
秘密情報ではなく、証明書の名前は通信経路上の第三者も決められる文字列なので、下記の
サニタイズと表示の扱い(プレーンテキストで描き、モデル入力とエクスポートに含めない)で受ける。

- 載せてよいのは、アダプタがサニタイズした文字列(送信した鍵の伏せ字・制御文字と不可視の
  書式文字の除去・長さの上限。`llm::ErrorDetail`)と、秘密情報を含まない識別子だけ。
  鍵ストア・設定ファイル・MCPサーバー由来の失敗は、鍵名・パス・URLを含みうるため詳細を
  持たない。どの種別が詳細を持つかは`orchestration::TurnFailure`のバリアントの形で決め、
  分類の1箇所に閉じる
- **画面の「詳細を表示」専用**。外部から来た文字列なので、モデルへの入力(履歴・
  システムプロンプト・ツール結果)とエクスポートには含めない。画面ではMarkdownとして
  解釈せず、プレーンテキストのまま描く
- `error_detail IS NULL OR (role = 'error' AND error_detail <> '')` を
  `0003_error_detail.sql` のトリガーで強制する

**`role='tool'` はツール実行記録専用**(Issue #131): `kind='tool_execution'` の行は、
SCITL自身のターンの記録か外部(MCP)経由の記録かによらず `role='tool'` とし、それ以外の
行は `'tool'` を使わない。記録の中身は呼び出し(ツール名・引数)と結果の組で、ユーザーの
発言でもモデルの発言でもないため。`(role = 'tool') = (kind = 'tool_execution')` は
`0002_tool_execution_role.sql` のトリガーで強制する(SQLiteは既存テーブルへCHECKを後から
足せないため、トリガーで同じ条件を表す)。

**事実系の結果を次ターン以降の履歴に載せる場合も、行を複製しない**(Issue #11): ツール
実行結果のうち「現在の状態」として言い表せないもの(検索結果・外部MCPツールの出力等)は
次ターン以降の履歴にも載せるが、そのために `kind='normal'` の行を別に書かず、実行記録の
行から履歴を組み立てる。同じ結果を2行に持つと、編集・再試行のカスケード(通常発言だけを
消す)で片方だけが消えて食い違うため。タスク・工程の更新のように**次のターン以降は**
最新状態JSONで代替できるものは、実行記録としては残すが会話履歴には投入しない。
同一ターン内のツール呼び出しループでは分類によらず結果をモデルに返すが、その返却分は
リクエストの組み立てにのみ使い、このテーブルの行としては保存しない(`tools.md` 4節)。
分類はツール定義の属性。実行時に決まった値を実行記録のJSONに書き写し、列・制約・クエリでは
扱わない(詳細は `tools.md` 4節)。

**ツール実行記録の`content`**: SCITL自身のターンの記録は次のキーを持つJSONオブジェクト
(形の定義は`orchestration::tool_record`の1箇所)。

| キー | 内容 |
|---|---|
| `tool` | モデルが呼んだ名前(外部ツールは公開した名前`サーバー識別子__ツール名`) |
| `arguments` | 引数。JSONとして読めなかった引数は、モデルが出した生の文字列 |
| `result` | 結果。失敗は最上位の`error`キーで表す(画面の判定と同じ基準) |
| `tool_kind` | `"state"`か`"fact"`。実行した呼び出しにだけ付ける。実行しなかった呼び出し(引数が読めない・公開していない名前・接続先が無い)と、Issue #11より前の記録には無い |
| `call_id` | プロバイダーが払い出した呼び出しID。払い出されなければ無い。記録のためだけに持ち、履歴の組み立てには使わない(`architecture.md` 3節) |

**1ターン内の往復で保存するもの**(Issue #131): 1ターンが確定したときに残る行は、
ツール呼び出し1件ごとの実行記録と、ターンの返信1行(成功時は通常発言、失敗時はエラー発言)。
モデルがツール呼び出しに添えて書いた本文は、そのターンの返信の一部として最終行に
まとめる(ラウンドの順に空行で区切る)。途中のラウンドを別の通常発言にしないのは、
「ターンの最終行が返信」という前提(表示・再試行・削除の対象がこの1行に決まる)を
保つため。ターンが失敗した場合、途中の本文は保存しない(`../principles.md` 3節
「保存するのは組み立て終わった応答」)。思考は各ラウンドの最初の実行記録、最終ラウンドの
ものは返信の行の `reasoning` 列に持たせる。

**ツール実行記録は通常発言の編集・削除・再試行の対象に含めない**(会話の整合性より
実行記録の保全を優先する)。監査ログ専用の別テーブルは作らない(`../principles.md` 2節)。

#### ターン境界(既知の不具合の構造的修正)

`../principles.md` 3節「ツール実行記録にはターンの境界を持たせる」への対応。

```sql
turn_id     TEXT NULL,   -- 応答生成の論理的な1ターン。再試行をまたいで同一
attempt_no  INTEGER NULL -- 同一ターン内の試行回数。再試行で増える
```

行の種類ごとに、両方を設定するか両方NULLかが決まる。**この3分類を取り違えると、
外部連携の書き込みが弾かれたり記録が表示から消えたりする**ため、実装前に必ず確認すること。

| 行の種類 | turn_id / attempt_no |
|---|---|
| ユーザー発言 | **両方NULL**(「応答生成の試行」に属さないため) |
| SCITLの応答生成に属する発言・ツール実行記録(`source IS NULL`) | **両方必須** |
| 外部(MCP)経由のツール実行記録(`source IS NOT NULL`) | **両方NULL** |

3つ目は**外部のLLMがMCP経由でSCITLを操作した向き**を指す。逆向き、つまりSCITL自身が
応答生成の途中で外部のツールサーバーを呼んだ記録(Issue #44)は2つ目に当たり、`turn_id`/
`attempt_no`を持ち`source`は付けない。`source`は「SCITLの外から操作された」ことの印であって
「外部と通信した」ことの印ではない。ここを取り違えると、自分のターンの記録が会話から
独立した行として現れる(表示側は`turn_id`の有無でこの2つを見分けている)。

3つ目が重要: 外部のLLMがMCP経由でSCITLを操作した場合、SCITL側では応答生成を行って
いないため、属するべきターンが存在しない。ここを「ツール実行記録なら必ずturn_idを持つ」と
誤解して `NOT NULL` や `CHECK (role = 'user' OR turn_id IS NOT NULL)` を書くと、
外部連携の書き込みが実行時に失敗する(しかもCHECK制約は後から外せない)。

制約として表現できるのは、**両方セットか両方NULLかのどちらかである**という不変条件のみ:

```sql
CHECK ((turn_id IS NULL) = (attempt_no IS NULL))
```

- 再試行時は同一 `turn_id` のまま `MAX(attempt_no) + 1` を採番し、
  **古い試行の記録は消さない**(「物理削除しない」方針と一致し、監査記録としても保全される)
- 表示・エクスポートは、**ユーザー発言と外部経由の記録(`turn_id IS NULL` の行)は常に表示**し、
  `turn_id` を持つ行だけを `turn_id` ごとの `MAX(attempt_no)` で絞る。
  `turn_id` を持つ行だけに絞ってから最大値を取らないと、外部経由の操作記録が
  表示・エクスポートから丸ごと落ちる(外部経由の操作が記録に残ることは
  `../legacy/backend.md` 9節の要件)
- AI主導のヒアリング開始(ユーザー発言なしで始まる)にも対応できるよう、`turn_id` は
  ユーザー発言への外部キーではなく独立した識別子(ULID等)にする
- **通常発言が1行も生き残っていないターンは、表示・エクスポートから丸ごと外す**
  (Issue #95)。編集・再試行のカスケードは `kind='normal'` しか論理削除しないため、
  破棄されたターンのツール実行記録だけが残る。これを会話に並べると、編集後の発言が
  その下に来て新規送信と区別が付かない。**保全(行はDBに残す)と表示(会話には出さない)を
  切り離すのが要点**で、辻褄を合わせるために記録の側を消してはならない。
  この判定は取得クエリ(`list_for_task`)に持たせ、表示側には持たせない
  (同じ判断を2箇所に置かない。`../principles.md` 5節)

なお、編集はこの帰結として**元の位置が保たれる**。編集後の本文は新しい行として挿入されるが、
生き残る通常発言はすべて対象より小さい `id` なので、破棄されたターンの記録さえ会話から
外れれば、新しい行は編集前と同じ位置に並ぶ。編集前の本文は `deleted_at` の立った行として
残るため、「物理削除しない」方針(`../principles.md` 2節)も満たす。編集済みであることを
示す列や、上書き前の本文を退避する表は持たない。

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

## 3. 索引

- `messages(task_id, created_at)` — チャンネル単位の発言取得(支配的クエリ)
- `messages(turn_id, attempt_no)` — ターン単位のグルーピング
- `task_steps(task_id, order_index)`
- `attachments(message_id)`
- `attachments(file_hash)` — 実体の重複排除の判定

## 4. 接続時のPRAGMA

- `journal_mode = WAL` — 永続設定。書き込みは別ファイルに追記され、読み手は書き込み中でも
  止まらずに読める。GUI・CLI・MCPサーバーが**別プロセスとして同じDBファイルを触る**前提の
  ため必須
- `foreign_keys = ON` — SQLiteは既定でOFF。接続ごとに毎回設定する
- `busy_timeout` を設定し、書き込みトランザクションは `BEGIN IMMEDIATE` で開始する
  (複数プロセスからの同時書き込みを直列化する排他制御の実体)

**この節の排他制御は未確定**(Issue #74)。現状の実装はWALと`busy_timeout`の設定までで、
`BEGIN IMMEDIATE`はまだ使っていない。プロセス内の直列化は接続を包む`Mutex`が担っており、
プロセスを跨いだ「読んで判断してから書く」操作の直列化は保証されていない。方式と、
トランザクションの境界をどこに引くかは#74で決め、決まったらこの節を書き直す。

## 5. マイグレーション

`rusqlite_migration` を用い、`migrations/0001_init.sql` から番号順で管理する
(`../legacy/data-model.md` §7が明示的に禁止した「列の有無から推測する」方式は採らない)。

**CHECK制約はCREATE TABLE時点で入れる**: SQLiteは `ALTER TABLE ADD CONSTRAINT` を
持たず、後から制約を追加するにはテーブル再構築(新テーブル作成→全行コピー→差し替え)が
必要になる。上記の制約はすべて `migrations/0001_init.sql` の `CREATE TABLE` に含める。
0001より後に加わった不変条件は、再構築の代わりに `BEFORE INSERT`/`BEFORE UPDATE` の
トリガーで同じ条件を表す(例: `0002_tool_execution_role.sql`)。再構築は外部キーの一時
無効化を伴い、マイグレーションのトランザクション内では行えないため。

## 6. DBと設定ファイルの境界、秘密情報

`../legacy/data-model.md` 3節・4節の方針(システムプロンプト等はDBでなく設定ファイルに置く、
秘密情報はOS資格情報ストアに置き設定ファイルには参照のみ)は変更なく維持する。
実装は `architecture.md` 6節を参照。
