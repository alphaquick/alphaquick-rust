//! Hyperliquid order-log files (HLC): every order and trade of one instrument for one day.
//!
//! A file holds, in order: a [`Meta`] record (what the file is); for a HIP-4
//! outcome market with both books, an [`Outcome`] record; the resting book at
//! 00:00 UTC as [`SnapOrder`] records in queue order; then each block of the
//! day: a [`Block`] record followed by that block's [`Order`] events and
//! [`Trade`]s. [`User`] records name each account before its first use.
//! Record kinds 0 and 1 (an aggregated L2 view some files carry, read with
//! [`crate::l2::Reader`]) and unknown kinds are skipped by [`Reader`].
//!
//! Inside a block the records come in this order: orders of system accounts
//! placed in the block (251 with [`ORDER_REBUILT`]); the order events that do
//! not remove an order (opens, rejects, `triggered`, a 251 right after a
//! `triggered` whose order starts resting); the block's trades; the
//! reduce-only cap shrinks (252 with [`ORDER_RO_CAP`]), with each order's
//! size after the trades; then the events that remove an order. Corrections
//! against a node snapshot ([`ORDER_SNAPSHOT_FIX`]) come in a BLOCK of their
//! own. Apply the records in file order, as [`Replay`] and
//! [`OrderBook::apply`] do.
//!
//! [`Replay`] reads the records and keeps the order-level [`OrderBook`] up to
//! date, with the format's rules:
//!
//! ```no_run
//! use alphaquick::orderlog::{Record, Replay};
//!
//! let mut replay = Replay::open("xyz_CL_HLC_20261101.bin.lz4")?;
//! let mut trades = 0;
//! while let Some(record) = replay.next_record()? {
//!     // `record` has been applied: the book is as it stands after it.
//!     if let Record::Trade(_) = record {
//!         trades += 1;
//!     }
//! }
//! let book = replay.book();
//! println!("{trades} trades, best bid {:?}, best ask {:?}", book.best_bid(), book.best_ask());
//! # Ok::<(), alphaquick::Error>(())
//! ```
//!
//! See <https://docs.alphaquick.io/data/hyperliquid-order-log#format>.

mod book;
mod codes;
mod replay;

use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::container::{i64_at, need, u16_at, u32_at, u64_at, RawRecord, RawRecords};
use crate::error::{Error, Result};
use crate::outcome::OutcomeInfo;
use crate::Side;

pub use book::{OrderBook, PriceLevel, RestingOrder};
pub use codes::{OrderType, Status, Tif, REJECT_STATUSES, REMOVING_STATUSES};
pub use replay::Replay;

/// Record kind of [`Meta`].
pub const KIND_META: i16 = 16;
/// Record kind of [`Block`].
pub const KIND_BLOCK: i16 = 17;
/// Record kind of [`Order`].
pub const KIND_ORDER: i16 = 18;
/// Record kind of [`Trade`].
pub const KIND_TRADE: i16 = 19;
/// Record kind of [`SnapOrder`].
pub const KIND_SNAPORDER: i16 = 20;
/// Record kind of [`User`].
pub const KIND_USER: i16 = 21;
/// Record kind of [`Outcome`].
pub const KIND_OUTCOME: i16 = 22;

/// The magic at offset 12 of a META record.
pub const MAGIC: [u8; 4] = *b"HLC1";

/// META flag: a HIP-4 outcome market with both books in one file.
pub const META_OUTCOME: u16 = 1;

/// [`Meta::dex`] of spot pairs and HIP-4 outcome markets.
pub const DEX_SPOT: u8 = 254;

/// Order flag: reduce-only.
pub const ORDER_REDUCE_ONLY: u8 = 1;
/// Order flag: a trigger order.
pub const ORDER_TRIGGER: u8 = 2;
/// Order flag: a position TP/SL.
pub const ORDER_POSITION_TPSL: u8 = 4;
/// Order flag, with status 252 ([`Status::CORRECTION_SIZE`]): a reduce-only
/// cap shrink. The exchange shrank a resting reduce-only order so that the
/// account's reduce-only orders on that side total at most its position;
/// Hyperliquid does not log this. Written after the block's trades with the
/// size after them (flags `0x11`). Regular, not a correction.
pub const ORDER_RO_CAP: u8 = 0x10;
/// Order flag, with status 251 ([`Status::CORRECTION_RESTS`]): an order of a
/// Hyperliquid system account whose placement the node never logs, rebuilt
/// from its later events and written first in the block it was placed in.
/// Regular, not a correction.
pub const ORDER_REBUILT: u8 = 0x20;
/// Order flag, with status 250 to 252: a correction where the book rebuilt
/// from the events differed from a snapshot the node took of its own book.
/// Such records come in a BLOCK of their own, numbered and timed as that
/// snapshot, and are rare (none on most days). Apply them like any other.
pub const ORDER_SNAPSHOT_FIX: u8 = 0x40;
/// Order flag (outcome files with both books): the order rests on the second side's book.
pub const ORDER_SECOND_SIDE: u8 = 0x80;

/// Trade flag: the maker order id is set.
pub const TRADE_MAKER_OID: u8 = 1;
/// Trade flag: the taker order id is set.
pub const TRADE_TAKER_OID: u8 = 2;
/// Trade flag: the maker was the buyer.
pub const TRADE_MAKER_IS_BUYER: u8 = 4;
/// Bits 3-4 of the trade flags hold Hyperliquid's direction override (0 none,
/// 1 buy, 2 sell, 3 [a system print](TRADE_DIRECTION_SYSTEM)).
pub const TRADE_DIRECTION_SHIFT: u8 = 3;
/// Value of the direction override (bits 3-4 of the trade flags) for a
/// system print; see [`Trade::is_system_print`].
pub const TRADE_DIRECTION_SYSTEM: u8 = 3;
/// Trade flag: the maker was a TWAP order.
pub const TRADE_MAKER_TWAP: u8 = 32;
/// Trade flag: the taker was a TWAP order.
pub const TRADE_TAKER_TWAP: u8 = 64;
/// Trade flag (outcome files with both books): the trade printed on the
/// second side's book, that is the taker's order is there.
pub const TRADE_SECOND_SIDE: u8 = 0x80;

/// Shortest META record (an empty coin name).
pub const META_MIN_LEN: usize = 45;
/// Length of a BLOCK record.
pub const BLOCK_LEN: usize = 36;
/// Length of an ORDER record.
pub const ORDER_LEN: usize = 45;
/// Length of a TRADE record.
pub const TRADE_LEN: usize = 70;
/// Length of a SNAPORDER record.
pub const SNAPORDER_LEN: usize = 44;
/// Length of a USER record.
pub const USER_LEN: usize = 36;
/// Shortest OUTCOME record (no text).
pub const OUTCOME_MIN_LEN: usize = 14;

/// An account's 20-byte address. `Display` writes `0x` and 40 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub [u8; 20]);

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({self})")
    }
}

/// What the file is. The first record of every file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Meta {
    /// Record time, ticks.
    pub time: i64,
    /// Format version, `1`.
    pub version: u16,
    /// Size decimals of the instrument (`0` in a HIP-4 file).
    pub size_decimals: u8,
    /// Dex index: `0` for the main perpetuals, the deployer's dex for a
    /// HIP-3 market, [`DEX_SPOT`] (254) for spot pairs and HIP-4 outcome
    /// markets.
    pub dex: u8,
    /// Asset index within the dex: the pair index for spot, `65535` in a
    /// HIP-4 file.
    pub asset: u16,
    /// [`META_OUTCOME`].
    pub flags: u16,
    /// The day, as the number `YYYYMMDD`.
    pub day: u32,
    /// The block the day starts from.
    pub start_block: u64,
    /// That block's time, Unix nanoseconds.
    pub start_block_time_ns: u64,
    /// The coin: `BTC`, `xyz:CL`, `@107`, `PURR/USDC`, or `HIP4:<outcome id>`
    /// in a HIP-4 file, whose OUTCOME record ([`Replay::outcome`]) holds the
    /// market's title. (HIP-4 files written before the current build, such
    /// as the free sample of 23 October 2026, hold the title cut to 64 bytes
    /// here, with dex and asset 0.) Invalid UTF-8 is replaced.
    pub coin: String,
}

impl Meta {
    /// A HIP-4 outcome market with both books in one file.
    pub fn is_outcome(&self) -> bool {
        self.flags & META_OUTCOME != 0
    }
}

/// A new block: the records after it, up to the next `Block`, happened in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Block {
    /// Record time: the block time in ticks, to the millisecond.
    pub time: i64,
    /// Block number.
    pub number: u64,
    /// Consensus block time, Unix nanoseconds: the clock to sequence by.
    pub time_ns: u64,
    /// The block time again, Unix nanoseconds: the files carry no time at
    /// which AlphaQuick's node received the block, so to model when the data
    /// could have been seen, add your own feed delay to [`Block::time_ns`].
    /// (In files of the withdrawn first build this was a later time of
    /// receipt.)
    pub local_time_ns: u64,
}

/// An account. Each appears once, before the first record that refers to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct User {
    /// Record time, ticks: the time of the block where the account is first
    /// used, even though the record can be written earlier.
    pub time: i64,
    /// A number local to the file.
    pub user_id: u32,
    /// The account's address.
    pub address: Address,
}

/// One order of the book at 00:00 UTC. The records come in queue order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SnapOrder {
    /// Record time, ticks.
    pub time: i64,
    /// Bid or ask.
    pub side: Side,
    /// Time in force.
    pub tif: Tif,
    /// Order flags ([`ORDER_REDUCE_ONLY`], ..., [`ORDER_SECOND_SIDE`]).
    pub flags: u8,
    /// Order id.
    pub oid: u64,
    /// Price ×10⁸.
    pub price: i64,
    /// Remaining size ×10⁸.
    pub size: i64,
    /// Account id.
    pub user_id: u32,
}

/// An order event: one change of state of one order.
///
/// Statuses 250 to 252 are written by AlphaQuick's converter; their flags say
/// why ([`Order::is_rebuilt`], [`Order::is_ro_cap`],
/// [`Order::is_snapshot_fix`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Order {
    /// Record time (the block time), ticks.
    pub time: i64,
    /// What happened.
    pub status: Status,
    /// Bid or ask.
    pub side: Side,
    /// Time in force.
    pub tif: Tif,
    /// Order type.
    pub order_type: OrderType,
    /// Order flags ([`ORDER_REDUCE_ONLY`], ..., [`ORDER_SECOND_SIDE`]).
    pub flags: u8,
    /// Order id.
    pub oid: u64,
    /// Price ×10⁸.
    pub price: i64,
    /// The order's unfilled size at the event ×10⁸.
    pub size: i64,
    /// Account id.
    pub user_id: u32,
}

impl Order {
    /// A reduce-only cap shrink: status 252 with [`ORDER_RO_CAP`].
    pub fn is_ro_cap(&self) -> bool {
        self.status == Status::CORRECTION_SIZE && self.flags & ORDER_RO_CAP != 0
    }

    /// A rebuilt order of a system account: status 251 with [`ORDER_REBUILT`].
    pub fn is_rebuilt(&self) -> bool {
        self.status == Status::CORRECTION_RESTS && self.flags & ORDER_REBUILT != 0
    }

    /// A correction against a node snapshot: status 250 to 252 with
    /// [`ORDER_SNAPSHOT_FIX`].
    pub fn is_snapshot_fix(&self) -> bool {
        self.status.is_correction() && self.flags & ORDER_SNAPSHOT_FIX != 0
    }
}

/// A fill between a maker and a taker order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Trade {
    /// Record time (the block time), ticks.
    pub time: i64,
    /// [`Side::Bid`]: the taker bought; [`Side::Ask`]: the taker sold.
    pub taker_side: Side,
    /// Trade flags ([`TRADE_MAKER_OID`], ..., [`TRADE_SECOND_SIDE`]).
    pub flags: u8,
    /// Price ×10⁸.
    pub price: i64,
    /// Size ×10⁸.
    pub size: i64,
    /// Maker order id (when [`Trade::has_maker_oid`]).
    pub maker_oid: u64,
    /// Taker order id (when [`Trade::has_taker_oid`]).
    pub taker_oid: u64,
    /// Maker account id.
    pub maker_user_id: u32,
    /// Taker account id.
    pub taker_user_id: u32,
    /// The maker's position before the trade ×10⁸.
    pub maker_position: i64,
    /// The taker's position before the trade ×10⁸.
    pub taker_position: i64,
}

impl Trade {
    /// The maker order id is set.
    pub fn has_maker_oid(&self) -> bool {
        self.flags & TRADE_MAKER_OID != 0
    }

    /// The taker order id is set.
    pub fn has_taker_oid(&self) -> bool {
        self.flags & TRADE_TAKER_OID != 0
    }

    /// The maker was the buyer.
    pub fn maker_is_buyer(&self) -> bool {
        self.flags & TRADE_MAKER_IS_BUYER != 0
    }

    /// Hyperliquid's direction override: 0 none, 1 buy, 2 sell, 3 a system
    /// print ([`Trade::is_system_print`]).
    pub fn direction_override(&self) -> u8 {
        (self.flags >> TRADE_DIRECTION_SHIFT) & 3
    }

    /// A system print: a trade the exchange books without a match in the
    /// book (direction override [`TRADE_DIRECTION_SYSTEM`]).
    ///
    /// These are HIP-4 settlements and outcome actions (mint, merge, negate)
    /// against a system account; in perpetual and spot files, the settlement
    /// of positions when a market is delisted (with status 26
    /// [`Status::DELISTED_CANCELED`] in the same block), lending-pool
    /// liquidations and vault position nettings. The counter-order never
    /// rests in a book, so do not expect to find it there. Hyperliquid's own
    /// candles do not count system prints: leave them out of volume and price
    /// statistics (counts, volume, VWAP, high and low) when you compare them
    /// with the exchange's.
    pub fn is_system_print(&self) -> bool {
        self.direction_override() == TRADE_DIRECTION_SYSTEM
    }

    /// The maker was a TWAP order.
    pub fn maker_is_twap(&self) -> bool {
        self.flags & TRADE_MAKER_TWAP != 0
    }

    /// The taker was a TWAP order.
    pub fn taker_is_twap(&self) -> bool {
        self.flags & TRADE_TAKER_TWAP != 0
    }

    /// Outcome files with both books: the trade printed on the second side's book.
    pub fn on_second_side(&self) -> bool {
        self.flags & TRADE_SECOND_SIDE != 0
    }
}

/// A HIP-4 outcome market's description (outcome files with both books only).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Outcome {
    /// Record time, ticks.
    pub time: i64,
    /// Version of the record, `1`.
    pub version: u16,
    /// UTF-8 text, one `key=value` per line (invalid UTF-8 is replaced).
    pub text: String,
}

impl Outcome {
    /// The description, parsed.
    pub fn info(&self) -> OutcomeInfo {
        OutcomeInfo::parse(&self.text)
    }
}

/// One record of an order-log file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Record {
    /// What the file is (kind 16).
    Meta(Meta),
    /// A new block starts (kind 17).
    Block(Block),
    /// An account (kind 21).
    User(User),
    /// An order of the book at 00:00 UTC (kind 20).
    SnapOrder(SnapOrder),
    /// An order event (kind 18).
    Order(Order),
    /// A trade (kind 19).
    Trade(Trade),
    /// An outcome market's description (kind 22).
    Outcome(Outcome),
}

impl Record {
    /// The time in the record's header, ticks.
    pub fn time(&self) -> i64 {
        match self {
            Record::Meta(r) => r.time,
            Record::Block(r) => r.time,
            Record::User(r) => r.time,
            Record::SnapOrder(r) => r.time,
            Record::Order(r) => r.time,
            Record::Trade(r) => r.time,
            Record::Outcome(r) => r.time,
        }
    }

    /// The record kind (16 to 22).
    pub fn kind(&self) -> i16 {
        match self {
            Record::Meta(_) => KIND_META,
            Record::Block(_) => KIND_BLOCK,
            Record::User(_) => KIND_USER,
            Record::SnapOrder(_) => KIND_SNAPORDER,
            Record::Order(_) => KIND_ORDER,
            Record::Trade(_) => KIND_TRADE,
            Record::Outcome(_) => KIND_OUTCOME,
        }
    }

    /// Decode one raw record; `None` for kinds that are not order-log records
    /// (0, 1 and kinds this version does not know). A record shorter than its
    /// kind needs, or a META record without the `HLC1` magic, is an
    /// [`Error::Format`].
    pub fn decode(record: RawRecord<'_>) -> Result<Option<Record>> {
        let b = record.bytes;
        let time = record.time;
        Ok(Some(match record.kind {
            KIND_ORDER => {
                need(&record, ORDER_LEN)?;
                Record::Order(Order {
                    time,
                    status: Status(b[12]),
                    side: Side::from_code(b[13]),
                    tif: Tif(b[14]),
                    order_type: OrderType(b[15]),
                    flags: b[16],
                    oid: u64_at(b, 17),
                    price: i64_at(b, 25),
                    size: i64_at(b, 33),
                    user_id: u32_at(b, 41),
                })
            }
            KIND_TRADE => {
                need(&record, TRADE_LEN)?;
                Record::Trade(Trade {
                    time,
                    taker_side: Side::from_code(b[12]),
                    flags: b[13],
                    price: i64_at(b, 14),
                    size: i64_at(b, 22),
                    maker_oid: u64_at(b, 30),
                    taker_oid: u64_at(b, 38),
                    maker_user_id: u32_at(b, 46),
                    taker_user_id: u32_at(b, 50),
                    maker_position: i64_at(b, 54),
                    taker_position: i64_at(b, 62),
                })
            }
            KIND_BLOCK => {
                need(&record, BLOCK_LEN)?;
                Record::Block(Block {
                    time,
                    number: u64_at(b, 12),
                    time_ns: u64_at(b, 20),
                    local_time_ns: u64_at(b, 28),
                })
            }
            KIND_SNAPORDER => {
                need(&record, SNAPORDER_LEN)?;
                Record::SnapOrder(SnapOrder {
                    time,
                    side: Side::from_code(b[12]),
                    tif: Tif(b[13]),
                    flags: b[14],
                    oid: u64_at(b, 16),
                    price: i64_at(b, 24),
                    size: i64_at(b, 32),
                    user_id: u32_at(b, 40),
                })
            }
            KIND_USER => {
                need(&record, USER_LEN)?;
                let mut address = [0u8; 20];
                address.copy_from_slice(&b[16..36]);
                Record::User(User {
                    time,
                    user_id: u32_at(b, 12),
                    address: Address(address),
                })
            }
            KIND_META => {
                need(&record, META_MIN_LEN)?;
                if b[12..16] != MAGIC {
                    return Err(Error::format(
                        "META record without the HLC1 magic: not an order-log file",
                    ));
                }
                let coin_len = usize::from(b[44]);
                if META_MIN_LEN + coin_len > b.len() {
                    return Err(Error::format("META coin name runs past the record"));
                }
                Record::Meta(Meta {
                    time,
                    version: u16_at(b, 16),
                    size_decimals: b[18],
                    dex: b[19],
                    asset: u16_at(b, 20),
                    flags: u16_at(b, 22),
                    day: u32_at(b, 24),
                    start_block: u64_at(b, 28),
                    start_block_time_ns: u64_at(b, 36),
                    coin: String::from_utf8_lossy(&b[45..45 + coin_len]).into_owned(),
                })
            }
            KIND_OUTCOME => {
                need(&record, OUTCOME_MIN_LEN)?;
                Record::Outcome(Outcome {
                    time,
                    version: u16_at(b, 12),
                    text: String::from_utf8_lossy(&b[14..]).into_owned(),
                })
            }
            _ => return Ok(None),
        }))
    }
}

/// Every order-log record of a file, in file order.
///
/// Kinds 0 and 1 (the aggregated L2 view) and kinds this version does not
/// know are skipped. After an error the iterator ends.
#[derive(Debug)]
pub struct Reader<R> {
    records: RawRecords<R>,
    failed: bool,
}

impl Reader<BufReader<File>> {
    /// Open a file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Reader {
            records: RawRecords::open(path)?,
            failed: false,
        })
    }
}

impl<R: Read> Reader<R> {
    /// Read from any byte source.
    pub fn new(reader: R) -> Self {
        Reader {
            records: RawRecords::new(reader),
            failed: false,
        }
    }
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            let raw = match self.records.next_record() {
                Ok(Some(raw)) => raw,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            match Record::decode(raw) {
                Ok(Some(record)) => return Some(Ok(record)),
                Ok(None) => {}
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
