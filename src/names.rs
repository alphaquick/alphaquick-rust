//! What a file is, from its name.
//!
//! L2 files are `<INSTRUMENT>_<VENUE>_<YYYYMMDD>.bin.lz4`. Both the
//! instrument (`BTCUSDT_261225`) and the venue (`BINANCE_USDM`) can contain
//! underscores, so the venue is matched as a known code at the end of the
//! name, not found by splitting.
//!
//! Order-log files are `<COIN>_HLC_<YYYYMMDD>.bin.lz4`: the Hyperliquid coin
//! with `:`, `@` and `/` written as `_` (`xyz:CL` is `xyz_CL`), or
//! `HIP4_<outcome id>_<short name>` for a HIP-4 outcome market with both
//! books.
//!
//! Every function takes a file name or a path; only the last component is read.

use std::path::Path;

use crate::FileKind;

/// L2 venue codes, longest first. A venue not listed yet is still read,
/// with the last part of the name as its code.
pub const L2_VENUES: &[&str] = &[
    "HYPERLIQUID_NODE",
    "BYBIT_INVERSE",
    "BINANCE_COINM",
    "BINANCE_USDM",
    "BINANCE_SPOT",
    "BINANCE_HFT",
    "HYPERLIQUID",
    "BINANCE",
    "BITHUMB",
    "LIGHTER",
    "ASTER",
    "BYBIT",
    "UPBIT",
    "DYDX",
    "OKX",
];

/// The parts of an L2 file name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct L2Name {
    /// The venue's own symbol, e.g. `BTCUSDT` or `BTC-USDT-SWAP`.
    pub symbol: String,
    /// The venue code, e.g. `BINANCE_USDM`.
    pub venue: String,
    /// The UTC day, as the number `YYYYMMDD`.
    pub day: u32,
    /// `true` for the `.continuity.jsonl` file that goes with a data file.
    pub continuity: bool,
}

/// The parts of an order-log file name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OrderLogName {
    /// Hyperliquid's name: `BTC`, `xyz:CL`, `@107`, `PURR/USDC`, `#114220`,
    /// or the `HIP4_` name of an outcome market with both books.
    pub coin: String,
    /// `perp`, the HIP-3 dex (`xyz`), `spot` or `outcome`.
    pub market: String,
    /// The UTC day, as the number `YYYYMMDD`.
    pub day: u32,
}

fn base_name(name: &Path) -> Option<&str> {
    name.file_name()?.to_str()
}

/// `name` ends with `_` and eight ASCII digits; the part before and the day.
fn split_day(name: &str) -> Option<(&str, u32)> {
    let cut = name.len().checked_sub(9)?;
    if !name.is_char_boundary(cut) {
        return None;
    }
    let (stem, tail) = name.split_at(cut);
    let digits = tail.strip_prefix('_')?;
    if stem.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((stem, digits.parse().ok()?))
}

fn all_bytes(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    s.bytes().all(ok)
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-'
}

/// `[A-Za-z0-9][A-Za-z0-9.-]*`
fn is_coin_name(s: &str) -> bool {
    s.bytes().next().is_some_and(is_alnum) && all_bytes(s, is_name_byte)
}

/// `[A-Z0-9][A-Za-z0-9]*`
fn is_pair_part(s: &str) -> bool {
    s.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && all_bytes(s, is_alnum)
}

/// `(symbol, venue)` of `<SYMBOL>_<VENUE>`.
pub fn split_l2_stem(stem: &str) -> Option<(&str, &str)> {
    // A node venue: HYPERLIQUID_NODE_<letter>_<what was recorded>.
    const NODE: &str = "_HYPERLIQUID_NODE_";
    let node_at = stem.match_indices(NODE).map(|(at, _)| at).find(|&at| {
        let b = &stem.as_bytes()[at + NODE.len()..];
        b.len() >= 3
            && b[0].is_ascii_uppercase()
            && b[1] == b'_'
            && b[2..].iter().all(|&c| is_alnum(c))
    });
    if let Some(at) = node_at.filter(|&at| at > 0) {
        return Some((&stem[..at], &stem[at + 1..]));
    }
    for venue in L2_VENUES {
        if stem.len() > venue.len() + 1 && stem.ends_with(venue) {
            let cut = stem.len() - venue.len() - 1;
            if stem.as_bytes()[cut] == b'_' {
                return Some((&stem[..cut], venue));
            }
        }
    }
    let cut = stem.rfind('_')?;
    if cut == 0 || cut == stem.len() - 1 {
        return None;
    }
    Some((&stem[..cut], &stem[cut + 1..]))
}

/// Read an L2 file name; `None` when it is not one (an order-log name is not).
pub fn parse_l2_name(name: impl AsRef<Path>) -> Option<L2Name> {
    let name = base_name(name.as_ref())?;
    if name.contains('\n') {
        return None;
    }
    let (rest, continuity) = if let Some(rest) = name.strip_suffix(".bin.lz4") {
        (rest, false)
    } else {
        (name.strip_suffix(".continuity.jsonl")?, true)
    };
    let (stem, day) = split_day(rest)?;
    if format!("{stem}_").contains("_HLC_") {
        return None;
    }
    let (symbol, venue) = split_l2_stem(stem)?;
    Some(L2Name {
        symbol: symbol.to_owned(),
        venue: venue.to_owned(),
        day,
        continuity,
    })
}

/// `(coin, market)` of the name part of an order-log file.
pub fn orderlog_coin(stem: &str) -> Option<(String, String)> {
    if stem.chars().count() > 80 {
        return None;
    }
    let outcome = || Some((stem.to_owned(), "outcome".to_owned()));
    // #<outcome id><side>
    if let Some(digits) = stem.strip_prefix('#') {
        if (2..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
            return outcome();
        }
    }
    // HIP4_<outcome id>_<short name>
    if let Some(rest) = stem.strip_prefix("HIP4_") {
        if let Some((id, short)) = rest.split_once('_') {
            if (1..=10).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_digit())
                && (1..=61).contains(&short.len())
                && is_coin_name(short)
            {
                return outcome();
            }
        }
    }
    if let Some((dex, coin)) = stem.split_once('_') {
        // <dex>_<coin>: a HIP-3 market
        let mut dex_bytes = dex.bytes();
        if (1..=16).contains(&dex.len())
            && dex_bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && dex_bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && is_coin_name(coin)
        {
            return Some((format!("{dex}:{coin}"), dex.to_owned()));
        }
        // _<index>: a spot pair by its index
        if dex.is_empty()
            && (1..=6).contains(&coin.len())
            && coin.bytes().all(|b| b.is_ascii_digit())
        {
            return Some((format!("@{coin}"), "spot".to_owned()));
        }
        // <BASE>_<QUOTE>: a spot pair
        if is_pair_part(dex) && is_pair_part(coin) {
            return Some((format!("{dex}/{coin}"), "spot".to_owned()));
        }
    }
    if is_coin_name(stem) {
        return Some((stem.to_owned(), "perp".to_owned()));
    }
    None
}

/// Read an order-log file name; `None` when it is not one.
pub fn parse_orderlog_name(name: impl AsRef<Path>) -> Option<OrderLogName> {
    let name = base_name(name.as_ref())?;
    if name.contains('\n') {
        return None;
    }
    let (rest, day) = split_day(name.strip_suffix(".bin.lz4")?)?;
    let stem = rest.strip_suffix("_HLC")?;
    if stem.is_empty() {
        return None;
    }
    let (coin, market) = orderlog_coin(stem)?;
    Some(OrderLogName { coin, market, day })
}

/// The file-name form of a coin: `xyz:CL` is `xyz_CL` (the inverse of [`orderlog_coin`]).
pub fn orderlog_stem(coin: &str) -> String {
    coin.replace([':', '@', '/'], "_")
}

/// [`FileKind::OrderLog`], [`FileKind::L2`] or `None`, from a file name.
/// [`crate::detect`] tells from the file's first record instead.
pub fn file_kind(name: impl AsRef<Path>) -> Option<FileKind> {
    let name = name.as_ref();
    if parse_orderlog_name(name).is_some() {
        Some(FileKind::OrderLog)
    } else if parse_l2_name(name).is_some() {
        Some(FileKind::L2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_names_split_the_venue_from_the_end() {
        let cases = [
            (
                "BTCUSDT_BINANCE_USDM_20261101.bin.lz4",
                "BTCUSDT",
                "BINANCE_USDM",
            ),
            (
                "BTCUSDT_261225_BINANCE_USDM_20261101.bin.lz4",
                "BTCUSDT_261225",
                "BINANCE_USDM",
            ),
            ("BTC-USDT-SWAP_OKX_20261101.bin.lz4", "BTC-USDT-SWAP", "OKX"),
            (
                "YB-USD_UM_XPERP-310829_OKX_20261101.bin.lz4",
                "YB-USD_UM_XPERP-310829",
                "OKX",
            ),
            (
                "YGG_HYPERLIQUID_NODE_B_bbo_20261101.bin.lz4",
                "YGG",
                "HYPERLIQUID_NODE_B_bbo",
            ),
            (
                "BTC_HYPERLIQUID_NODE_20261101.bin.lz4",
                "BTC",
                "HYPERLIQUID_NODE",
            ),
            ("BTC_HYPERLIQUID_20261101.bin.lz4", "BTC", "HYPERLIQUID"),
            ("KRW-BTC_UPBIT_20261101.bin.lz4", "KRW-BTC", "UPBIT"),
            ("ABC_NEWVENUE_20261101.bin.lz4", "ABC", "NEWVENUE"),
            ("ABCOKX_20261101.bin.lz4", "ABCOKX", ""),
        ];
        for (name, symbol, venue) in cases {
            let parsed = parse_l2_name(format!("/data/20261101/{name}"));
            if venue.is_empty() {
                assert_eq!(parsed, None, "{name}");
                continue;
            }
            let parsed = parsed.unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                (
                    parsed.symbol.as_str(),
                    parsed.venue.as_str(),
                    parsed.day,
                    parsed.continuity
                ),
                (symbol, venue, 20261101, false),
                "{name}"
            );
        }
        let side = parse_l2_name("BTC_HYPERLIQUID_20261101.continuity.jsonl").unwrap();
        assert!(side.continuity && side.venue == "HYPERLIQUID");
        assert_eq!(parse_l2_name("BTC_HLC_20261101.bin.lz4"), None);
        assert_eq!(parse_l2_name("notes.txt"), None);
        assert_eq!(parse_l2_name("_OKX_20261101.bin.lz4"), None);
        assert_eq!(parse_l2_name("BTC_OKX_2026110.bin.lz4"), None);
        assert_eq!(
            split_l2_stem("_HYPERLIQUID_NODE_B_bbo"),
            Some(("_HYPERLIQUID_NODE_B", "bbo"))
        );
    }

    #[test]
    fn orderlog_names_read_back_the_coin() {
        let cases = [
            ("BTC", "BTC", "perp"),
            ("kBONK", "kBONK", "perp"),
            ("xyz_CL", "xyz:CL", "xyz"),
            ("xyz_BRK.B", "xyz:BRK.B", "xyz"),
            ("PURR_USDC", "PURR/USDC", "spot"),
            ("_107", "@107", "spot"),
            ("#114220", "#114220", "outcome"),
            (
                "HIP4_3600_Fed-Oct-no-change",
                "HIP4_3600_Fed-Oct-no-change",
                "outcome",
            ),
            // without the short name it is no merged outcome file
            ("HIP4_3600", "HIP4/3600", "spot"),
        ];
        for (stem, coin, market) in cases {
            let parsed = parse_orderlog_name(format!("{stem}_HLC_20261101.bin.lz4"))
                .unwrap_or_else(|| panic!("{stem}"));
            assert_eq!(
                (parsed.coin.as_str(), parsed.market.as_str(), parsed.day),
                (coin, market, 20261101),
                "{stem}"
            );
            assert_eq!(orderlog_stem(coin), stem);
        }
        assert_eq!(parse_orderlog_name("BT C_HLC_20261101.bin.lz4"), None);
        assert_eq!(parse_orderlog_name("_HLC_20261101.bin.lz4"), None);
        assert_eq!(orderlog_coin("#1"), None);
        assert_eq!(orderlog_coin(&"A".repeat(81)), None);
        assert_eq!(
            file_kind("xyz_CL_HLC_20261101.bin.lz4"),
            Some(FileKind::OrderLog)
        );
        assert_eq!(
            file_kind("BTCUSDT_BINANCE_USDM_20261101.bin.lz4"),
            Some(FileKind::L2)
        );
        assert_eq!(file_kind("ZEC_HLC_20261014_sample.bin.lz4"), None);
    }
}
