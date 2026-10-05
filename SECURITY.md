# セキュリティ上の問題の報告

脆弱性を見つけた場合は、公開のIssueではなく、GitHubの非公開の報告機能から知らせてください。
リポジトリの「Security」タブ →「Report a vulnerability」から送れます。内容はメンテナにだけ
見え、修正を公開するまで伏せたまま扱います。

To report a vulnerability, please use GitHub's private vulnerability reporting
("Security" tab → "Report a vulnerability") instead of opening a public issue.

## 対象

- このリポジトリのコード(`crates/`・`frontend/`)と、そこからビルドしたアプリ
- 最新の`develop`ブランチと、最新のリリース

利用者が登録したLLMプロバイダー・外部ツールサーバーの振る舞いそのものは対象外です
(登録した先を信頼することを前提にしています。`docs/spec/principles.md`「セキュリティ方針」)。
