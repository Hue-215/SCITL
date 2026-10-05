//! 端末への出力。CLIはここの関数だけから端末へ書く。出力はJSONに揃え、見えない文字を
//! JSONのエスケープの形にしてから書く(`docs/spec/architecture/sanitize.md`「無害化」の端末の行)。

use std::fmt::Display;
use std::io::{ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

use scitl_core::text;

/// 値を、人が読める字下げ付きのJSONで書く。
pub fn print_json(value: &impl Serialize) {
    let json = serde_json::to_string_pretty(value).expect("views serialize to JSON");
    write_stdout(&format!("{}\n", text::reveal_invisible(&json)));
}

/// 値を1行のJSONで書く。処理の途中経過のように、値を続けて書く出力に使う。
pub fn print_json_line(value: &impl Serialize) {
    let json = serde_json::to_string(value).expect("views serialize to JSON");
    write_stdout(&format!("{}\n", text::reveal_invisible(&json)));
}

/// 標準出力へ書く。書けなければ、その場でプロセスを終える(`println!`はpanicする)。
/// 読み手がパイプを閉じた(`| head`等)のは、読み手が必要な分を読み終えたということなので、
/// 何も言わずに成功で終える。それ以外の失敗は、標準エラーに書いて失敗で終える。
///
/// scitl-cliの出力は操作を終えた後に書くので、ここで終えても操作は済んでいる。scitl-debug-cliの
/// 途中経過([`print_json_line`])では、ターンの途中で終わる。
fn write_stdout(s: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        if e.kind() == ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        print_error(&format!("could not write the output: {e}"));
        std::process::exit(1);
    }
}

/// 引数を解析する。ヘルプ・引数のエラーはここで端末へ書き、終了コードを返す
/// (`Parser::parse`に任せると、clapが見えない文字を含む引数の値をそのまま端末へ書く)。
pub fn parse<C: Parser>() -> Result<C, ExitCode> {
    C::try_parse().map_err(|e| print_clap(&e))
}

/// コマンドの結果を終了コードにする。失敗は標準エラーへ書く。
pub fn finish<E: Display>(result: Result<(), E>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

/// エラーの表示文はタスクのタイトル等の値を含みうるので、出力と同じく見えない文字を見せる形にする。
fn print_error(error: &impl Display) {
    eprintln!("error: {}", text::reveal_invisible(&error.to_string()));
}

/// clapが組み立てたヘルプ・引数のエラー。エラーは受け取った引数の値をそのまま含むので、
/// 他の出力と同じく見えない文字を見せる形にする。
fn print_clap(error: &clap::Error) -> ExitCode {
    let rendered = text::reveal_invisible(&error.render().to_string());
    if error.use_stderr() {
        eprint!("{rendered}");
    } else {
        write_stdout(&rendered);
    }
    ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
}
