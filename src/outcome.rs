//! HIP-4 outcome markets (Hyperliquid's prediction markets) in an order-log file.
//!
//! An outcome has two sides, each a token with its own order book, and the
//! two books trade with each other: a buy of the first side at `p` meets a
//! buy of the second at `1 − p` and creates a pair (a *mint*); a sell of each
//! redeems one (a *merge*).
//!
//! A file with both books of one outcome (META flag
//! [`crate::orderlog::META_OUTCOME`]) is written in the **first side's
//! terms**: every price is the first side's price, as a share of 1. An order
//! of the second side's book carries [`crate::orderlog::ORDER_SECOND_SIDE`]
//! and is written with its side flipped and its price as `1 − price`
//! ([`as_placed`] undoes that). A trade that printed on the second side's
//! book carries [`crate::orderlog::TRADE_SECOND_SIDE`]. [`trade_kind`] tells
//! what a trade did.
//!
//! See <https://docs.alphaquick.io/data/hyperliquid-order-log#outcome-markets>.

use std::fmt;

use crate::orderlog::{ORDER_SECOND_SIDE, TRADE_SECOND_SIDE};
use crate::time::SCALE;
use crate::Side;

/// One whole unit of collateral per pair, in the format's fixed point.
pub const ONE: i64 = SCALE;

/// What an OUTCOME record says about the market. Every key is optional;
/// keys this version does not know are ignored.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct OutcomeInfo {
    /// The outcome id (the coin `#N` of one side is the id times ten plus the side).
    pub outcome: Option<u64>,
    /// The market as Hyperliquid's app names it; `"Outcome <id>"` or
    /// `"Outcome market"` when the record has no title.
    pub title: String,
    /// Names of the first and the second side; `"Yes"` and `"No"` when not given.
    pub sides: [String; 2],
    /// The question the outcome answers, when it is one of several.
    pub question_id: Option<u64>,
    /// The text of that question.
    pub question: String,
    /// The template the market was registered from.
    pub template: String,
    /// The template's fields, in order (`key:value` pairs joined by `|` in the record).
    pub fields: Vec<(String, String)>,
    /// Who listed it.
    pub venue: String,
    /// The deployer's address.
    pub deployer: String,
    /// The first side's settlement value (1, 0 or 0.5), when the market had
    /// settled by the time the file was written.
    pub settled: Option<f64>,
    /// What the settlement says.
    pub settle_detail: String,
}

impl OutcomeInfo {
    /// Read the text of an OUTCOME record: one `key=value` per line. Lines
    /// without `=` and unknown keys are ignored.
    pub fn parse(text: &str) -> Self {
        let mut info = OutcomeInfo {
            outcome: None,
            title: String::new(),
            sides: [String::new(), String::new()],
            question_id: None,
            question: String::new(),
            template: String::new(),
            fields: Vec::new(),
            venue: String::new(),
            deployer: String::new(),
            settled: None,
            settle_detail: String::new(),
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "outcome" => info.outcome = value.parse().ok(),
                "title" => info.title = value.to_owned(),
                "side0" => info.sides[0] = value.to_owned(),
                "side1" => info.sides[1] = value.to_owned(),
                "question_id" => info.question_id = value.parse().ok(),
                "question" => info.question = value.to_owned(),
                "template" => info.template = value.to_owned(),
                "fields" => {
                    info.fields = value
                        .split('|')
                        .filter_map(|pair| pair.split_once(':'))
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect();
                }
                "venue" => info.venue = value.to_owned(),
                "deployer" => info.deployer = value.to_owned(),
                "settled" => {
                    info.settled = value.parse::<f64>().ok().filter(|v| v.is_finite());
                }
                "settle_detail" => info.settle_detail = value.to_owned(),
                _ => {}
            }
        }
        for (side, default) in info.sides.iter_mut().zip(["Yes", "No"]) {
            if side.is_empty() {
                *side = default.to_owned();
            }
        }
        if info.title.is_empty() {
            info.title = match info.outcome {
                Some(id) => format!("Outcome {id}"),
                None => "Outcome market".to_owned(),
            };
        }
        info
    }

    /// The name of the first (`false`) or the second (`true`) side.
    pub fn side_name(&self, second: bool) -> &str {
        &self.sides[usize::from(second)]
    }

    /// A field of the template, e.g. `time`.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// An order of an outcome file as it was placed: `(on_second_book, side, price)`.
///
/// Outcome files store every order in the first side's terms: a buy of the
/// second side at `q` is written as an ask at `1 − q` with
/// [`ORDER_SECOND_SIDE`]. This undoes that.
///
/// ```
/// use alphaquick::outcome::as_placed;
/// use alphaquick::orderlog::ORDER_SECOND_SIDE;
/// use alphaquick::Side;
///
/// // A buy of "No" at 0.07698, written as an ask at 0.92302:
/// assert_eq!(as_placed(Side::Ask, 92_302_000, ORDER_SECOND_SIDE), (true, Side::Bid, 7_698_000));
/// ```
pub fn as_placed(side: Side, price: i64, flags: u8) -> (bool, Side, i64) {
    if flags & ORDER_SECOND_SIDE != 0 {
        (true, side.opposite(), ONE.saturating_sub(price))
    } else {
        (false, side, price)
    }
}

/// What a trade of a HIP-4 outcome market did; see [`trade_kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TradeKind {
    /// Both orders on the first side's book: tokens changed hands.
    FirstBook = 1,
    /// Both orders on the second side's book.
    SecondBook = 2,
    /// A buy on each book: a new pair of tokens was created.
    Mint = 3,
    /// A sell on each book: a pair was redeemed for its collateral.
    Merge = 4,
    /// The maker order was not in the book, so its book is not known.
    Unknown = 5,
}

impl TradeKind {
    /// Every kind, in the order of their codes.
    pub const ALL: [TradeKind; 5] = [
        TradeKind::FirstBook,
        TradeKind::SecondBook,
        TradeKind::Mint,
        TradeKind::Merge,
        TradeKind::Unknown,
    ];

    /// A short name: `"first book"`, `"second book"`, `"mint"`, `"merge"`, `"unknown"`.
    pub const fn name(self) -> &'static str {
        match self {
            TradeKind::FirstBook => "first book",
            TradeKind::SecondBook => "second book",
            TradeKind::Mint => "mint",
            TradeKind::Merge => "merge",
            TradeKind::Unknown => "unknown",
        }
    }
}

impl fmt::Display for TradeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a trade of an outcome file did.
///
/// `trade_flags` and `taker_side` are the TRADE record's; `maker_flags` are
/// the maker order's flags as it rested before the trade (`None` when it is
/// not known). Both orders on one book exchanged tokens; a taker and a maker
/// on different books minted a pair when the taker's order, as placed, was a
/// buy, and merged one when it was a sell.
/// [`crate::orderlog::Replay::last_trade_kind`] applies this to every trade.
pub fn trade_kind(trade_flags: u8, taker_side: Side, maker_flags: Option<u8>) -> TradeKind {
    let Some(maker_flags) = maker_flags else {
        return TradeKind::Unknown;
    };
    let taker_second = trade_flags & TRADE_SECOND_SIDE != 0;
    let maker_second = maker_flags & ORDER_SECOND_SIDE != 0;
    match (taker_second, maker_second) {
        (false, false) => TradeKind::FirstBook,
        (true, true) => TradeKind::SecondBook,
        _ => {
            let placed = if taker_second {
                taker_side.opposite()
            } else {
                taker_side
            };
            if placed == Side::Bid {
                TradeKind::Mint
            } else {
                TradeKind::Merge
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const TEXT: &str = "outcome=3600\ntitle=Fed decision \u{2014} No change\nside0=Yes\nside1=No\nquestion_id=289\nfields=time:1792713600|price:80\nsettled=1\nnot a pair\nfuture_key=1";

    #[test]
    fn outcome_info_parses_and_defaults() {
        let info = OutcomeInfo::parse(TEXT);
        assert_eq!(info.outcome, Some(3600));
        assert_eq!(info.title, "Fed decision \u{2014} No change");
        assert_eq!(info.sides, ["Yes".to_string(), "No".to_string()]);
        assert_eq!((info.question_id, info.settled), (Some(289), Some(1.0)));
        assert_eq!(
            info.fields,
            vec![
                ("time".into(), "1792713600".into()),
                ("price".into(), "80".into())
            ]
        );
        assert_eq!(info.field("price"), Some("80"));
        assert_eq!(info.side_name(true), "No");
        let empty = OutcomeInfo::parse("outcome=12\nsettled=nan");
        assert_eq!((empty.title.as_str(), empty.settled), ("Outcome 12", None));
        assert_eq!(empty.sides, ["Yes".to_string(), "No".to_string()]);
        assert_eq!(OutcomeInfo::parse("").title, "Outcome market");
        let odd = OutcomeInfo::parse("title=a = b\nfields=feed:https://x.io/a:b|threshold:5");
        assert_eq!(odd.title, "a = b");
        assert_eq!(odd.field("feed"), Some("https://x.io/a:b"));
    }

    #[test]
    fn as_placed_undoes_the_first_side_terms() {
        assert_eq!(
            as_placed(Side::Ask, 92_302_000, ORDER_SECOND_SIDE),
            (true, Side::Bid, 7_698_000)
        );
        assert_eq!(
            as_placed(Side::Bid, 55_000_000, ORDER_SECOND_SIDE),
            (true, Side::Ask, 45_000_000)
        );
        assert_eq!(
            as_placed(Side::Bid, 60_000_000, 0),
            (false, Side::Bid, 60_000_000)
        );
    }

    #[test]
    fn two_buys_across_the_books_mint_two_sells_merge() {
        use TradeKind::*;
        assert_eq!(trade_kind(0, Side::Bid, Some(ORDER_SECOND_SIDE)), Mint);
        assert_eq!(trade_kind(0, Side::Ask, Some(ORDER_SECOND_SIDE)), Merge);
        assert_eq!(trade_kind(TRADE_SECOND_SIDE, Side::Ask, Some(0)), Mint);
        assert_eq!(trade_kind(TRADE_SECOND_SIDE, Side::Bid, Some(0)), Merge);
        assert_eq!(trade_kind(0, Side::Bid, Some(0)), FirstBook);
        assert_eq!(
            trade_kind(
                TRADE_SECOND_SIDE | 1,
                Side::Ask,
                Some(ORDER_SECOND_SIDE | 1)
            ),
            SecondBook
        );
        assert_eq!(trade_kind(0, Side::Bid, None), Unknown);
        assert_eq!(Mint.to_string(), "mint");
        assert_eq!(TradeKind::ALL.len(), 5);
    }
}
