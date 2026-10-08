//! 利用者に返さない診断の出口。失敗にせず先へ進めた処理が、何を諦めたかをここへ書く。
//! 標準エラー(Androidではlogcat)へはcoreのどこからもここを通して書く。

use std::fmt::Display;

use crate::text;

/// 診断を1行書く。書き先は標準エラーで、Androidではアプリの標準エラーがどこにも出ないので
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

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub(super) fn write(line: &str) {
        // NULは`text::reveal_invisible`がエスケープ済みなので失敗しないが、書けなくても先へ進む。
        let Ok(text) = CString::new(line) else {
            return;
        };
        // SAFETY: 渡すのはどちらもNUL終端の文字列で、呼び出しの間だけ生きていればよい。
        unsafe {
            __android_log_write(ANDROID_LOG_WARN, TAG.as_ptr(), text.as_ptr());
        }
    }
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
    fn nul_is_escaped_so_the_line_fits_a_c_string() {
        assert_eq!(for_terminal("a\0b"), "a\\u0000b");
    }
}
