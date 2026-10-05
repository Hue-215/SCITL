//! 端末への出力。CLIはここの関数だけから端末へ書く。出力はJSONに揃え、見えない文字を
//! JSONのエスケープの形にしてから書く(`docs/spec/architecture/sanitize.md`「無害化」の端末の行)。

use std::fmt::Display;
use std::io::{ErrorKind, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// 標準出力に書けなくなったか。書けなくなったら以降の出力は捨てる。
static STDOUT_GONE: AtomicBool = AtomicBool::new(false);
/// 標準出力への書き込みが、読み手がパイプを閉じた以外の理由で失敗したか([`finish`]が失敗にする)。
static STDOUT_FAILED: AtomicBool = AtomicBool::new(false);

/// 標準出力へ書く。書けなくても、panic(`println!`)もプロセスの終了もせず、以降の出力を捨てて
/// 処理は最後まで進める。scitl-debug-cliの途中経過([`print_json_line`])はターンの途中で書くので、
/// ここで終えるとターンが半端に終わる。
///
/// 読み手がパイプを閉じた(`| head`等)のは、読み手が必要な分を読み終えたということなので、
/// 失敗にしない。それ以外の失敗は、標準エラーに書いて失敗で終える。
fn write_stdout(s: &str) {
    if STDOUT_GONE.load(Ordering::Relaxed) {
        return;
    }
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        STDOUT_GONE.store(true, Ordering::Relaxed);
        if e.kind() != ErrorKind::BrokenPipe {
            STDOUT_FAILED.store(true, Ordering::Relaxed);
            print_error(&format!("could not write the output: {e}"));
        }
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
        Ok(()) if STDOUT_FAILED.load(Ordering::Relaxed) => ExitCode::FAILURE,
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

/// エラーの表示文はタスクのタイトル等の値を含みうるので、出力と同じく見えない文字を見せる形にする。
fn print_error(error: &impl Display) {
    write_stderr(&format!(
        "error: {}\n",
        text::reveal_invisible(&error.to_string())
    ));
}

/// 標準エラーへ書く。書けなければ捨てる(`eprintln!`はpanicする。知らせる先も無い)。
fn write_stderr(s: &str) {
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

/// clapが組み立てたヘルプ・引数のエラー。エラーは受け取った引数の値をそのまま含むので、
/// 他の出力と同じく見えない文字を見せる形にする。
fn print_clap(error: &clap::Error) -> ExitCode {
    let rendered = text::reveal_invisible(&error.render().to_string());
    if error.use_stderr() {
        write_stderr(&rendered);
    } else {
        write_stdout(&rendered);
    }
    ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
}
