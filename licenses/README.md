# 配布物の第三者ライセンスに足すファイル

`scripts/assemble-dist.mjs`が、配布物の`THIRD-PARTY-LICENSES/rust.txt`を組み立てるときに使う。
どのクレートにどのフォルダを使うかは、スクリプトの`SUPPLIED`・`BUNDLED`にある。

- **SUPPLIED**: crates.ioのパッケージにライセンスファイルが入っていないクレートの分。上流の
  リポジトリから写した。パッケージにファイルが入るようになったら、スクリプトが止まるので消す
- **BUNDLED**: クレートのライセンスとは別に、そのクレートが実行ファイルに入れる第三者のもの
- **STD**: Rustの標準ライブラリと、それと一緒に実行ファイルに入るクレートのうち、依存の一覧に
  現れないもの(cargo-aboutは標準ライブラリの中を見ない)

| フォルダ | 対象 | 入手先 |
|---|---|---|
| `rmcp` | rmcp | https://github.com/modelcontextprotocol/rust-sdk のタグ`rmcp-v3.4.0` |
| `alloc-stdlib` | alloc-stdlib | https://github.com/dropbox/rust-alloc-no-stdlib のタグ`0.2.4` |
| `dlopen2` | dlopen2・dlopen2_derive | https://github.com/OpenByteDev/dlopen2 のコミット`c1ca060`(版のタグが無い) |
| `unic` | unic-*(5クレート) | https://github.com/open-i18n/rust-unic のタグ`v0.9.0` |
| `webview2-com` | webview2-com・webview2-com-sys・webview2-com-macros | https://github.com/wravery/webview2-rs のコミット`edc2caf`(版のタグが無い) |
| `webview2-sdk` | webview2-com-sysが静的にリンクするWebView2のローダー(BUNDLED) | NuGetの`Microsoft.Web.WebView2` 1.0.3800.47の`LICENSE.txt`・`NOTICE.txt` |
| `rust` | Rustの標準ライブラリ(STD) | https://github.com/rust-lang/rust のタグ`1.93.1`の`COPYRIGHT`・`LICENSE-MIT`・`LICENSE-APACHE` |
| `addr2line` | 標準ライブラリが使うaddr2line(STD) | https://github.com/gimli-rs/addr2line のタグ`0.25.1` |
| `gimli` | 標準ライブラリが使うgimli(STD) | https://github.com/gimli-rs/gimli のタグ`0.32.3` |
| `rustc-demangle` | 標準ライブラリが使うrustc-demangle(STD) | https://github.com/rust-lang/rustc-demangle のタグ`rustc-demangle-v0.1.26` |
| `object` | 標準ライブラリが使うobject(STD) | https://github.com/gimli-rs/object のタグ`0.37.3` |
| `compiler_builtins` | 標準ライブラリが使うcompiler_builtins(STD) | https://github.com/rust-lang/rust のタグ`1.93.1`の`library/compiler-builtins/LICENSE.txt` |

STDの版は、Linuxの配布物を作ったRust 1.93.1のもの。標準ライブラリが使うクレートは、その版の
https://github.com/rust-lang/rust の`library/Cargo.lock`で確かめる(配布の対象のOS向けに実行ファイルへ
入るもののうち、`rust.txt`の一覧に無いものを写す)。Rustを更新したら見直す。

ファイルは手で書き換えない。版が上がって上流のファイルが変わったら、写し直して入手先を更新する。
