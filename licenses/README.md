# 配布物の第三者ライセンスに足すファイル

`scripts/assemble-dist.mjs`が、配布物の`THIRD-PARTY-LICENSES/rust.txt`を組み立てるときに使う。
どのクレートにどのフォルダを使うかは、スクリプトの`SUPPLIED`・`BUNDLED`にある。

- **SUPPLIED**: crates.ioのパッケージにライセンスファイルが入っていないクレートの分。上流の
  リポジトリから写した。パッケージにファイルが入るようになったら、スクリプトが止まるので消す
- **BUNDLED**: クレートのライセンスとは別に、そのクレートが実行ファイルに入れる第三者のもの

| フォルダ | 対象 | 入手先 |
|---|---|---|
| `rmcp` | rmcp | https://github.com/modelcontextprotocol/rust-sdk のタグ`rmcp-v3.4.0` |
| `alloc-stdlib` | alloc-stdlib | https://github.com/dropbox/rust-alloc-no-stdlib のタグ`0.2.4` |
| `dlopen2` | dlopen2・dlopen2_derive | https://github.com/OpenByteDev/dlopen2 のコミット`c1ca060`(版のタグが無い) |
| `unic` | unic-*(5クレート) | https://github.com/open-i18n/rust-unic のタグ`v0.9.0` |
| `webview2-com` | webview2-com・webview2-com-sys・webview2-com-macros | https://github.com/wravery/webview2-rs のコミット`edc2caf`(版のタグが無い) |
| `webview2-sdk` | webview2-com-sysが静的にリンクするWebView2のローダー(BUNDLED) | NuGetの`Microsoft.Web.WebView2` 1.0.3800.47の`LICENSE.txt`・`NOTICE.txt` |

ファイルは手で書き換えない。版が上がって上流のファイルが変わったら、写し直して入手先を更新する。
