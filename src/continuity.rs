//! The continuity file of an L2 data file: where the recorder seeded the book,
//! and where the feed broke.
//!
//! Each L2 data file `<NAME>.bin.lz4` comes with `<NAME>.continuity.jsonl`
//! ([`path_of`]): JSON Lines, one flat object per line, written by the
//! recorder. [`read`] returns its [`Line`]s. Times are Unix milliseconds.
//!
//! * **`seed`** lines: a Clear transaction was written, and the book starts
//!   again from a full copy. Reasons: `day_roll` (the first seed of the file,
//!   at the start of the UTC day), `snapshot` (the venue's snapshot when the
//!   recorder subscribed, also after a recorder restart), `resync` (the book
//!   rebuilt after a gap); `legacy_seed` in files up to 2026-11-25, one each
//!   time the recorder started on the file. **The `n`-th seed line describes
//!   the `n`-th Clear transaction of the data file.**
//! * **`gap`** lines (from 2026-11-26): the feed broke, and the book is
//!   unknown from here to the next seed. The data file holds no depth updates
//!   of the instrument in between, so a book rebuilt from it stays frozen at
//!   its last state, which is out of date; trades can continue. Reasons:
//!   `disconnect`, `sequence`, `crossed` (dYdX left the book crossed),
//!   `unparsable` (a book the format cannot hold). Fields: `kind`,
//!   `reason`, `ordinal`, `exchange`, `symbol`, `received_at_ms`,
//!   `written_at_ms`.
//! * Files up to 2026-11-25 have, for the venues, only `legacy_seed` lines:
//!   that recorder did not detect lost messages, so there are no gaps even
//!   where data is missing. Files of the node venues (`HYPERLIQUID_NODE_*`)
//!   can also hold `incident`, `gap` (with `detected_at_ms`,
//!   `last_good_ordinal` and `clear_ordinal`, written after the seed that
//!   ends it), `no_loss`, `resume_complete` and `writer_failure` lines;
//!   `no_loss` and `resume_complete` mean nothing was lost.
//!
//! The set of kinds, reasons and fields can grow: every line keeps all its
//! key/value pairs ([`Line::fields`]), and unknown kinds are returned as they
//! are.
//!
//! To read the depth only where it is known, merge the gaps into the events
//! of the data file with [`crate::l2::Reader::with_continuity`] and apply
//! them to an [`crate::L2Book`], which then reports
//! [`is_known`](crate::L2Book::is_known):
//!
//! ```no_run
//! use alphaquick::{continuity, l2, L2Book};
//!
//! let data = "BTCUSDT_BINANCE_USDM_20261126.bin.lz4";
//! let lines = continuity::read(continuity::path_of(data))?;
//! let mut book = L2Book::new();
//! for event in l2::Reader::open(data)?.with_continuity(&lines) {
//!     book.apply_event(&event?);
//!     if book.is_consistent() && book.is_known() {
//!         // the book is the venue's as of `book.time()`
//!     }
//! }
//! # Ok::<(), alphaquick::Error>(())
//! ```
//!
//! See <https://docs.alphaquick.io/data/l2-depth-trades#continuity-file>.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::time::unix_ms_to_ticks;

/// [`Line::kind`] of a seed line.
pub const KIND_SEED: &str = "seed";
/// [`Line::kind`] of a gap line.
pub const KIND_GAP: &str = "gap";

/// The deepest nesting of arrays and objects a line may have.
const MAX_DEPTH: usize = 64;

/// A value of a continuity line.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as written, so that 64-bit integers stay exact.
    Number(String),
    /// A string, unescaped.
    String(String),
    /// An array or an object, as its JSON text (the writers do not write any).
    Raw(String),
}

impl Value {
    /// The string, for a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    /// The number, for a [`Value::Number`] written as an integer that fits.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The number, for a [`Value::Number`] written as a non-negative integer that fits.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The number, for a [`Value::Number`].
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The value, for a [`Value::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// `true` for [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// One line of a continuity file: the common fields, typed, and every
/// key/value pair as written.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Line {
    /// `seed`, `gap`, or another kind (empty when the line has none).
    pub kind: String,
    /// Why (`resync`, `sequence`, ...); empty when the line has none.
    pub reason: String,
    /// `ordinal`: the line's depth transaction, counted from 1 since the
    /// recorder (re)opened the data file. A restart starts the count again.
    pub ordinal: Option<u64>,
    /// `received_at_ms`: when the recorder received the data, Unix ms.
    pub received_at_ms: Option<i64>,
    /// `detected_at_ms` (old node lines): when the break was detected, Unix ms.
    pub detected_at_ms: Option<i64>,
    /// `written_at_ms`: when the recorder wrote the line, Unix ms.
    pub written_at_ms: Option<i64>,
    /// `ts_ms` (seeds): the venue's time of the snapshot, when it gives one, Unix ms.
    pub ts_ms: Option<i64>,
    /// Every key/value pair of the line, in the order written.
    pub fields: Vec<(String, Value)>,
}

impl Line {
    /// Parse one line: a JSON object. Keys this version does not know are
    /// kept in [`Line::fields`]; a field of an unexpected type reads as
    /// absent in the typed fields.
    pub fn parse(text: &str) -> Result<Line> {
        Parser::new(text.as_bytes())
            .document()
            .map(Line::from_fields)
            .map_err(|why| Error::format(format!("continuity line: {why}")))
    }

    fn from_fields(fields: Vec<(String, Value)>) -> Line {
        let get = |key: &str| fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v);
        let text = |key| {
            get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let number = |key| get(key).and_then(Value::as_i64);
        Line {
            kind: text("kind"),
            reason: text("reason"),
            ordinal: get("ordinal").and_then(Value::as_u64),
            received_at_ms: number("received_at_ms"),
            detected_at_ms: number("detected_at_ms"),
            written_at_ms: number("written_at_ms"),
            ts_ms: number("ts_ms"),
            fields,
        }
    }

    /// The value of a key (the last one, should a key repeat).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value)
    }

    /// A `seed` line.
    pub fn is_seed(&self) -> bool {
        self.kind == KIND_SEED
    }

    /// A `gap` line.
    pub fn is_gap(&self) -> bool {
        self.kind == KIND_GAP
    }

    /// When it happened, as ticks: `received_at_ms`, or else `detected_at_ms`
    /// (old node lines), or else `written_at_ms`.
    pub fn time(&self) -> Option<i64> {
        self.received_at_ms
            .or(self.detected_at_ms)
            .or(self.written_at_ms)
            .map(unix_ms_to_ticks)
    }
}

/// The continuity file of a data file: `X.bin.lz4` → `X.continuity.jsonl`
/// in the same directory (any other name gets `.continuity.jsonl` added).
pub fn path_of(data_file: impl AsRef<Path>) -> PathBuf {
    let data_file = data_file.as_ref();
    let name = data_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".bin.lz4").unwrap_or(&name);
    data_file.with_file_name(format!("{stem}.continuity.jsonl"))
}

/// Read a continuity file. See [`parse`].
pub fn read(path: impl AsRef<Path>) -> Result<Vec<Line>> {
    from_reader(File::open(path)?)
}

/// Read a continuity file from any byte source. See [`parse`].
pub fn from_reader<R: Read>(mut reader: R) -> Result<Vec<Line>> {
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;
    parse(data)
}

/// The lines of a continuity file, in file order. Blank lines are skipped.
///
/// A last line without its newline that does not parse is a line the
/// recorder was writing when the file was copied, or when it stopped: it is
/// left out. Any other line that is not a JSON object is an
/// [`Error::Format`] naming the line; use [`Line::parse`] line by line to
/// skip such lines instead.
pub fn parse(data: impl AsRef<[u8]>) -> Result<Vec<Line>> {
    let data = data.as_ref();
    let complete = data.ends_with(b"\n");
    let mut parts: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if complete {
        parts.pop(); // the empty piece after the last newline
    }
    let last = parts.len().saturating_sub(1);
    let mut lines = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        let part = part.strip_suffix(b"\r").unwrap_or(part);
        if part.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed = std::str::from_utf8(part)
            .map_err(|_| "not UTF-8".to_owned())
            .and_then(|text| Parser::new(text.as_bytes()).document());
        match parsed {
            Ok(fields) => lines.push(Line::from_fields(fields)),
            Err(_) if index == last && !complete => {} // torn last line
            Err(why) => {
                return Err(Error::format(format!(
                    "continuity file, line {}: {why}",
                    index + 1
                )))
            }
        }
    }
    Ok(lines)
}

/// A strict reader of one JSON object (RFC 8259), with values kept as written.
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

type Parsed<T> = std::result::Result<T, String>;

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Parser { bytes, at: 0 }
    }

    /// The whole input: one object, nothing after it but white space.
    fn document(mut self) -> Parsed<Vec<(String, Value)>> {
        self.space();
        if self.peek() != Some(b'{') {
            return Err("not a JSON object".to_owned());
        }
        let fields = self.object(0)?;
        self.space();
        if self.at != self.bytes.len() {
            return Err(format!("text after the object at byte {}", self.at));
        }
        Ok(fields)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Parsed<()> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else if self.at >= self.bytes.len() {
            Err("the line ends early".to_owned())
        } else {
            Err(format!(
                "expected '{}' at byte {}",
                char::from(byte),
                self.at
            ))
        }
    }

    fn object(&mut self, depth: usize) -> Parsed<Vec<(String, Value)>> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(fields);
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.expect(b':')?;
            self.space();
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(fields);
                }
                None => return Err("the line ends early".to_owned()),
                Some(_) => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Parsed<()> {
        self.expect(b'[')?;
        self.space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(());
        }
        loop {
            self.space();
            self.value(depth + 1)?;
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(());
                }
                None => return Err("the line ends early".to_owned()),
                Some(_) => return Err(format!("expected ',' or ']' at byte {}", self.at)),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Parsed<Value> {
        if depth > MAX_DEPTH {
            return Err("values nested too deep".to_owned());
        }
        let start = self.at;
        match self.peek() {
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'{') => {
                self.object(depth)?;
                Ok(Value::Raw(self.text(start)))
            }
            Some(b'[') => {
                self.array(depth)?;
                Ok(Value::Raw(self.text(start)))
            }
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => {
                self.number()?;
                Ok(Value::Number(self.text(start)))
            }
            None => Err("the line ends early".to_owned()),
            Some(_) => Err(format!("no value at byte {}", self.at)),
        }
    }

    /// The input from `start` to here; always whole UTF-8 characters, since
    /// values start and end at ASCII bytes.
    fn text(&self, start: usize) -> String {
        String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned()
    }

    fn word(&mut self, word: &str, value: Value) -> Parsed<Value> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("no value at byte {}", self.at))
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Parsed<()> {
        let bad = |at: usize| format!("bad number at byte {at}");
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(bad(start)),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(bad(start));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(bad(start));
            }
        }
        Ok(())
    }

    fn hex4(&mut self) -> Parsed<u32> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| "the line ends early".to_owned())?;
        let mut value = 0u32;
        for &digit in digits {
            let nibble = char::from(digit)
                .to_digit(16)
                .ok_or_else(|| format!("bad \\u escape at byte {}", self.at))?;
            value = value * 16 + nibble;
        }
        self.at += 4;
        Ok(value)
    }

    fn string(&mut self) -> Parsed<String> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("the line ends inside a string".to_owned());
            };
            self.at += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let Some(escape) = self.peek() else {
                        return Err("the line ends inside a string".to_owned());
                    };
                    self.at += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&high)
                                && self.bytes[self.at..].starts_with(b"\\u")
                            {
                                let back = self.at;
                                self.at += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                                } else {
                                    self.at = back; // not a pair: read it on its own
                                    high
                                }
                            } else {
                                high
                            };
                            // A lone surrogate becomes U+FFFD.
                            char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
                        }
                        _ => return Err(format!("bad escape at byte {}", self.at - 1)),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0x00..=0x1F => {
                    return Err(format!(
                        "control character in a string at byte {}",
                        self.at - 1
                    ))
                }
                _ => out.push(byte),
            }
        }
        // The input is text, and escapes add whole characters.
        String::from_utf8(out).map_err(|_| "not UTF-8".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TICKS_AT_UNIX_EPOCH;

    const NEW: &str = concat!(
        r#"{"bt_ms":null,"epoch":null,"exchange":"BINANCE_USDM","kind":"seed","levels":2000,"ordinal":1,"reason":"day_roll","received_at_ms":1795615200000,"seq":12236779505659,"symbol":"AAPLUSDT@BINANCE_USDM","ts_ms":1795615199990,"written_at_ms":1795615200001}"#,
        "\n",
        r#"{"exchange":"BINANCE_USDM","kind":"gap","ordinal":1,"reason":"sequence","received_at_ms":1795615793314,"symbol":"AAPLUSDT@BINANCE_USDM","written_at_ms":1795615793314}"#,
        "\n",
        r#"{"bt_ms":null,"epoch":null,"exchange":"BINANCE_USDM","kind":"seed","levels":2000,"ordinal":2,"reason":"resync","received_at_ms":1795615794048,"seq":12236779505659,"symbol":"AAPLUSDT@BINANCE_USDM","ts_ms":1795615793960,"written_at_ms":1795615794048}"#,
        "\n"
    );

    #[test]
    fn lines_of_the_new_recorder() {
        let lines = parse(NEW).unwrap();
        assert_eq!(lines.len(), 3);
        let kinds: Vec<(&str, &str)> = lines
            .iter()
            .map(|l| (l.kind.as_str(), l.reason.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("seed", "day_roll"),
                ("gap", "sequence"),
                ("seed", "resync")
            ]
        );
        let gap = &lines[1];
        assert!(gap.is_gap() && !gap.is_seed() && lines[0].is_seed());
        assert_eq!(gap.ordinal, Some(1));
        assert_eq!(gap.received_at_ms, Some(1_795_615_793_314));
        assert_eq!(
            gap.time(),
            Some(TICKS_AT_UNIX_EPOCH + 1_795_615_793_314 * 10_000)
        );
        assert_eq!(
            gap.get("symbol").and_then(Value::as_str),
            Some("AAPLUSDT@BINANCE_USDM")
        );
        let seed = &lines[2];
        assert_eq!(seed.ts_ms, Some(1_795_615_793_960));
        assert_eq!(
            seed.get("seq").and_then(Value::as_u64),
            Some(12_236_779_505_659)
        );
        assert!(seed.get("bt_ms").unwrap().is_null());
        assert_eq!(seed.fields.len(), 12);
        assert_eq!(seed.fields[0].0, "bt_ms");
    }

    #[test]
    fn lines_of_the_old_recorder() {
        let text = concat!(
            r#"{"bt_ms":null,"epoch":null,"exchange":"UPBIT","kind":"seed","ordinal":1,"reason":"legacy_seed","received_at_ms":1791072000081,"seq":null,"symbol":"KRW-BTC@UPBIT","ts_ms":1791072000046,"written_at_ms":1791072000166}"#,
            "\n",
            r#"{"kind":"incident","written_at_ms":5,"exchange":"HL","symbol":"S","incident_id":7,"detected_at_ms":1000,"reason":"sequence_jump","epoch":3,"from_seq":100,"reported_through_seq":110,"connection_id":5,"server_connection_id":null,"exact":true}"#,
            "\n",
            r#"{"kind":"gap","written_at_ms":9,"detected_at_ms":1000,"reason":"sequence_jump","exact":false,"anomaly":null,"missing_from_seq":101,"last_good_ordinal":41,"clear_ordinal":42}"#,
            "\n",
            r#"{"kind":"writer_failure","exchange":"HL","symbol":"S","error":"disk full"}"#,
            "\n",
        );
        let lines = parse(text).unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].reason, "legacy_seed");
        assert_eq!(lines[1].kind, "incident");
        assert_eq!(lines[1].get("exact").and_then(Value::as_bool), Some(true));
        let gap = &lines[2];
        assert!(gap.is_gap());
        assert_eq!((gap.received_at_ms, gap.detected_at_ms), (None, Some(1000)));
        assert_eq!(gap.time(), Some(TICKS_AT_UNIX_EPOCH + 1000 * 10_000));
        assert_eq!(gap.get("clear_ordinal").and_then(Value::as_u64), Some(42));
        let failure = &lines[3];
        assert_eq!(
            (failure.kind.as_str(), failure.reason.as_str()),
            ("writer_failure", "")
        );
        assert_eq!(failure.time(), None);
    }

    #[test]
    fn a_torn_last_line_is_left_out() {
        let mut text = NEW.to_owned();
        text.push_str(r#"{"exchange":"BINANCE_USDM","kind":"gap","ord"#);
        assert_eq!(parse(&text).unwrap().len(), 3);
        // A complete last line without its newline is kept.
        let mut whole = NEW.trim_end().to_owned();
        assert_eq!(parse(&whole).unwrap().len(), 3);
        // Cut inside a multi-byte character.
        whole.push_str("\n{\"kind\":\"gap\",\"reason\":\"\u{e9}");
        let cut = &whole.as_bytes()[..whole.len() - 1];
        assert_eq!(parse(cut).unwrap().len(), 3);
        // A damaged line anywhere else is an error.
        let damaged = format!("{{\"kind\":\"se\n{NEW}");
        let error = parse(damaged).unwrap_err();
        assert!(error.to_string().contains("line 1"), "{error}");
        let mut ended = text.clone();
        ended.push('\n');
        assert!(parse(ended).is_err());
        // Blank lines and CRLF line ends are fine.
        let spaced = format!("\r\n  \n{}", NEW.replace('\n', "\r\n"));
        assert_eq!(parse(spaced).unwrap().len(), 3);
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn json_values() {
        let line = Line::parse(
            r#" { "s" : "a\"b\\c\/\n\u00e9\ud83d\ude00\ud800x" , "n": -12.5e+3, "z": 0, "big": 18446744073709551615,
                 "t": true, "f": false, "o": {"a": [1, {"b": null}]}, "e": [], "kind": 7, "s": "last" } "#,
        )
        .unwrap();
        assert_eq!(
            line.fields[0].1.as_str(),
            Some("a\"b\\c/\n\u{e9}\u{1f600}\u{fffd}x")
        );
        assert_eq!(line.get("s").and_then(Value::as_str), Some("last")); // the last one wins
        assert_eq!(line.get("n").and_then(Value::as_f64), Some(-12500.0));
        assert_eq!(line.get("n").and_then(Value::as_i64), None);
        assert_eq!(line.get("z").and_then(Value::as_i64), Some(0));
        assert_eq!(line.get("big").and_then(Value::as_u64), Some(u64::MAX));
        assert_eq!(line.get("big").and_then(Value::as_i64), None);
        assert_eq!(line.get("t").and_then(Value::as_bool), Some(true));
        assert_eq!(
            line.get("o"),
            Some(&Value::Raw(r#"{"a": [1, {"b": null}]}"#.to_owned()))
        );
        assert_eq!(line.get("e"), Some(&Value::Raw("[]".to_owned())));
        assert_eq!(line.kind, ""); // not a string: absent
        assert_eq!(Line::parse("{}").unwrap().fields.len(), 0);
        for bad in [
            "",
            "[]",
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "{\"a\":01}",
            "{\"a\":1.}",
            "{\"a\":-}",
            "{\"a\":1e}",
            "{\"a\":tru}",
            "{\"a\":\"\\x\"}",
            "{\"a\":\"\\u12\"}",
            "{\"a\":\"tab\there\"}",
            "{\"a\":1} x",
            "{a:1}",
            "{\"a\":[1 2]}",
        ] {
            assert!(Line::parse(bad).is_err(), "{bad:?}");
        }
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(100), "]".repeat(100));
        assert!(Line::parse(&deep).is_err());
        let fine = format!("{{\"a\":{}{}}}", "[".repeat(10), "]".repeat(10));
        assert!(Line::parse(&fine).is_ok());
    }

    #[test]
    fn the_continuity_file_of_a_data_file() {
        assert_eq!(
            path_of("/d/20261126/BTC_HYPERLIQUID_BBO_20261126.bin.lz4"),
            PathBuf::from("/d/20261126/BTC_HYPERLIQUID_BBO_20261126.continuity.jsonl")
        );
        assert_eq!(
            path_of("BTC_OKX_20261126.bin"),
            PathBuf::from("BTC_OKX_20261126.bin.continuity.jsonl")
        );
    }
}
