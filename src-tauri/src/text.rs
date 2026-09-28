//! 解码 yt-dlp 子进程的输出。
//!
//! ## 为什么不能直接当 UTF-8
//!
//! `HANDOFF.md` §3.7 建议「设 `PYTHONIOENCODING=utf-8` 让 yt-dlp 输出 UTF-8」。
//! **实测这条不成立**：yt-dlp 用 `preferredencoding()`
//! （即 `locale.getpreferredencoding()`，本机是 cp936/GBK）编码控制台输出，
//! 主动绕过了 `PYTHONIOENCODING`。
//!
//! 实测证据：设了 `PYTHONIOENCODING=utf-8` 之后，stderr 里 `you’re` 的 `’`
//! 仍然是字节 `A1 AF`（GBK 的 U+2019），按 UTF-8 读就成了 `�`。
//!
//! ## 好消息：关键路径不受影响
//!
//! - `--dump-single-json` 走 stdout，且把非 ASCII **转义成 `\uXXXX`**，
//!   整段是纯 ASCII，`serde_json` 能正确还原（实测中文标题往返无损）
//! - `--print-to-file` 写文件时用的是**显式 UTF-8**（`YoutubeDL.py:3255`）
//!
//! 受影响的只有人类可读的报错与日志文本——但那正是用户要看的。

/// 按「先 UTF-8、失败再按系统代码页」解码。
///
/// 顺序很重要：UTF-8 是严格校验的，合法的 GBK 字节序列几乎不可能是合法 UTF-8，
/// 所以先试 UTF-8 不会误判；反过来先试 GBK 则会把正常的中文 UTF-8 解成乱码。
pub fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // 用 GBK 而非 latin1 之类的兜底：本机（以及所有中文 Windows）
        // 的 `getpreferredencoding()` 就是 cp936。
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

/// 专门给「按行读取」用的版本。
pub fn decode_line(bytes: &[u8]) -> String {
    decode_console(bytes).trim_end_matches(['\r', '\n']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_ascii_and_utf8() {
        assert_eq!(decode_console(b"hello"), "hello");
        // 合法 UTF-8 的中文必须原样保留（不能被当成 GBK 解坏）
        let zh = "中文标题".as_bytes();
        assert_eq!(decode_console(zh), "中文标题");
        // emoji 是 4 字节 UTF-8，也必须是合法输入
        assert_eq!(decode_console("🎬".as_bytes()), "🎬");
    }

    /// 实测原文：yt-dlp 把 `’` 写成 GBK 的 `A1 AF`。
    #[test]
    fn decodes_real_gbk_bytes_from_ytdlp() {
        // "you" + GBK(’) + "re not a bot"
        let mut buf = b"you".to_vec();
        buf.extend_from_slice(&[0xA1, 0xAF]);
        buf.extend_from_slice(b"re not a bot");
        assert_eq!(decode_console(&buf), "you’re not a bot");
        assert!(!decode_console(&buf).contains('\u{FFFD}'));
    }

    /// 一段完整的 GBK 中文报错。
    #[test]
    fn decodes_gbk_chinese() {
        let gbk = encoding_rs::GBK.encode("视频不存在").0;
        assert_eq!(decode_console(&gbk), "视频不存在");
    }

    /// 截断的 UTF-8（分片读取时可能出现）也要能解码而不是 panic。
    #[test]
    fn truncated_utf8_does_not_panic() {
        let zh = "中文".as_bytes();
        let out = decode_console(&zh[..3]);
        assert!(!out.is_empty());
    }

    #[test]
    fn empty_input() {
        assert_eq!(decode_console(b""), "");
        assert_eq!(decode_line(b"\r\n"), "");
    }

    #[test]
    fn decode_line_strips_newlines() {
        assert_eq!(decode_line(b"abc\r\n"), "abc");
        assert_eq!(decode_line(b"abc\n"), "abc");
    }
}
