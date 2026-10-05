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
| 多重起動の防止 | `tauri-plugin-single-instance` | 公式プラグインで、通信はローカルのIPCだけ。自前で持つとOSごとのIPCを2通り書くことになり、攻撃面も保守量も増える(`concurrency.md`「多重起動の防止」) |

トークン数の見積もりは、現状は文字数からのフォールバック(`llm::token_estimate`)だけを持つ
(Issue #66)。トークナイザはモデルごとに違い、クレートを足しても登録されたモデルに合う保証が
無い。推論サーバーの数え上げのエンドポイントはサーバーごとに方言があり、OpenAI互換APIには無い。
どちらも実行時依存か通信の追加に当たるわりに、多めに見積もったときの損は古い発言が早めに
落ちるだけなので、見合わない。見積もった値は履歴の間引き(`orchestration::history_trim`)が
コンテキスト長(`llm-adapter.md`の能力)と比べて使う。

## ライセンス

- **MPL-2.0は許容する**(ファイル単位の弱いコピーレフトで、SCITL自身のコードのライセンスを縛らない)。
  実行バイナリに入るのは`option-ext`(`dirs`経由。Tauri自身も使う)だけで、`selectors`・`cssparser`・
  `dtoa-short`はビルド時のみ。npmのMPL-2.0は開発依存の`lightningcss`だけ。GPL・LGPL・AGPLは無い。
  同梱フォントはSIL OFL 1.1(同梱は可、フォント自体はOFLのまま)。ライセンスはMIT予定なので、
  コピーレフトのライブラリは使わない
- **Windowsの実行ファイルには、MicrosoftのWebView2 SDKのローダーが入る**(`webview2-com-sys`が
  `WebView2LoaderStatic.lib`を静的にリンクする)。SDKのライセンスはBSD-3-Clauseと同じ形の条項で、
  バイナリでの配布にも著作権表示とライセンス文の同梱を求める。クレートのライセンス(MIT)とは別に、
  配布物の第三者ライセンスの一覧に載せる(`licenses/README.md`)
