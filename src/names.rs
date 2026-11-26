//! What a file is, from its name.
//!
//! L2 files are `<INSTRUMENT>_<VENUE>_<YYYYMMDD>.bin.lz4`. Both the
//! instrument (`BTCUSDT_261225`) and the venue (`BINANCE_USDM`) can contain
//! underscores, so the venue is matched as a known code at the end of the
//! name, not found by splitting.
//!
//! Order-log files are published one folder per day and, inside it, one
//! folder per market type (in R2 under `hyperliquid/hlc_v2/`):
//!
//! ```text
//! <YYYYMMDD>/PERPS/<COIN>_HLC_<YYYYMMDD>.bin.lz4             main perpetuals: BTC, kPEPE
//! <YYYYMMDD>/HIP3/<dex>_<COIN>_HLC_<YYYYMMDD>.bin.lz4        HIP-3 markets: xyz_NVDA is xyz:NVDA
//! <YYYYMMDD>/SPOT/<BASE>_<QUOTE>_HLC_<YYYYMMDD>.bin.lz4      spot pairs: HYPE_USDC (@107), PURR_USDC
//! <YYYYMMDD>/HIP4/<title>_id<outcome id>_HLC_<YYYYMMDD>.bin.lz4   HIP-4 outcome markets, both books
//! <YYYYMMDD>/INSTRUMENTS_HLC_<YYYYMMDD>.json                 every file of the day, with its venue id
//! ```
//!
//! A HIP-4 market's title is written with `22:50` as `2250`, `$` left out
//! and every other run of characters but letters, digits and dots inside
//! numbers as `-`, never shortened
//! (`BTC-touches-60000-by-2026-12-01-0000-UTC_id37348`). The name is a label
//! to read: the venue's own id of a spot pair (`@107`) or of an outcome
//! (`HIP4:<id>`) is the coin of the file's META record and is listed in
//! `INSTRUMENTS_HLC_<YYYYMMDD>.json`.
//!
//! When a path has one of those folders, the folder says the market type
//! ([`parse_orderlog_name`]); a bare name is read by its shape
//! ([`orderlog_coin`]). Names of an earlier layout, which was not published,
//! are still read: `_107` (the spot pair `@107`), `HIP4_<outcome id>[_<short
//! name>]` (from a `hip4/` folder) and `#<outcome id><side>` (one side of an
//! outcome market).
//!
//! Every function takes a file name or a path; only the last component is
//! read, and for an order-log file the folder it is in.

use std::path::Path;

use crate::FileKind;

/// The folders of a day's order-log files, one per market type.
pub const ORDERLOG_FOLDERS: &[&str] = &["PERPS", "HIP3", "SPOT", "HIP4"];

/// Longest name part of a perp, HIP-3 or spot order-log file.
const MAX_NAME: usize = 80;
/// Longest name part of a HIP-4 file: its title is never shortened, up to a
/// 255-byte file name.
const MAX_OUTCOME_NAME: usize = 255 - "_HLC_YYYYMMDD.bin.lz4".len();

/// L2 venue codes, longest first. A venue not listed yet is still read,
/// with the last part of the name as its code.
pub const L2_VENUES: &[&str] = &[
    "BINANCE_BBO_COINM",
    "BINANCE_BBO_USDM",
    "BINANCE_BBO_SPOT",
    "HYPERLIQUID_NODE",
    "HYPERLIQUID_BBO",
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
    /// Hyperliquid's name: `BTC`, `xyz:CL`, `PURR/USDC`, or `HIP4:<id>` for
    /// a HIP-4 outcome file (as its META says). A spot file is named by its
    /// tokens, so `SPOT/HYPE_USDC` reads `HYPE/USDC`; the venue's `@107` is in
    /// META and INSTRUMENTS. Names of the earlier layout read as before:
    /// `_107` is `@107`, `HIP4_<id>[_<short name>]` itself, and `#114220` is
    /// one side of an outcome, as files of the withdrawn first build named
    /// them.
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

/// `<title>_id<outcome id>`: `HIP4:<outcome id>`.
fn outcome_coin(stem: &str) -> Option<(String, String)> {
    if stem.len() > MAX_OUTCOME_NAME {
        return None;
    }
    let (title, id) = stem.rsplit_once("_id")?;
    if is_coin_name(title) && (1..=10).contains(&id.len()) && all_bytes(id, |b| b.is_ascii_digit())
    {
        Some((format!("HIP4:{id}"), "outcome".to_owned()))
    } else {
        None
    }
}

/// `<dex>_<coin>`, the dex in lower case: `(dex:coin, dex)`.
fn hip3_coin(stem: &str) -> Option<(String, String)> {
    let (dex, coin) = stem.split_once('_')?;
    let mut dex_bytes = dex.bytes();
    if (1..=16).contains(&dex.len())
        && dex_bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && dex_bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && is_coin_name(coin)
    {
        Some((format!("{dex}:{coin}"), dex.to_owned()))
    } else {
        None
    }
}

/// `(coin, market)` of the name part of an order-log file, read by its
/// shape: `<title>_id<N>` is the outcome `HIP4:<N>`, `<dex>_<COIN>` (the dex
/// in lower case) a HIP-3 market, `<BASE>_<QUOTE>` the spot pair
/// `BASE/QUOTE` (a display name: the venue's `@N` is in the file's META and
/// in INSTRUMENTS), anything else of letters, digits, dots and dashes a
/// perpetual. [`orderlog_coin_in`] reads a name in its folder.
pub fn orderlog_coin(stem: &str) -> Option<(String, String)> {
    if let Some(outcome) = outcome_coin(stem) {
        return Some(outcome);
    }
    if stem.chars().count() > MAX_NAME {
        return None;
    }
    let outcome = || Some((stem.to_owned(), "outcome".to_owned()));
    // #<outcome id><side>
    if let Some(digits) = stem.strip_prefix('#') {
        if (2..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
            return outcome();
        }
    }
    // HIP4_<outcome id>_<short name>, or HIP4_<outcome id> without a short name
    if let Some(rest) = stem.strip_prefix("HIP4_") {
        let (id, short) = rest.split_once('_').unwrap_or((rest, ""));
        if (1..=10).contains(&id.len())
            && id.bytes().all(|b| b.is_ascii_digit())
            && (short.is_empty() || ((1..=61).contains(&short.len()) && is_coin_name(short)))
        {
            return outcome();
        }
    }
    if let Some(hip3) = hip3_coin(stem) {
        return Some(hip3);
    }
    if let Some((dex, coin)) = stem.split_once('_') {
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

/// `(coin, market)` of the name part of an order-log file in the folder
/// `folder`. `PERPS`, `HIP3`, `SPOT` and `HIP4` say the market type, and the
/// name must have that type's shape (`SPOT/abc_USDC` is the spot pair
/// `abc/USDC`, not a HIP-3 market); in any other folder the name is read by
/// its shape alone ([`orderlog_coin`]).
pub fn orderlog_coin_in(folder: &str, stem: &str) -> Option<(String, String)> {
    let short = || stem.chars().count() <= MAX_NAME;
    match folder {
        "HIP4" => outcome_coin(stem),
        "PERPS" => (short() && is_coin_name(stem)).then(|| (stem.to_owned(), "perp".to_owned())),
        "HIP3" => hip3_coin(stem).filter(|_| short()),
        "SPOT" => {
            let (base, quote) = stem.split_once('_')?;
            (short() && is_coin_name(base) && is_coin_name(quote))
                .then(|| (format!("{base}/{quote}"), "spot".to_owned()))
        }
        _ => orderlog_coin(stem),
    }
}

/// Read an order-log file name; `None` when it is not one. In a path, the
/// folder the file is in decides the market type when it is `PERPS`,
/// `HIP3`, `SPOT` or `HIP4` (see [`orderlog_coin_in`]).
pub fn parse_orderlog_name(name: impl AsRef<Path>) -> Option<OrderLogName> {
    let path = name.as_ref();
    let name = base_name(path)?;
    if name.contains('\n') {
        return None;
    }
    let (rest, day) = split_day(name.strip_suffix(".bin.lz4")?)?;
    let stem = rest.strip_suffix("_HLC")?;
    if stem.is_empty() {
        return None;
    }
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|folder| folder.to_str())
        .unwrap_or("");
    let (coin, market) = orderlog_coin_in(folder, stem)?;
    Some(OrderLogName { coin, market, day })
}

/// The file-name form of a coin: `xyz:CL` is `xyz_CL`, `PURR/USDC` is
/// `PURR_USDC` (the inverse of [`orderlog_coin`] for perpetuals, HIP-3
/// markets and spot pairs named by their tokens). A spot pair the venue
/// calls `@107` and a HIP-4 outcome are named by their tokens and their
/// title, which the coin does not hold: take their names from
/// `INSTRUMENTS_HLC_<YYYYMMDD>.json` (`orderlog_stem("@107")` gives the
/// earlier layout's `_107`).
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
            (
                "BTC_HYPERLIQUID_BBO_20261101.bin.lz4",
                "BTC",
                "HYPERLIQUID_BBO",
            ),
            (
                "xyz_TSLA_HYPERLIQUID_BBO_20261101.bin.lz4",
                "xyz_TSLA",
                "HYPERLIQUID_BBO",
            ),
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
            // a market without a title has no short name
            ("HIP4_3600", "HIP4_3600", "outcome"),
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
        assert_eq!(orderlog_coin("HIP4_"), None);
        let in_dir = parse_orderlog_name(
            "hyperliquid/hlc_v2/20261117/hip4/HIP4_1473_2026-2027-English-Premier-League-winner-A_HLC_20261117.bin.lz4",
        )
        .unwrap();
        assert_eq!((in_dir.market.as_str(), in_dir.day), ("outcome", 20261117));
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

    #[test]
    fn orderlog_names_of_the_published_layout() {
        let nba = "2026-2027-NBA-winner-Oklahoma-City-Thunder_id11492";
        let in_folders = [
            ("PERPS/BTC".to_owned(), "BTC", "perp"),
            ("PERPS/kPEPE".to_owned(), "kPEPE", "perp"),
            ("HIP3/xyz_NVDA".to_owned(), "xyz:NVDA", "xyz"),
            ("HIP3/xyz_BRK.B".to_owned(), "xyz:BRK.B", "xyz"),
            // the venue's @107: in META and INSTRUMENTS
            ("SPOT/HYPE_USDC".to_owned(), "HYPE/USDC", "spot"),
            ("SPOT/PURR_USDC".to_owned(), "PURR/USDC", "spot"),
            // the folder decides: bare, it reads as the HIP-3 dex abc
            ("SPOT/abc_USDC".to_owned(), "abc/USDC", "spot"),
            (
                "HIP4/Anthropic-first-day-market-cap-above-1500B_id8573".to_owned(),
                "HIP4:8573",
                "outcome",
            ),
            (format!("HIP4/{nba}"), "HIP4:11492", "outcome"),
            (
                "HIP4/BTC-above-64800-at-2026-11-17-2250-UTC_id32511".to_owned(),
                "HIP4:32511",
                "outcome",
            ),
            // a market with no title
            ("HIP4/outcome_id4021".to_owned(), "HIP4:4021", "outcome"),
        ];
        for (path, coin, market) in &in_folders {
            let parsed =
                parse_orderlog_name(format!("/data/hlc_v2/20261117/{path}_HLC_20261117.bin.lz4"))
                    .unwrap_or_else(|| panic!("{path}"));
            assert_eq!(
                (parsed.coin.as_str(), parsed.market.as_str(), parsed.day),
                (*coin, *market, 20261117),
                "{path}"
            );
            assert_eq!(
                file_kind(format!("{path}_HLC_20261117.bin.lz4")),
                Some(FileKind::OrderLog),
                "{path}"
            );
        }
        // Bare names, read by their shape.
        let bare = [
            ("BTC", "BTC", "perp"),
            ("xyz_NVDA", "xyz:NVDA", "xyz"),
            ("HYPE_USDC", "HYPE/USDC", "spot"),
            ("abc_USDC", "abc:USDC", "abc"),
            (nba, "HIP4:11492", "outcome"),
            ("outcome_id4021", "HIP4:4021", "outcome"),
            (
                "BTC-touches-60000-by-2026-12-01-0000-UTC_id37348",
                "HIP4:37348",
                "outcome",
            ),
        ];
        for (stem, coin, market) in bare {
            let parsed = parse_orderlog_name(format!("{stem}_HLC_20261119.bin.lz4"))
                .unwrap_or_else(|| panic!("{stem}"));
            assert_eq!(
                (parsed.coin.as_str(), parsed.market.as_str()),
                (coin, market),
                "{stem}"
            );
        }
        // A title is never shortened: longer than any other name.
        let long_title = format!(
            "{}_id99",
            ["Will-the-Federal-Reserve-cut-rates"; 5].join("-")
        );
        assert!(long_title.len() > 80);
        for name in [
            format!("HIP4/{long_title}_HLC_20261119.bin.lz4"),
            format!("{long_title}_HLC_20261119.bin.lz4"),
        ] {
            assert_eq!(
                parse_orderlog_name(&name).map(|n| n.coin),
                Some("HIP4:99".to_owned())
            );
        }
        let long_perp = format!("PERPS/{}_HLC_20261119.bin.lz4", "A".repeat(81));
        assert_eq!(parse_orderlog_name(long_perp), None);
        // In a type folder the name must have that type's shape.
        for path in [
            "PERPS/xyz_NVDA",
            "HIP3/BTC",
            "HIP3/HYPE_USDC",
            "SPOT/BTC",
            "SPOT/_107",
            "HIP4/BTC",
            "HIP4/HIP4_3600_Fed-Oct-no-change",
            "HIP4/Fed_id",
            "HIP4/_id12",
            "HIP4/a_b_id12",
        ] {
            assert_eq!(
                parse_orderlog_name(format!("20261117/{path}_HLC_20261117.bin.lz4")),
                None,
                "{path}"
            );
        }
        // Any other folder: the name alone, so the earlier layout reads as before.
        let old = parse_orderlog_name("20261117/hip4/HIP4_1473_x_HLC_20261117.bin.lz4").unwrap();
        assert_eq!(old.coin, "HIP4_1473_x");
        let old = parse_orderlog_name("20261117/_107_HLC_20261117.bin.lz4").unwrap();
        assert_eq!(old.coin, "@107");
        assert_eq!(
            orderlog_coin_in("HIP4", nba),
            Some(("HIP4:11492".to_owned(), "outcome".to_owned()))
        );
        assert_eq!(
            orderlog_coin("HYPE_USDC"),
            Some(("HYPE/USDC".to_owned(), "spot".to_owned()))
        );
    }
}
