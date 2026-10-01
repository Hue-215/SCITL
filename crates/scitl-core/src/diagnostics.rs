//! 利用者に返さない診断の出口。失敗にせず先へ進めた処理が、何を諦めたかをここへ書く。
//! 標準エラーへはcoreのどこからもここを通して書く。

use std::fmt::Display;

use crate::text;

/// 診断を標準エラーへ1行書く。標準エラーはCLIでは端末なので、端末への出力と同じく見えない
/// 文字を見せる形にする(`docs/spec/architecture/sanitize.md`「無害化」の端末の行)。
#[allow(clippy::print_stderr)]
pub fn report(message: impl Display) {
    eprintln!("{}", for_terminal(message));
}

fn for_terminal(message: impl Display) -> String {
    text::reveal_invisible(&message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_characters_are_written_escaped() {
        assert_eq!(
            for_terminal(format_args!("failed: {}", "a\u{1B}[31m\u{202E}b")),
            "failed: a\\u001B[31m\\u202Eb"
        );
    }
}
