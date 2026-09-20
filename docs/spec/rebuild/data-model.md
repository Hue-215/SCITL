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
  自動命名の判定は `title IS NULL`
- 主キーは `INTEGER PRIMARY KEY`(SQLiteのrowidエイリアス)

## 2. テーブル

### tasks

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| title | TEXT | NULL可。NULL=未設定・自動命名の対象 |
| description | TEXT | NULL可 |
| deadline | TEXT | `YYYY-MM-DD`。NULL可 |
| archived_at | TEXT | ISO8601。NULL=未アーカイブ |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| created_at | TEXT | ISO8601。NOT NULL |
| updated_at | TEXT | ISO8601。NOT NULL |

優先度カラムは持たない(`../principles.md` 2節)。

### task_steps

| カラム | 型 | 制約・備考 |
|---|---|---|
| id | INTEGER | PRIMARY KEY |
| task_id | INTEGER | NOT NULL, `REFERENCES tasks(id)` |
| description | TEXT | NOT NULL |
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
| role | TEXT | NOT NULL, `CHECK (role IN ('user','assistant','tool'))` |
| content | TEXT | NOT NULL |
| kind | TEXT | NOT NULL, `CHECK (kind IN ('normal','tool_execution'))` |
| source | TEXT | NULL=内部。外部(MCP)経由には印を付ける |
| reasoning | TEXT | NULL可。表示・エクスポート専用、APIには送らない |
| is_error | INTEGER | NOT NULL DEFAULT 0(bool)。単一メッセージの結果を表すfactであり、 lifecycle状態(いつ起きたか)ではないためタイムスタンプ化は不要 |
| turn_id | TEXT | NULL可(下記「ターン境界」参照) |
| attempt_no | INTEGER | NULL可 |
| deleted_at | TEXT | ISO8601。NULL=未削除 |
| created_at | TEXT | ISO8601。NOT NULL |

`CHECK (kind <> 'tool_execution' OR json_valid(content))` — ツール実行記録の `content` は
構造化データ(JSON)であることを制約で保証する。

**`role='tool'` について**(Issue #11): ツール実行結果のうち「現在の状態」として
言い表せないもの(検索結果・外部MCPツールの出力等)は、`role='tool'` として会話履歴にも
残す。タスク・工程の更新のように**次のターン以降は**最新状態JSONで代替できるものは、
実行記録としては残すが会話履歴には投入しない。同一ターン内のツール呼び出しループでは
分類によらず結果をモデルに返すが、その返却分はリクエストの組み立てにのみ使い、
このテーブルの行としては保存しない(`tools.md` 4節)。分類はツール定義側の属性として持たせ、
このテーブル構造自体には分類ロジックを持たせない(詳細は `tools.md`)。

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

## 5. マイグレーション

`rusqlite_migration` を用い、`migrations/0001_init.sql` から番号順で管理する
(`../legacy/data-model.md` §7が明示的に禁止した「列の有無から推測する」方式は採らない)。

**CHECK制約はCREATE TABLE時点で入れる**: SQLiteは `ALTER TABLE ADD CONSTRAINT` を
持たず、後から制約を追加するにはテーブル再構築(新テーブル作成→全行コピー→差し替え)が
必要になる。上記の制約はすべて `migrations/0001_init.sql` の `CREATE TABLE` に含める。

## 6. DBと設定ファイルの境界、秘密情報

`../legacy/data-model.md` 3節・4節の方針(システムプロンプト等はDBでなく設定ファイルに置く、
秘密情報はOS資格情報ストアに置き設定ファイルには参照のみ)は変更なく維持する。
実装は `architecture.md` 6節を参照。
