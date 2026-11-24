//! Turning a child's raw output bytes into clean log lines.
//!
//! Same rules as v1 `desktop/src/main/proc.js` (`start` → `pump`):
//! split on `\n`, `\r\n` *and* a bare `\r` (progress bars rewrite one line
//! with `\r`; every rewrite is a line), strip ANSI colours, trim the end, drop
//! empty lines. Bytes are decoded as lossy UTF-8 so a stray Windows code-page
//! byte never stops the stream.

/// Longest line we buffer before flushing it as-is (a child that prints
/// megabytes without a newline must not grow memory without bound).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Remove ANSI escape sequences (CSI `ESC [ … final`, OSC `ESC ] … BEL|ST`,
/// and two/three-byte `ESC x` forms) and other control characters except tab.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // Parameters and intermediates, then one final byte 0x40..=0x7E.
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{07}' {
                            break;
                        }
                        if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    // ESC, optional intermediates 0x20..=0x2F, then a final byte.
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if !('\u{20}'..='\u{2f}').contains(&n) {
                            break;
                        }
                    }
                }
                None => {}
            },
            // Single-byte CSI (C1) form.
            '\u{9b}' => {
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Clean one raw line: lossy UTF-8, ANSI stripped, end trimmed. `None` if empty.
pub fn clean_line(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let line = strip_ansi(&text);
    let line = line.trim_end();
    if line.trim().is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Incremental splitter: feed byte chunks, get complete clean lines.
#[derive(Debug, Default)]
pub struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<String>) {
        for &b in chunk {
            if b == b'\n' || b == b'\r' {
                if let Some(line) = clean_line(&self.pending) {
                    out.push(line);
                }
                self.pending.clear();
            } else {
                self.pending.push(b);
                if self.pending.len() >= MAX_LINE_BYTES {
                    if let Some(line) = clean_line(&self.pending) {
                        out.push(line);
                    }
                    self.pending.clear();
                }
            }
        }
    }

    /// End of stream: whatever is left, if anything.
    pub fn finish(&mut self) -> Option<String> {
        let line = clean_line(&self.pending);
        self.pending.clear();
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colours_and_osc() {
        assert_eq!(
            strip_ansi("\u{1b}[32m  detected RTX 4090\u{1b}[0m"),
            "  detected RTX 4090"
        );
        assert_eq!(strip_ansi("\u{1b}[1;38;5;208mX\u{1b}[K"), "X");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}ok"), "ok");
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(strip_ansi("a\u{1b}(Bb"), "ab");
        assert_eq!(strip_ansi("tab\there"), "tab\there");
    }

    #[test]
    fn splits_on_all_line_ends() {
        let mut s = LineSplitter::new();
        let mut out = vec![];
        s.feed(b"one\r\ntwo\n 10%\r 50%\r100%\n\n", &mut out);
        s.feed(b"par", &mut out);
        s.feed(b"tial", &mut out);
        assert_eq!(out, vec!["one", "two", " 10%", " 50%", "100%"]);
        assert_eq!(s.finish().as_deref(), Some("partial"));
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn lossy_utf8_and_split_multibyte() {
        let mut s = LineSplitter::new();
        let mut out = vec![];
        let bytes = "héllo\n".as_bytes();
        s.feed(&bytes[..2], &mut out);
        s.feed(&bytes[2..], &mut out);
        s.feed(b"bad \xff byte\n", &mut out);
        assert_eq!(
            out,
            vec!["héllo".to_string(), "bad \u{fffd} byte".to_string()]
        );
    }

    #[test]
    fn caps_runaway_lines() {
        let mut s = LineSplitter::new();
        let mut out = vec![];
        s.feed(&vec![b'x'; MAX_LINE_BYTES + 10], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(s.finish().unwrap().len(), 10);
    }
}
