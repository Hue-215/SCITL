//! 出力先を知らない、文字単位の部品。どの文字列に何を掛けるかは出力先ごとに決まり
//! (docs/spec/rebuild/architecture.md 10節)、ここはその判断に使う部品だけを持つ。

/// 表示の順序を入れ替える双方向制御文字。
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// `char::is_control`(Cc)が拾わない書式文字(Cf)のうち、見えないまま文字列に紛れて
/// 見た目を惑わすもの(双方向制御文字・ゼロ幅文字・タグ文字等)。タグ文字(U+E0000〜)は
/// ASCIIを見えない形で写せるため、画面に見えない指示を紛れ込ませる手口に使われる。
/// 標準ライブラリに一般カテゴリの判定が無いため、該当する範囲を列挙する。
pub fn is_invisible_format(c: char) -> bool {
    is_bidi_control(c)
        || matches!(
            c,
            '\u{00AD}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200D}'
                | '\u{2060}'..='\u{2064}'
                | '\u{206A}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// 制御文字(改行を含む)を空白に畳み、連続空白を1つにまとめ、前後の空白を落とす。
pub fn collapse_whitespace(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 先頭から`max`文字までを返す。2つ目の値は切り詰めたかどうか。省略の印は出力先ごとに
/// 違うため(architecture.md 10節)、付けるのは呼び出し側に任せる。
pub fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    let truncated = chars.next().is_some();
    (head, truncated)
}

/// 画面に出す文字列を`max`文字で切り詰め、切った場合は「…」を付ける。
pub fn ellipsize(s: &str, max: usize) -> String {
    match truncate_chars(s, max) {
        (head, true) => format!("{head}…"),
        (head, false) => head,
    }
}

/// 不可視の書式文字を除き、制御文字を空白にして1行に畳む。[`display_label`]の切り詰め前の
/// 段階で、切り詰めの前に別の処理(秘密情報の伏せ字等)を挟む呼び出し側が使う。
pub fn visible_line(s: &str) -> String {
    let visible: String = s.chars().filter(|&c| !is_invisible_format(c)).collect();
    collapse_whitespace(&visible)
}

/// 外部から来た文字列を、画面に1行で出す形にする(ツール名・エラー文言等)。
pub fn display_label(s: &str, max: usize) -> String {
    ellipsize(&visible_line(s), max)
}

/// 外部から来た文字列を、改行を保ったまま画面に出す形にする(ツールの説明・stderr等)。
/// [`display_label`]との違いは、改行・タブを残し、空白を畳まないことだけ。
pub fn display_block(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|&c| !is_invisible_format(c))
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    ellipsize(&cleaned, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_removes_bidi_and_zero_width_and_blanks_controls() {
        assert_eq!(display_label("ab\u{202E}cd\u{200B}e", 100), "abcde");
        assert_eq!(display_label("a\n\tb\u{7}c", 100), "a b c");
        assert_eq!(display_label("  a   b  ", 100), "a b");
    }

    #[test]
    fn label_removes_tag_characters() {
        // タグ文字で「hi」を写したもの。画面には見えないまま残らないこと。
        assert_eq!(
            display_label("read\u{E0068}\u{E0069}_file", 100),
            "read_file"
        );
    }

    #[test]
    fn label_marks_truncation_with_ellipsis() {
        assert_eq!(display_label("abcdef", 3), "abc…");
        assert_eq!(display_label("abc", 3), "abc");
    }

    #[test]
    fn block_keeps_newlines_and_tabs() {
        assert_eq!(
            display_block("line1\n\tline2\u{1b}[31m\u{2066}x", 100),
            "line1\n\tline2 [31mx"
        );
    }

    #[test]
    fn c1_controls_are_controls() {
        // CSI(U+009B)は端末で制御シーケンスの開始になる。Ccとして除かれること。
        assert_eq!(display_label("a\u{9B}b", 100), "a b");
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(
            truncate_chars("日本語です", 3),
            ("日本語".to_string(), true)
        );
        assert_eq!(truncate_chars("日本語", 3), ("日本語".to_string(), false));
    }
}
