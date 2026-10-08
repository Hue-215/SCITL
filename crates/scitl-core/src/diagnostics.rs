//! 利用者に返さない診断の出口。失敗にせず先へ進めた処理が、何を諦めたかをここへ書く。
//! 標準エラー(Androidではlogcat)へはcoreのどこからもここを通して書く。

use std::fmt::Display;

use crate::text;

/// 診断を1件書く。書き先は標準エラーで、Androidではアプリの標準エラーがどこにも出ないので
/// logcatにする。どちらも端末で読まれるので、端末への出力と同じく見えない文字を見せる形にする
/// (`docs/spec/architecture/sanitize.md`「無害化」の端末・logcatの行)。
pub fn report(message: impl Display) {
    write_line(&for_terminal(message));
}

fn for_terminal(message: impl Display) -> String {
    text::reveal_invisible(&message.to_string())
}

#[cfg(not(target_os = "android"))]
#[allow(clippy::print_stderr)]
fn write_line(line: &str) {
    eprintln!("{line}");
}

#[cfg(target_os = "android")]
fn write_line(line: &str) {
    logcat::write(line);
}

// ---- logcat ----

/// NDKの`liblog`を直に呼んで、logcatへ1件書く。
#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{c_char, c_int, CStr, CString};

    /// `adb logcat -s SCITL`で絞り込むときの名前。
    const TAG: &CStr = c"SCITL";
    /// `android/log.h`の`ANDROID_LOG_WARN`。
    const ANDROID_LOG_WARN: c_int = 5;
    /// 1件の本文の長さ(バイト)。liblogは1件をおよそ4KBで黙って切るので、それより短く分けて書く。
    const MAX_ENTRY_BYTES: usize = 4000;

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub(super) fn write(line: &str) {
        for piece in super::split_at_char_boundaries(line, MAX_ENTRY_BYTES) {
            // NULは`text::reveal_invisible`がエスケープ済みなので失敗しないが、書けなくても先へ進む。
            let Ok(text) = CString::new(piece) else {
                return;
            };
            // SAFETY: 渡すのはどちらもNUL終端の文字列で、呼び出しの間だけ生きていればよい。
            unsafe {
                __android_log_write(ANDROID_LOG_WARN, TAG.as_ptr(), text.as_ptr());
            }
        }
    }
}

/// `line`を、文字の途中で切らずに`max_bytes`バイト以下の区間に分ける。空の`line`は空の区間1つになる。
#[cfg(any(target_os = "android", test))]
fn split_at_char_boundaries(line: &str, max_bytes: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = line;
    while rest.len() > max_bytes {
        let mut end = max_bytes;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        pieces.push(piece);
        rest = tail;
    }
    pieces.push(rest);
    pieces
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

    #[test]
    fn long_lines_are_split_without_breaking_characters() {
        assert_eq!(split_at_char_boundaries("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(split_at_char_boundaries("aあい", 4), ["aあ", "い"]);
        assert_eq!(split_at_char_boundaries("", 4), [""]);
    }

    #[test]
    fn nul_is_escaped_so_the_line_fits_a_c_string() {
        assert_eq!(for_terminal("a\0b"), "a\\u0000b");
    }
}
