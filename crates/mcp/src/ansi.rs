//! ANSI 控制序列剥离：把终端字节流还原成给人看的纯文本。
//!
//! 只处理常见的三类序列（CSI、OSC、两字节转义），不追求完整的 VT 规范；
//! 目的是让 MCP 工具把「带颜色的输出」直接交给模型时不再夹着控制字符。

/// 剥离状态机的内部状态。
///
/// 无参数
/// 核心流程：按 ESC 之后读到的字节切换，命中终止条件再回到普通文本。
enum State {
    /// 普通文本。
    Ground,
    /// 刚读到 ESC，等下一个字符决定序列类型。
    Escape,
    /// CSI 序列内部，等终止字节。
    Csi,
    /// OSC 序列内部，等 BEL 或 ESC 反斜杠。
    Osc,
    /// OSC 里读到了 ESC，等确认是不是反斜杠。
    OscEscape,
}

/// 去掉 ANSI 控制序列，并按终端语义合并回车覆盖。
///
/// 参数：input 为原始终端文本（通常由字节流按 UTF-8 lossy 得到）
/// 核心流程：先逐字符扫描剥掉转义序列（CSI 丢弃到终止字节、OSC 丢弃到 BEL 或 ST、
/// 两字节转义丢弃 ESC 加一个字符），此时 `\r` 原样保留以防丢失覆盖语义；
/// 再按 `\n` 切行、每行按 `\r` 切段只保留最后一段，还原终端上「回车回到行首重新覆盖」后的可见文本。
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut state = State::Ground;

    for ch in input.chars() {
        match state {
            State::Ground => match ch {
                '\x1b' => state = State::Escape,
                // \r 先原样留下，等剥完转义序列再统一做覆盖合并
                other => output.push(other),
            },
            State::Escape => match ch {
                '[' => state = State::Csi,
                ']' => state = State::Osc,
                // 两字节转义：ESC 加单个字符，整段丢弃
                _ => state = State::Ground,
            },
            State::Csi => {
                // 参数与中间字节是 0x20..=0x3F，终止字节是 0x40..=0x7E
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    state = State::Ground;
                }
            }
            State::Osc => match ch {
                '\x07' => state = State::Ground,
                '\x1b' => state = State::OscEscape,
                _ => {}
            },
            State::OscEscape => {
                // ESC \ 是 OSC 的标准终止符；其它情况按仍在 OSC 内继续吞
                state = if ch == '\\' {
                    State::Ground
                } else {
                    State::Osc
                };
            }
        }
    }

    collapse_carriage_returns(&output)
}

/// 合并回车覆盖，得到终端上真正看得见的一行行文本。
///
/// 参数：text 为已剥掉转义序列的文本，可能夹着 `\r`
/// 核心流程：按 `\n` 切行、按 `\r` 切段；一行内后面的段落到行首重新覆盖，
/// 因此只保留最后一段，最后一段为空时回退到最后一个非空段（`\r\n` 行尾就属于这种情形）；
/// 行尾的 `\n` 原样保留，`\r\n` 已在切行时分开，不会被当成两次覆盖。
fn collapse_carriage_returns(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(visible_segment(line));
    }
    output
}

/// 取一行里 `\r` 覆盖后仍可见的那一段。
///
/// 参数：line 为不含 `\n` 的单行文本
/// 核心流程：顺序扫描以 `\r` 分割的段，记录最后一段与最后一个非空段；
/// 最后一段为空说明光标回到行首后没再写字，此时取最后一个非空段。
fn visible_segment(line: &str) -> &str {
    let mut last = "";
    let mut last_non_empty = "";
    for segment in line.split('\r') {
        last = segment;
        if !segment.is_empty() {
            last_non_empty = segment;
        }
    }
    if last.is_empty() {
        last_non_empty
    } else {
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CSI 颜色码应整段消失，正文保留。
    #[test]
    fn strips_csi_color_codes() {
        let text = strip_ansi("\x1b[31mRED\x1b[0m plain");
        assert_eq!(text, "RED plain");
    }

    /// OSC 标题以 BEL 或 ESC 反斜杠收尾，都不该漏进正文。
    #[test]
    fn strips_osc_title() {
        assert_eq!(strip_ansi("\x1b]0;my title\x07done"), "done");
        assert_eq!(strip_ansi("\x1b]0;my title\x1b\\done"), "done");
    }

    /// \r 回到行首覆盖，只留覆盖后的可见文本。
    #[test]
    fn collapses_cr_overwrite() {
        assert_eq!(strip_ansi("abc\rXYZ"), "XYZ");
        assert_eq!(strip_ansi("abcdef\rXY"), "XY");
    }

    /// \r\n 是行结束，不能被当成一次覆盖而把上一行吃掉。
    #[test]
    fn keeps_crlf_as_line_break() {
        assert_eq!(strip_ansi("a\r\nb"), "a\nb");
        assert_eq!(strip_ansi("a\r\nb\r\n"), "a\nb\n");
    }

    /// 每一行都以 \r\n 收尾时行数与行序都不变。
    #[test]
    fn keeps_every_line_ending_with_crlf() {
        let text = strip_ansi("l1\r\nWRITE-WRITE-WRITE\r\n");
        assert_eq!(text, "l1\nWRITE-WRITE-WRITE\n");
        assert_eq!(text.lines().count(), 2);
    }

    /// 颜色码与回车覆盖混在一起时，既不残留控制序列也不残留覆盖中间态。
    #[test]
    fn strips_ansi_before_collapsing_cr() {
        let text = "\x1b[32m$ \x1b[0mWri\r\x1b[32m$ \x1b[0mWrite-Output 'MARK'";
        assert_eq!(strip_ansi(text), "$ Write-Output 'MARK'");
        assert!(!strip_ansi(text).contains('\x1b'));
    }

    /// 纯文本原样返回，不增不减。
    #[test]
    fn keeps_plain_text_untouched() {
        let text = "hello 世界\nsecond line";
        assert_eq!(strip_ansi(text), text);
    }
}
