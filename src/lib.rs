//! Read AlphaQuick market data files (`.bin.lz4`).
//!
//! Two datasets share one container format:
//!
//! * **L2 depth & trades** (`<INSTRUMENT>_<VENUE>_<YYYYMMDD>.bin.lz4`): every
//!   depth update and trade print of one instrument for one UTC day, from
//!   Binance, Bybit, OKX, Hyperliquid and other venues. Read them with
//!   [`l2::Reader`] and rebuild the price-level book with [`L2Book`].
//! * **Hyperliquid order log** (`<COIN>_HLC_<YYYYMMDD>.bin.lz4`): every order
//!   and every trade of one Hyperliquid instrument for a day, with accounts
//!   and queue positions (Level 3). Read the records with
//!   [`orderlog::Reader`], or let a [`Replay`] keep the order-level
//!   [`OrderBook`] up to date as it reads.
//!
//! Prices and sizes stay integers scaled by [`SCALE`] (10⁸), exactly as the
//! files store them; times are 100 ns ticks since 0001-01-01 UTC (see
//! [`time`]). The crate has one dependency, `lz4_flex`, and no unsafe code.
//!
//! # Quick start
//!
//! An L2 file: every event in file order, and the book at the end.
//!
//! ```no_run
//! use alphaquick::l2::{self, Event};
//! use alphaquick::L2Book;
//!
//! let mut book = L2Book::new();
//! let mut trades = 0;
//! for event in l2::Reader::open("BTC_HYPERLIQUID_20261101.bin.lz4")? {
//!     match event? {
//!         Event::Depth(update) => book.apply(&update),
//!         Event::Trade(_) => trades += 1,
//!         _ => {} // kinds a later version may add
//!     }
//! }
//! println!("{trades} trades, best bid {:?}, best ask {:?}", book.best_bid(), book.best_ask());
//! # Ok::<(), alphaquick::Error>(())
//! ```
//!
//! An order-log file: the order book at 00:15 UTC, and the queue at the best bid.
//!
//! ```no_run
//! use alphaquick::time::{UtcDateTime, TICKS_PER_SECOND};
//! use alphaquick::{Replay, Side};
//!
//! let mut replay = Replay::open("ZEC_HLC_20261014_sample.bin.lz4")?;
//! let day = UtcDateTime::from_yyyymmdd(20261014).unwrap().to_ticks();
//! replay.run_until(day + 15 * 60 * TICKS_PER_SECOND)?;
//!
//! let book = replay.book();
//! if let Some(best) = book.best_bid() {
//!     for order in book.queue(Side::Bid, best).take(3) {
//!         let address = replay.address(order.user_id).map(|a| a.to_string());
//!         println!("{} {:?}", alphaquick::time::format_fixed(order.size), address);
//!     }
//! }
//! replay.run()?; // on to the end of the file
//! # Ok::<(), alphaquick::Error>(())
//! ```
//!
//! The format documentation:
//! <https://docs.alphaquick.io/data/l2-depth-trades> and
//! <https://docs.alphaquick.io/data/hyperliquid-order-log>.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod container;
mod error;
pub mod l2;
pub mod names;
pub mod orderlog;
pub mod outcome;
pub mod time;

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod fuzz;

// Compile the README's code blocks as doc tests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

use std::fmt;
use std::io::Read;

pub use error::{Error, Result};
pub use l2::L2Book;
pub use orderlog::{OrderBook, Replay};
pub use time::{SCALE, TICKS_AT_UNIX_EPOCH};

/// A side of the book: bid (buy) or ask (sell).
///
/// The order log writes `1` for a bid and `2` for an ask; any other code is
/// read as an ask, as the reference readers do. In a trade, the taker's side
/// [`Side::Bid`] means the taker bought.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    /// A bid: a buy order, or a taker that bought.
    Bid,
    /// An ask: a sell order, or a taker that sold.
    Ask,
}

impl Side {
    /// The side of a format code: `1` is a bid, anything else an ask.
    pub const fn from_code(code: u8) -> Side {
        if code == 1 {
            Side::Bid
        } else {
            Side::Ask
        }
    }

    /// The format code: `1` for a bid, `2` for an ask.
    pub const fn code(self) -> u8 {
        match self {
            Side::Bid => 1,
            Side::Ask => 2,
        }
    }

    /// The other side.
    pub const fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }

    /// `true` for [`Side::Bid`].
    pub const fn is_bid(self) -> bool {
        matches!(self, Side::Bid)
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Bid => "bid",
            Side::Ask => "ask",
        })
    }
}

/// Which of the two datasets a file belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileKind {
    /// L2 depth & trades.
    L2,
    /// Hyperliquid order log.
    OrderLog,
}

/// Which dataset a file holds, from its first record; `None` for a file
/// without records.
///
/// An order-log file starts with its META record (kind 16), an L2 file with
/// a depth update or a trade (kind 0 or 1). Any other first record is a
/// [`Error::Format`]. To tell from the file name alone, use
/// [`names::file_kind`].
pub fn detect<R: Read>(reader: R) -> Result<Option<FileKind>> {
    let mut records = container::RawRecords::new(reader);
    match records.next_record()? {
        None => Ok(None),
        Some(record) => match record.kind {
            orderlog::KIND_META => Ok(Some(FileKind::OrderLog)),
            l2::KIND_DEPTH | l2::KIND_TRADE => Ok(Some(FileKind::L2)),
            kind => Err(Error::Format(format!(
                "the file starts with a record of kind {kind}, which is neither format's first record"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    #[test]
    fn detect_reads_the_first_record() {
        let l2 = container(&[depth(T0, 1.0, 1.0, 1)], true);
        assert_eq!(detect(&l2[..]).unwrap(), Some(FileKind::L2));
        let hlc = container(&[meta("BTC", 0)], true);
        assert_eq!(detect(&hlc[..]).unwrap(), Some(FileKind::OrderLog));
        assert_eq!(detect(&b""[..]).unwrap(), None);
        let odd = container(&[unknown(42, T0)], true);
        assert!(matches!(detect(&odd[..]), Err(Error::Format(_))));
    }

    #[test]
    fn sides() {
        assert_eq!(Side::from_code(1), Side::Bid);
        assert_eq!(Side::from_code(2), Side::Ask);
        assert_eq!(Side::from_code(9), Side::Ask);
        assert_eq!(Side::Bid.opposite().code(), 2);
        assert_eq!(Side::Ask.to_string(), "ask");
    }
}
