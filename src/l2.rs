//! L2 depth & trades files: every depth update and trade of one instrument for one day.
//!
//! Records (all little-endian, after the 12-byte header `kind, size, time`):
//!
//! * depth update, kind 0, 29 bytes: `price, volume` (`int64`, ×10⁸) and
//!   `flags` (`uint8`: [`BUY`], [`SELL`], [`CLEAR`] the book first,
//!   [`END_OF_TRANSACTION`]);
//! * trade, kind 1, 37 bytes: `id, price, volume` (`int64`) and `side`
//!   (`uint8`, the aggressor: 1 buy, 2 sell, 0 not reported).
//!
//! `volume` of a depth update is the new size at the level, not a change; 0
//! removes the level. Order-log files carry the same day as an aggregated L2
//! view in these two record kinds, so [`Reader`] reads them too.
//!
//! See <https://docs.alphaquick.io/data/l2-depth-trades#format-of-binlz4>.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::container::{i64_at, need, RawRecord, RawRecords};
use crate::error::Result;
use crate::Side;

/// Record kind of a depth update.
pub const KIND_DEPTH: i16 = 0;
/// Record kind of a trade.
pub const KIND_TRADE: i16 = 1;
/// Length of a depth update record.
pub const DEPTH_LEN: usize = 29;
/// Length of a trade record.
pub const TRADE_LEN: usize = 37;

/// Depth flag: the level is a bid.
pub const BUY: u8 = 1;
/// Depth flag: the level is an ask.
pub const SELL: u8 = 2;
/// Depth flag: discard the whole book before applying this update (a fresh snapshot starts here).
pub const CLEAR: u8 = 4;
/// Depth flag: the last update of a batch; the book is consistent after it.
pub const END_OF_TRANSACTION: u8 = 8;

/// One level of the book changed. `volume` is the new size; 0 removes the level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthUpdate {
    /// 100 ns ticks since 0001-01-01 UTC.
    pub time: i64,
    /// Price ×10⁸.
    pub price: i64,
    /// The new size at the level ×10⁸; 0 removes the level.
    pub volume: i64,
    /// [`BUY`], [`SELL`], [`CLEAR`], [`END_OF_TRANSACTION`].
    pub flags: u8,
}

impl DepthUpdate {
    /// The level is a bid ([`BUY`] is set).
    pub fn is_bid(&self) -> bool {
        self.flags & BUY != 0
    }

    /// The side of the level: a bid when [`BUY`] is set, an ask otherwise.
    pub fn side(&self) -> Side {
        if self.is_bid() {
            Side::Bid
        } else {
            Side::Ask
        }
    }

    /// The book is discarded before this update: a fresh snapshot starts here.
    pub fn clears_book(&self) -> bool {
        self.flags & CLEAR != 0
    }

    /// The last update of a batch: the book is consistent after it.
    pub fn ends_transaction(&self) -> bool {
        self.flags & END_OF_TRANSACTION != 0
    }
}

/// One trade print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Trade {
    /// 100 ns ticks since 0001-01-01 UTC.
    pub time: i64,
    /// The venue's trade id, where it provides one.
    pub trade_id: i64,
    /// Price ×10⁸.
    pub price: i64,
    /// Size ×10⁸.
    pub volume: i64,
    /// The aggressor: [`Side::Bid`] bought, [`Side::Ask`] sold, `None` when
    /// the venue does not report it (code 0, or a code the format does not define).
    pub side: Option<Side>,
}

impl Trade {
    /// A market print: price and volume are both positive.
    pub fn is_market(&self) -> bool {
        self.price > 0 && self.volume > 0
    }
}

/// A record of an L2 file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event {
    /// A depth update (kind 0).
    Depth(DepthUpdate),
    /// A trade print (kind 1).
    Trade(Trade),
}

impl Event {
    /// The record time, 100 ns ticks since 0001-01-01 UTC.
    pub fn time(&self) -> i64 {
        match self {
            Event::Depth(update) => update.time,
            Event::Trade(trade) => trade.time,
        }
    }

    /// Decode one raw record; `None` for kinds other than 0 and 1.
    pub fn decode(record: RawRecord<'_>) -> Result<Option<Event>> {
        let b = record.bytes;
        match record.kind {
            KIND_DEPTH => {
                need(&record, DEPTH_LEN)?;
                Ok(Some(Event::Depth(DepthUpdate {
                    time: record.time,
                    price: i64_at(b, 12),
                    volume: i64_at(b, 20),
                    flags: b[28],
                })))
            }
            KIND_TRADE => {
                need(&record, TRADE_LEN)?;
                Ok(Some(Event::Trade(Trade {
                    time: record.time,
                    trade_id: i64_at(b, 12),
                    price: i64_at(b, 20),
                    volume: i64_at(b, 28),
                    side: match b[36] {
                        1 => Some(Side::Bid),
                        2 => Some(Side::Ask),
                        _ => None,
                    },
                })))
            }
            _ => Ok(None),
        }
    }
}

/// Every depth update and trade of an L2 file, in file order.
///
/// Trades whose price or volume is not positive are not market prints and
/// are skipped, unless [`Reader::include_non_market`] is set. Other record
/// kinds (the order-level records of an order-log file, kinds this version
/// does not know) are skipped. After an error the iterator ends.
#[derive(Debug)]
pub struct Reader<R> {
    records: RawRecords<R>,
    include_non_market: bool,
    failed: bool,
}

impl Reader<BufReader<File>> {
    /// Open a file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Reader::from_records(RawRecords::open(path)?))
    }
}

impl<R: Read> Reader<R> {
    /// Read from any byte source.
    pub fn new(reader: R) -> Self {
        Reader::from_records(RawRecords::new(reader))
    }

    fn from_records(records: RawRecords<R>) -> Self {
        Reader {
            records,
            include_non_market: false,
            failed: false,
        }
    }

    /// Also yield trades whose price or volume is not positive.
    pub fn include_non_market(mut self, include: bool) -> Self {
        self.include_non_market = include;
        self
    }
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            let record = match self.records.next_record() {
                Ok(Some(record)) => record,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            match Event::decode(record) {
                Ok(Some(Event::Trade(trade))) if !self.include_non_market && !trade.is_market() => {
                }
                Ok(Some(event)) => return Some(Ok(event)),
                Ok(None) => {}
                Err(error) => {
                    // Nothing after a damaged record is trusted.
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// `(price, size)` of the levels of one side, best first, as [`L2Book::depth`] returns them.
pub type Levels = Vec<(i64, i64)>;

/// A price-level order book rebuilt from depth updates.
///
/// Apply every [`DepthUpdate`] in file order with [`L2Book::apply`]. Prices
/// and sizes stay scaled integers (×10⁸).
///
/// [`L2Book::with_uncross`] removes the opposite levels a new level crosses
/// (asks at or below a new bid, bids at or above a new ask). The format does
/// not need it, but some venues' feeds never delete the levels a move swept,
/// which leaves a crossed book; Depth Replay applies this rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L2Book {
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    uncross: bool,
    consistent: bool,
    time: Option<i64>,
}

impl Default for L2Book {
    /// The same as [`L2Book::new`].
    fn default() -> Self {
        L2Book::new()
    }
}

impl L2Book {
    /// An empty book, without the uncross rule.
    pub fn new() -> Self {
        L2Book {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            uncross: false,
            consistent: true,
            time: None,
        }
    }

    /// The same book with the uncross rule on or off.
    pub fn with_uncross(mut self, uncross: bool) -> Self {
        self.uncross = uncross;
        self
    }

    /// Whether the uncross rule is on.
    pub fn uncrosses(&self) -> bool {
        self.uncross
    }

    /// Apply one update: [`CLEAR`] empties the book first, then the level is
    /// set to `volume` (removed when `volume` is 0 or less).
    pub fn apply(&mut self, update: &DepthUpdate) {
        if update.clears_book() {
            self.clear();
        }
        let bid = update.is_bid();
        let side = if bid { &mut self.bids } else { &mut self.asks };
        if update.volume > 0 {
            side.insert(update.price, update.volume);
            if self.uncross {
                if bid {
                    while let Some((&ask, _)) = self.asks.first_key_value() {
                        if ask > update.price {
                            break;
                        }
                        self.asks.remove(&ask);
                    }
                } else {
                    while let Some((&bid, _)) = self.bids.last_key_value() {
                        if bid < update.price {
                            break;
                        }
                        self.bids.remove(&bid);
                    }
                }
            }
        } else {
            side.remove(&update.price);
        }
        self.consistent = update.ends_transaction();
        self.time = Some(update.time);
    }

    /// Remove every level.
    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }

    /// `(price, size)` of the best bid.
    pub fn best_bid(&self) -> Option<(i64, i64)> {
        self.bids.last_key_value().map(|(&p, &s)| (p, s))
    }

    /// `(price, size)` of the best ask.
    pub fn best_ask(&self) -> Option<(i64, i64)> {
        self.asks.first_key_value().map(|(&p, &s)| (p, s))
    }

    /// Best ask minus best bid, ×10⁸ (saturating at the limits of `i64`).
    pub fn spread(&self) -> Option<i64> {
        Some(self.best_ask()?.0.saturating_sub(self.best_bid()?.0))
    }

    /// The mid price, still scaled by 10⁸ (divide by [`crate::SCALE`] for the price).
    pub fn mid(&self) -> Option<f64> {
        Some((self.best_bid()?.0 as f64 + self.best_ask()?.0 as f64) / 2.0)
    }

    /// The size at a price on one side, 0 when there is no level.
    pub fn size_at(&self, side: Side, price: i64) -> i64 {
        self.side(side).get(&price).copied().unwrap_or(0)
    }

    /// `(bids, asks)`, each `(price, size)` from the best price outwards, at
    /// most `levels` per side (`usize::MAX` for all).
    pub fn depth(&self, levels: usize) -> (Levels, Levels) {
        let bids = self.bids.iter().rev().take(levels).map(|(&p, &s)| (p, s));
        let asks = self.asks.iter().take(levels).map(|(&p, &s)| (p, s));
        (bids.collect(), asks.collect())
    }

    /// The bid levels, price → size, in ascending price order (the best bid is the last).
    pub fn bids(&self) -> &BTreeMap<i64, i64> {
        &self.bids
    }

    /// The ask levels, price → size, in ascending price order (the best ask is the first).
    pub fn asks(&self) -> &BTreeMap<i64, i64> {
        &self.asks
    }

    /// The levels of one side, price → size, in ascending price order.
    pub fn side(&self, side: Side) -> &BTreeMap<i64, i64> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    /// `true` after an update that ends a transaction (and before any update),
    /// `false` in the middle of one.
    pub fn is_consistent(&self) -> bool {
        self.consistent
    }

    /// The time of the last update applied (ticks).
    pub fn time(&self) -> Option<i64> {
        self.time
    }
}

/// The book as it stood at a moment: every depth update of the file up to and
/// including `until` (ticks), with or without the uncross rule.
///
/// Records are in the order they were received, and venue times can step
/// back slightly; this stops at the first record later than `until`. The
/// book may be in the middle of a transaction; check
/// [`L2Book::is_consistent`].
pub fn book_at<R: Read>(reader: R, until: i64, uncross: bool) -> Result<L2Book> {
    let mut book = L2Book::new().with_uncross(uncross);
    for event in Reader::new(reader) {
        let event = event?;
        if event.time() > until {
            break;
        }
        if let Event::Depth(update) = event {
            book.apply(&update);
        }
    }
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use crate::Error;
    use std::io::Cursor;

    fn file_of(records: &[Vec<u8>]) -> Vec<u8> {
        container(&[records.concat()], true)
    }

    fn update(time: i64, price: i64, volume: i64, flags: u8) -> DepthUpdate {
        DepthUpdate {
            time,
            price,
            volume,
            flags,
        }
    }

    #[test]
    fn the_reader_yields_updates_and_trades_in_order() {
        let data = file_of(&[
            depth(T0, 100.0, 1.5, BUY | CLEAR),
            depth(T0 + 1, 101.0, 2.0, SELL | END_OF_TRANSACTION),
            unknown(42, T0 + 2),
            l2_trade(T0 + 3, 100.5, 0.25, 1, 77),
            l2_trade(T0 + 4, 0.0, 1.0, 2, 0), // not a market print
            l2_trade(T0 + 5, 100.0, 1.0, 0, 0),
        ]);
        let events: Vec<Event> = Reader::new(&data[..]).map(|e| e.unwrap()).collect();
        assert_eq!(
            events,
            vec![
                Event::Depth(update(T0, 100 * P, 150_000_000, BUY | CLEAR)),
                Event::Depth(update(T0 + 1, 101 * P, 2 * P, SELL | END_OF_TRANSACTION)),
                Event::Trade(Trade {
                    time: T0 + 3,
                    trade_id: 77,
                    price: 10_050_000_000,
                    volume: 25_000_000,
                    side: Some(Side::Bid),
                }),
                Event::Trade(Trade {
                    time: T0 + 5,
                    trade_id: 0,
                    price: 100 * P,
                    volume: P,
                    side: None,
                }),
            ]
        );
        let Event::Depth(first) = events[0] else {
            panic!()
        };
        assert!(first.is_bid() && first.clears_book() && !first.ends_transaction());
        assert_eq!(first.side(), Side::Bid);
        let every: Vec<Event> = Reader::new(Cursor::new(file_of(&[l2_trade(T0, 0.0, 1.0, 2, 0)])))
            .include_non_market(true)
            .map(|e| e.unwrap())
            .collect();
        assert!(matches!(
            every[..],
            [Event::Trade(Trade {
                price: 0,
                side: Some(Side::Ask),
                ..
            })]
        ));
    }

    #[test]
    fn short_records_are_format_errors() {
        let mut short = depth(T0, 1.0, 1.0, BUY);
        short[2] = 20;
        short.truncate(20);
        let mut reader = Reader::new(Cursor::new(file_of(&[short, depth(T0, 1.0, 1.0, BUY)])));
        assert!(matches!(reader.next(), Some(Err(Error::Format(m))) if m.contains("fewer than")));
        assert!(reader.next().is_none());
    }

    #[test]
    fn book_levels_clear_and_transactions() {
        let mut book = L2Book::new();
        assert!(book.is_consistent() && book.time().is_none());
        let updates = [
            update(1, 100 * P, P, BUY | CLEAR),
            update(2, 99 * P, 2 * P, BUY),
            update(3, 101 * P, 3 * P, SELL),
            update(4, 102 * P, 4 * P, SELL | END_OF_TRANSACTION),
        ];
        for u in &updates[..3] {
            book.apply(u);
        }
        assert!(!book.is_consistent());
        book.apply(&updates[3]);
        assert!(book.is_consistent());
        assert_eq!(book.best_bid(), Some((100 * P, P)));
        assert_eq!(book.best_ask(), Some((101 * P, 3 * P)));
        assert_eq!(book.spread(), Some(P));
        assert_eq!(book.mid(), Some(100.5 * P as f64));
        assert_eq!(
            book.depth(5),
            (
                vec![(100 * P, P), (99 * P, 2 * P)],
                vec![(101 * P, 3 * P), (102 * P, 4 * P)]
            )
        );
        assert_eq!(book.size_at(Side::Ask, 102 * P), 4 * P);
        book.apply(&update(5, 100 * P, 0, BUY | END_OF_TRANSACTION)); // 0 removes the level
        assert_eq!(book.best_bid(), Some((99 * P, 2 * P)));
        book.apply(&update(6, 99 * P, 5 * P, BUY)); // the new size, not a change
        assert_eq!(book.best_bid(), Some((99 * P, 5 * P)));
        book.apply(&update(7, 50 * P, P, BUY | CLEAR)); // a fresh snapshot starts
        assert_eq!(book.depth(usize::MAX), (vec![(50 * P, P)], vec![]));
        assert_eq!(book.time(), Some(7));
        assert_eq!(book.bids().len() + book.asks().len(), 1);
    }

    #[test]
    fn uncross_removes_levels_a_new_level_crosses() {
        let mut plain = L2Book::new();
        let mut uncrossed = L2Book::new().with_uncross(true);
        for book in [&mut plain, &mut uncrossed] {
            for u in [
                update(1, 101 * P, P, SELL),
                update(1, 102 * P, P, SELL),
                update(1, 100 * P, P, BUY),
                update(2, 101 * P, P, BUY),
            ] {
                book.apply(&u);
            }
        }
        assert_eq!(plain.best_ask(), Some((101 * P, P))); // crossed: bid 101, ask 101
        assert_eq!(uncrossed.best_ask(), Some((102 * P, P)));
        assert_eq!(uncrossed.best_bid(), Some((101 * P, P)));
        uncrossed.apply(&update(3, 99 * P, P, SELL));
        assert_eq!(uncrossed.best_bid(), None);
        assert_eq!(uncrossed.best_ask(), Some((99 * P, P)));
        // Removing a level never uncrosses.
        uncrossed.apply(&update(4, 200 * P, 0, BUY));
        assert_eq!(uncrossed.asks().len(), 2);
        assert!(uncrossed.uncrosses() && !plain.uncrosses());
    }

    #[test]
    fn book_at_stops_at_the_moment() {
        let data = container(
            &[
                depth(10, 100.0, 1.0, BUY),
                depth(20, 100.0, 2.0, BUY),
                depth(30, 100.0, 3.0, BUY),
            ],
            true,
        );
        assert_eq!(
            book_at(&data[..], 20, false).unwrap().best_bid(),
            Some((100 * P, 2 * P))
        );
        assert_eq!(book_at(&data[..], 9, false).unwrap().best_bid(), None);
        assert!(book_at(&data[..], 9, true).unwrap().uncrosses());
    }
}
