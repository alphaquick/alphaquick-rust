//! L2 depth & trades files: every depth update and trade of one instrument for one day.
//!
//! Records (all little-endian, after the 12-byte header `kind, size, time`):
//!
//! * depth update, kind 0, 29 bytes: `price, volume` (`int64`, ×10⁸) and
//!   `flags` (`uint8`: [`BUY`], [`SELL`], [`CLEAR`] the book first,
//!   [`END_OF_TRANSACTION`]);
//! * trade, kind 1, 37 bytes: `id, price, volume` (`int64`) and `side`
//!   (`uint8`, the aggressor: 1 buy, 2 sell).
//!
//! `volume` of a depth update is the new size at the level, not a change; 0
//! removes the level. Order-log files carry the same day as an aggregated L2
//! view in these two record kinds, so [`Reader`] reads them too.
//!
//! Prices and sizes are the venue's decimal numbers ×10⁸. Files up to
//! 2026-11-25 hold them to **6 decimal places** (digits beyond 10⁻⁶ were cut
//! off); from 2026-11-26 they are exact.
//!
//! Each data file comes with a continuity file that lists every seed (Clear
//! transaction) and, from 2026-11-26, every gap, where the feed broke and the
//! book is unknown until the next seed. [`Reader::with_continuity`] merges
//! the gaps into the events as [`Event::Gap`], and [`L2Book::apply_event`]
//! then marks the book unknown ([`L2Book::is_known`]); see
//! [`crate::continuity`].
//!
//! See <https://docs.alphaquick.io/data/l2-depth-trades#format-of-binlz4>.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::container::{i64_at, need, RawRecord, RawRecords};
use crate::continuity::{Line, Value};
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
    /// The number of the trade in the file, given by the recorder: `1` for
    /// the file's first trade, then one more for each. It is **not** the
    /// venue's trade id. In files up to 2026-11-25 it starts again at `1`
    /// wherever the recorder was restarted during the day, so it can repeat
    /// within a file; from 2026-11-26 it continues across restarts.
    pub trade_id: i64,
    /// Price ×10⁸.
    pub price: i64,
    /// Size ×10⁸.
    pub volume: i64,
    /// The aggressor: [`Side::Bid`] bought (code 1), [`Side::Ask`] sold
    /// (code 2). The recorders write only these two codes; any other is read
    /// as `None` (not known). `LIGHTER` files up to 2026-11-25 have code 1,
    /// a buy, on every trade, whatever the aggressor was.
    pub side: Option<Side>,
}

impl Trade {
    /// A market print: price and volume are both positive.
    pub fn is_market(&self) -> bool {
        self.price > 0 && self.volume > 0
    }
}

/// Why the feed broke, from a gap line of the continuity file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum GapReason {
    /// `disconnect`: the connection to the venue was lost or closed.
    Disconnect,
    /// `sequence`: the venue's sequence numbers show a lost update.
    Sequence,
    /// `crossed`: dYdX's stream left the book crossed; the recorder
    /// subscribed again for a fresh snapshot.
    Crossed,
    /// `unparsable`: the venue sent a book the format cannot hold.
    Unparsable,
    /// Another reason, for example those of the old node lines
    /// (`sequence_jump`, `legacy_reconnect`, ...); the line itself says which
    /// ([`Gap::line`]).
    Other,
}

impl GapReason {
    /// The reason of a gap line's `reason` text.
    pub fn from_name(name: &str) -> GapReason {
        match name {
            "disconnect" => GapReason::Disconnect,
            "sequence" => GapReason::Sequence,
            "crossed" => GapReason::Crossed,
            "unparsable" => GapReason::Unparsable,
            _ => GapReason::Other,
        }
    }

    /// The name as the continuity file writes it, or `"other"`.
    pub const fn name(self) -> &'static str {
        match self {
            GapReason::Disconnect => "disconnect",
            GapReason::Sequence => "sequence",
            GapReason::Crossed => "crossed",
            GapReason::Unparsable => "unparsable",
            GapReason::Other => "other",
        }
    }
}

impl fmt::Display for GapReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The feed broke: from here to the next Clear the book is not known. Made
/// from a gap line of the continuity file by [`Reader::with_continuity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Gap {
    /// When the recorder noticed it, ticks: the line's `received_at_ms`
    /// (`detected_at_ms` in old node lines). It is the recorder's clock,
    /// while records carry the venue's time where it gives one.
    pub time: i64,
    /// Why.
    pub reason: GapReason,
    /// The index of the gap's line in the lines given to
    /// [`Reader::with_continuity`], for its other fields.
    pub line: usize,
}

/// A record of an L2 file, or a gap merged in from its continuity file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event {
    /// A depth update (kind 0).
    Depth(DepthUpdate),
    /// A trade print (kind 1).
    Trade(Trade),
    /// A gap of the continuity file ([`Reader::with_continuity`]); not a
    /// record of the data file.
    Gap(Gap),
}

impl Event {
    /// The record time, 100 ns ticks since 0001-01-01 UTC (for a gap, its
    /// [`Gap::time`]).
    pub fn time(&self) -> i64 {
        match self {
            Event::Depth(update) => update.time,
            Event::Trade(trade) => trade.time,
            Event::Gap(gap) => gap.time,
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

/// Every depth update and trade of an L2 file, in file order, and the gaps
/// of its continuity file when [`Reader::with_continuity`] is given them.
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
    gaps: Gaps,
    /// Read, and to be returned after the gaps placed before it.
    held: Option<Event>,
}

/// The gaps of a continuity file, waiting for their place among the records.
///
/// Each gap belongs between two Clears: a gap line that follows `n` seed
/// lines (the `n`-th seed line describes the `n`-th Clear) comes after Clear
/// `n` and before Clear `n + 1`; an old node gap line, written after the
/// seed that ends it (its `clear_ordinal`), comes before that seed's Clear.
/// Within that stretch a gap goes before the first record whose time is at
/// or after its own.
#[derive(Debug, Default)]
struct Gaps {
    /// `(Clears before the gap, gap)`, in that order and then by time.
    pending: Vec<(u64, Gap)>,
    next: usize,
    /// Clears read so far.
    clears: u64,
}

impl Gaps {
    fn plan(lines: &[Line]) -> Vec<(u64, Gap)> {
        let mut seed_ordinals: Vec<Option<u64>> = Vec::new();
        let mut pending = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.is_seed() {
                seed_ordinals.push(line.ordinal);
                continue;
            }
            if !line.is_gap() {
                continue;
            }
            let Some(time) = line.time() else { continue };
            let seeds = seed_ordinals.len() as u64;
            let clears_before = match line.get("clear_ordinal").and_then(Value::as_u64) {
                // An old node line, written after the seed that ends the gap.
                Some(clear) => seed_ordinals
                    .iter()
                    .rposition(|&ordinal| ordinal == Some(clear))
                    .map_or(seeds, |at| at as u64),
                None => seeds,
            };
            let gap = Gap {
                time,
                reason: GapReason::from_name(&line.reason),
                line: index,
            };
            pending.push((clears_before, gap));
        }
        pending.sort_by_key(|&(clears, gap)| (clears, gap.time, gap.line));
        pending
    }

    /// The next gap to place before `event`, when one is due.
    fn due_before(&mut self, event: &Event) -> Option<Gap> {
        let &(clears, gap) = self.pending.get(self.next)?;
        let due = match event {
            // Every gap before this Clear goes before it, whatever its time.
            Event::Depth(update) if update.clears_book() => clears <= self.clears,
            _ => clears < self.clears || (clears == self.clears && gap.time <= event.time()),
        };
        if due {
            self.next += 1;
            Some(gap)
        } else {
            None
        }
    }

    fn passed(&mut self, event: &Event) {
        if matches!(event, Event::Depth(update) if update.clears_book()) {
            self.clears += 1;
        }
    }

    /// At the end of the file: the gaps not placed yet.
    fn remaining(&mut self) -> Option<Gap> {
        let &(_, gap) = self.pending.get(self.next)?;
        self.next += 1;
        Some(gap)
    }
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
            gaps: Gaps::default(),
            held: None,
        }
    }

    /// Also yield trades whose price or volume is not positive.
    pub fn include_non_market(mut self, include: bool) -> Self {
        self.include_non_market = include;
        self
    }

    /// Merge the gaps of the data file's continuity file ([`crate::continuity::read`])
    /// into the events, as [`Event::Gap`]. Lines of other kinds are only
    /// used to place the gaps, and gap lines without any time are left out.
    ///
    /// A gap is placed before the first record whose time is at or after the
    /// gap's ([`Gap::time`]), but never after the Clear that follows the gap
    /// in the file nor before the one that precedes it: the `n`-th seed line
    /// of the continuity file describes the `n`-th Clear of the data file, so
    /// a gap line written after `n` seed lines belongs between Clear `n` and
    /// Clear `n + 1` (an old node gap line, written after the seed that ends
    /// it, before that seed's Clear). Gaps not placed by the end of the file
    /// come last. Since records carry the venue's time and a gap the
    /// recorder's, a gap can come a few records early or late within that
    /// stretch; from 2026-11-26 the data file holds no depth updates between
    /// a gap and the next Clear, only trades.
    ///
    /// Call it before reading: Clears already read are counted, but gaps
    /// whose place was passed come at the next record.
    pub fn with_continuity(mut self, lines: &[Line]) -> Self {
        self.gaps.pending = Gaps::plan(lines);
        self.gaps.next = 0;
        self
    }

    /// The book as it stood at a moment (ticks): every depth update of the
    /// file up to `until`, and the gaps up to it, with or without the
    /// uncross rule. See [`book_at`].
    pub fn book_at(self, until: i64, uncross: bool) -> Result<L2Book> {
        let mut book = L2Book::new().with_uncross(uncross);
        for event in self {
            match event? {
                // Only depth updates decide where the book ends: a trade the
                // venue stamped later must not stop it early.
                Event::Depth(update) if update.time > until => break,
                Event::Depth(update) => book.apply(&update),
                Event::Gap(gap) if gap.time <= until => book.apply_gap(&gap),
                _ => {}
            }
        }
        Ok(book)
    }

    /// The next depth update or trade of the file.
    fn next_record(&mut self) -> Option<Result<Event>> {
        loop {
            let record = match self.records.next_record() {
                Ok(Some(record)) => record,
                Ok(None) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
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

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let event = match self.held.take() {
            Some(event) => event,
            None => match self.next_record() {
                Some(Ok(event)) => event,
                Some(Err(error)) => return Some(Err(error)),
                None => return self.gaps.remaining().map(|gap| Ok(Event::Gap(gap))),
            },
        };
        if let Some(gap) = self.gaps.due_before(&event) {
            self.held = Some(event);
            return Some(Ok(Event::Gap(gap)));
        }
        self.gaps.passed(&event);
        Some(Ok(event))
    }
}

/// `(price, size)` of the levels of one side, best first, as [`L2Book::depth`] returns them.
pub type Levels = Vec<(i64, i64)>;

/// A price-level order book rebuilt from depth updates.
///
/// Apply every [`DepthUpdate`] in file order with [`L2Book::apply`] (or every
/// [`Event`] with [`L2Book::apply_event`]). Prices and sizes stay scaled
/// integers (×10⁸).
///
/// The book is the venue's when [`is_consistent`](L2Book::is_consistent)
/// `&&` [`is_known`](L2Book::is_known): at the end of a transaction, and not
/// in a gap. After a [`Gap`] ([`L2Book::apply_gap`]) the data file holds no
/// depth updates until the next Clear, so the levels stay as they were last
/// known, which is out of date; the next Clear makes the book known again.
///
/// [`L2Book::with_uncross`] removes the opposite levels a new level crosses
/// (asks at or below a new bid, bids at or above a new ask). The format does
/// not need it. It is for files whose books keep levels that should have
/// been removed: `BINANCE_USDM` and `ASTER` files up to 2026-11-25 were
/// recorded without the venues' snapshot and sequence procedure, so every
/// level a lost update or a reconnect should have removed stays, and the
/// rebuilt book grows stale levels and is crossed. Uncrossing hides only the
/// crossed part near the touch; stale levels deeper in the book remain, so
/// the depth beyond the touch of those files is not reliable either way.
/// Files from 2026-11-26 are sequence-checked and seeded again after every
/// gap. Depth Replay applies this rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L2Book {
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    uncross: bool,
    consistent: bool,
    time: Option<i64>,
    /// The gap the book is in, since the last Clear.
    gap: Option<Gap>,
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
            gap: None,
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

    /// Apply one update: [`CLEAR`] empties the book first (and ends a gap),
    /// then the level is set to `volume` (removed when `volume` is 0 or less).
    pub fn apply(&mut self, update: &DepthUpdate) {
        if update.clears_book() {
            self.clear();
            self.gap = None;
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

    /// A gap: the book is not known from here to the next Clear
    /// ([`L2Book::is_known`] is `false`). The levels are kept as they were.
    pub fn apply_gap(&mut self, gap: &Gap) {
        if self.gap.is_none() {
            self.gap = Some(*gap);
        }
    }

    /// Apply an event of a [`Reader`]: a depth update ([`L2Book::apply`]) or
    /// a gap ([`L2Book::apply_gap`]); trades leave the book as it is.
    pub fn apply_event(&mut self, event: &Event) {
        match event {
            Event::Depth(update) => self.apply(update),
            Event::Gap(gap) => self.apply_gap(gap),
            Event::Trade(_) => {}
        }
    }

    /// Remove every level. The book stays in its gap, if it is in one: only
    /// a Clear update ends a gap.
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
    /// `false` in the middle of one. See also [`L2Book::is_known`].
    pub fn is_consistent(&self) -> bool {
        self.consistent
    }

    /// `false` from a [`Gap`] to the next Clear update: the feed broke, and
    /// the levels are the last known, out of date. `true` otherwise, also
    /// before the first Clear (a book is not marked unknown without a gap,
    /// and files up to 2026-11-25 have no gaps even where data is missing).
    /// Read the book as the venue's only when `is_consistent() && is_known()`.
    pub fn is_known(&self) -> bool {
        self.gap.is_none()
    }

    /// The gap the book is in (the first since the last Clear), or `None`
    /// when it is known.
    pub fn gap(&self) -> Option<&Gap> {
        self.gap.as_ref()
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
/// back slightly; this stops at the first depth update later than `until`.
/// Only depth updates decide that: trades are stamped from another clock and
/// one stamped later does not stop it. The book may be in the middle of a
/// transaction; check [`L2Book::is_consistent`].
///
/// This reads the data file alone, so it cannot know whether the moment is
/// in a gap. To find out, give the reader the continuity file and check
/// [`L2Book::is_known`]: a book in a gap is the last known one, not the
/// book at the moment.
///
/// ```no_run
/// use alphaquick::{continuity, l2};
/// # let until = 0;
/// let data = "BTCUSDT_BINANCE_USDM_20261126.bin.lz4";
/// let lines = continuity::read(continuity::path_of(data))?;
/// let book = l2::Reader::open(data)?.with_continuity(&lines).book_at(until, false)?;
/// if let Some(gap) = book.gap() {
///     println!("not known: the feed broke ({}), the book is out of date", gap.reason);
/// }
/// # Ok::<(), alphaquick::Error>(())
/// ```
pub fn book_at<R: Read>(reader: R, until: i64, uncross: bool) -> Result<L2Book> {
    Reader::new(reader).book_at(until, uncross)
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

    #[test]
    fn book_at_is_not_stopped_by_a_later_trade() {
        // Trades are stamped from another clock: one stamped after the
        // moment, before depth updates that are not, must not end the book.
        let data = file_of(&[
            depth(10, 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            l2_trade(25, 100.0, 1.0, 1, 1),
            depth(19, 100.0, 2.0, BUY | END_OF_TRANSACTION),
            depth(20, 99.0, 1.0, BUY | END_OF_TRANSACTION),
            depth(21, 98.0, 1.0, BUY | END_OF_TRANSACTION),
        ]);
        let book = book_at(&data[..], 20, false).unwrap();
        assert_eq!(book.best_bid(), Some((100 * P, 2 * P)));
        assert_eq!(book.bids().len(), 2);
        assert_eq!(book.time(), Some(20));
    }

    /// A continuity file: `seed` and `gap` lines at Unix milliseconds.
    fn lines(spec: &[(&str, &str, i64)]) -> Vec<Line> {
        spec.iter()
            .map(|&(kind, reason, ms)| {
                Line::parse(&format!(
                    r#"{{"kind":"{kind}","reason":"{reason}","received_at_ms":{ms},"written_at_ms":{ms}}}"#
                ))
                .unwrap()
            })
            .collect()
    }

    /// Ticks of a Unix millisecond.
    fn ms(ms: i64) -> i64 {
        crate::time::unix_ms_to_ticks(ms)
    }

    /// The events, gaps as `("gap", time)` and records as their kind and time.
    fn sequence(reader: Reader<&[u8]>) -> Vec<(&'static str, i64)> {
        reader
            .map(|event| match event.unwrap() {
                Event::Depth(update) if update.clears_book() => ("clear", update.time),
                Event::Depth(update) => ("depth", update.time),
                Event::Trade(trade) => ("trade", trade.time),
                Event::Gap(gap) => ("gap", gap.time),
            })
            .map(|(kind, time)| (kind, time - ms(0)))
            .collect()
    }

    #[test]
    fn gaps_go_before_the_first_record_at_their_time() {
        let data = file_of(&[
            depth(ms(100), 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(200), 100.0, 2.0, BUY | END_OF_TRANSACTION),
            l2_trade(ms(300), 100.0, 1.0, 1, 1),
            l2_trade(ms(400), 100.0, 1.0, 2, 2),
            depth(ms(500), 101.0, 1.0, SELL | CLEAR),
            depth(ms(500), 100.0, 1.0, BUY | END_OF_TRANSACTION),
            depth(ms(600), 100.0, 3.0, BUY | END_OF_TRANSACTION),
        ]);
        let continuity = lines(&[
            ("seed", "day_roll", 100),
            ("gap", "sequence", 250),
            ("seed", "resync", 500),
        ]);
        let reader = Reader::new(&data[..]).with_continuity(&continuity);
        let t = |ms: i64| ms * 10_000;
        assert_eq!(
            sequence(reader),
            vec![
                ("clear", t(100)),
                ("depth", t(200)),
                ("gap", t(250)),
                ("trade", t(300)),
                ("trade", t(400)),
                ("clear", t(500)),
                ("depth", t(500)),
                ("depth", t(600)),
            ]
        );
        // Without the continuity file: the same records, no gap.
        assert_eq!(sequence(Reader::new(&data[..])).len(), 7);

        // The book is unknown from the gap to the next Clear.
        let mut book = L2Book::new();
        let mut known = Vec::new();
        for event in Reader::new(&data[..]).with_continuity(&continuity) {
            let event = event.unwrap();
            book.apply_event(&event);
            known.push(book.is_known());
            if let Event::Gap(gap) = event {
                assert_eq!((gap.reason, gap.line), (GapReason::Sequence, 1));
                assert_eq!(book.gap(), Some(&gap));
            }
        }
        assert_eq!(known, [true, true, false, false, false, true, true, true]);
        assert!(book.gap().is_none());
        // book_at: in the gap, the last known book, marked unknown.
        let at = |moment| {
            Reader::new(&data[..])
                .with_continuity(&continuity)
                .book_at(ms(moment), false)
                .unwrap()
        };
        let in_gap = at(450);
        assert!(!in_gap.is_known() && in_gap.is_consistent());
        assert_eq!(in_gap.best_bid(), Some((100 * P, 2 * P)));
        assert_eq!(in_gap.gap().unwrap().reason.to_string(), "sequence");
        assert!(at(249).is_known());
        let after = at(500);
        assert!(after.is_known() && after.best_ask().is_some());
    }

    #[test]
    fn a_gap_never_goes_after_the_clear_that_follows_it() {
        // The resync seed is stamped by the venue before the recorder's time
        // of the gap: the gap still goes before it.
        let data = file_of(&[
            depth(ms(100), 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(240), 100.0, 2.0, BUY | END_OF_TRANSACTION),
            depth(ms(245), 100.0, 3.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(400), 100.0, 4.0, BUY | END_OF_TRANSACTION),
        ]);
        let continuity = lines(&[
            ("seed", "snapshot", 100),
            ("gap", "disconnect", 250),
            ("seed", "resync", 260),
        ]);
        let reader = Reader::new(&data[..]).with_continuity(&continuity);
        let t = |ms: i64| ms * 10_000;
        assert_eq!(
            sequence(reader),
            vec![
                ("clear", t(100)),
                ("depth", t(240)),
                ("gap", t(250)),
                ("clear", t(245)),
                ("depth", t(400)),
            ]
        );
    }

    #[test]
    fn a_gap_never_goes_before_the_clear_that_precedes_it() {
        // The seed before the gap is stamped by the venue after the
        // recorder's time of the gap: the gap still goes after it, and
        // before the next record.
        let data = file_of(&[
            depth(ms(100), 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(300), 100.0, 2.0, BUY | CLEAR | END_OF_TRANSACTION),
            l2_trade(ms(310), 100.0, 1.0, 1, 1),
            depth(ms(400), 100.0, 3.0, BUY | CLEAR | END_OF_TRANSACTION),
        ]);
        let continuity = lines(&[
            ("seed", "snapshot", 100),
            ("seed", "snapshot", 300),
            ("gap", "crossed", 290),
            ("seed", "resync", 400),
            ("gap", "unparsable", 900), // after the last record
            ("gap", "new_reason", 950),
        ]);
        let reader = Reader::new(&data[..]).with_continuity(&continuity);
        let t = |ms: i64| ms * 10_000;
        assert_eq!(
            sequence(reader),
            vec![
                ("clear", t(100)),
                ("clear", t(300)),
                ("gap", t(290)),
                ("trade", t(310)),
                ("clear", t(400)),
                ("gap", t(900)),
                ("gap", t(950)),
            ]
        );
        let book = Reader::new(&data[..])
            .with_continuity(&continuity)
            .book_at(i64::MAX, false)
            .unwrap();
        // The first gap since the Clear is the one the book is in.
        assert_eq!(book.gap().unwrap().reason, GapReason::Unparsable);
        let other = GapReason::from_name("new_reason");
        assert_eq!((other, other.name()), (GapReason::Other, "other"));
    }

    #[test]
    fn old_node_gaps_go_before_the_seed_that_ends_them() {
        // The old recorder wrote a gap after the seed that ends it, with that
        // seed's ordinal as clear_ordinal and the time it was detected.
        let data = file_of(&[
            depth(ms(100), 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(200), 100.0, 2.0, BUY | END_OF_TRANSACTION),
            depth(ms(300), 100.0, 3.0, BUY | END_OF_TRANSACTION),
            depth(ms(400), 100.0, 4.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(500), 100.0, 5.0, BUY | END_OF_TRANSACTION),
        ]);
        let text = concat!(
            r#"{"kind":"seed","reason":"snapshot","ordinal":1,"received_at_ms":100}"#,
            "
",
            r#"{"kind":"incident","detected_at_ms":250,"reason":"sequence_jump"}"#,
            "
",
            r#"{"kind":"seed","reason":"snapshot","ordinal":4,"received_at_ms":400}"#,
            "
",
            r#"{"kind":"gap","detected_at_ms":250,"reason":"sequence_jump","last_good_ordinal":3,"clear_ordinal":4,"written_at_ms":401}"#,
            "
",
            r#"{"kind":"no_loss","detected_at_ms":450}"#,
            "
",
        );
        let continuity = crate::continuity::parse(text).unwrap();
        let reader = Reader::new(&data[..]).with_continuity(&continuity);
        let t = |ms: i64| ms * 10_000;
        assert_eq!(
            sequence(reader),
            vec![
                ("clear", t(100)),
                ("depth", t(200)),
                ("gap", t(250)),
                ("depth", t(300)),
                ("clear", t(400)),
                ("depth", t(500)),
            ]
        );
        let gap = Reader::new(&data[..])
            .with_continuity(&continuity)
            .find_map(|e| match e.unwrap() {
                Event::Gap(gap) => Some(gap),
                _ => None,
            })
            .unwrap();
        assert_eq!(gap.reason, GapReason::Other);
        assert_eq!(continuity[gap.line].reason, "sequence_jump");
    }

    #[test]
    fn an_error_ends_the_events_and_the_gaps() {
        let mut data = file_of(&[
            depth(ms(100), 100.0, 1.0, BUY | CLEAR | END_OF_TRANSACTION),
            depth(ms(200), 100.0, 2.0, BUY | END_OF_TRANSACTION),
        ]);
        data.truncate(data.len() - 3);
        let continuity = lines(&[("gap", "disconnect", 900)]);
        let mut reader = Reader::new(&data[..]).with_continuity(&continuity);
        assert!(reader.next().unwrap().is_err());
        assert!(reader.next().is_none());
    }
}
