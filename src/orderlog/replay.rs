//! Records applied in order, with the state a reader needs.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use super::{Address, Block, Meta, OrderBook, Reader, Record, Trade};
use crate::error::Result;
use crate::outcome::{trade_kind, OutcomeInfo, TradeKind};

/// Read an order-log file and keep its [`OrderBook`] up to date.
///
/// Each record is applied to the book as it is read. Alongside the book the
/// replay keeps the file's [`Meta`], the [`OutcomeInfo`] of an outcome file
/// with both books, the accounts seen so far, the current [`Block`] and, for
/// each trade of an outcome market, its [`TradeKind`] (read from the book
/// before the trade changed it).
///
/// A replay runs on in steps: [`Replay::run_until`], then a later
/// `run_until`, [`Replay::run`] or [`Replay::next_record`], each continue
/// where the previous call stopped. Nothing is read twice or skipped, and the
/// end state is that of a single full run.
///
/// ```no_run
/// use alphaquick::orderlog::{Record, Replay};
/// use alphaquick::outcome::TradeKind;
///
/// let mut replay = Replay::open("HIP4_3600_Fed-Oct-no-change_HLC_20261023.bin.lz4")?;
/// let mut mints = 0;
/// while let Some(record) = replay.next_record()? {
///     if let Record::Trade(_) = record {
///         if replay.last_trade_kind() == Some(TradeKind::Mint) {
///             mints += 1;
///         }
///     }
/// }
/// println!("{mints} mints");
/// # Ok::<(), alphaquick::Error>(())
/// ```
///
/// ## Removals written before a trade
///
/// The trade kind needs the maker order's flags. A file may write an
/// order's removal (for example `canceled`) before a trade of that order in
/// the same block, so the replay keeps the flags of the orders removed in
/// the current block and uses them when the maker is no longer in the book.
/// The book itself is applied in file order.
#[derive(Debug)]
pub struct Replay<R> {
    reader: Reader<R>,
    book: OrderBook,
    meta: Option<Meta>,
    outcome: Option<OutcomeInfo>,
    accounts: HashMap<u32, Address>,
    block: Option<Block>,
    last_trade_kind: Option<TradeKind>,
    outcome_file: bool,
    /// Read but not applied: the BLOCK at which `run_until` stopped.
    pending: Option<Record>,
    /// Order id → flags of the orders removed in the current block.
    removed_in_block: HashMap<u64, u8>,
}

impl Replay<BufReader<File>> {
    /// Open a file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Replay::from_reader(Reader::open(path)?))
    }
}

impl<R: Read> Replay<R> {
    /// Replay from any byte source.
    pub fn new(reader: R) -> Self {
        Replay::from_reader(Reader::new(reader))
    }

    /// Replay the records of a [`Reader`].
    pub fn from_reader(reader: Reader<R>) -> Self {
        Replay {
            reader,
            book: OrderBook::new(),
            meta: None,
            outcome: None,
            accounts: HashMap::new(),
            block: None,
            last_trade_kind: None,
            outcome_file: false,
            pending: None,
            removed_in_block: HashMap::new(),
        }
    }

    /// Read the next record, apply it, and return it; `None` at the end of
    /// the file. While the record is in your hands, [`Replay::book`] is the
    /// book as it stands after it.
    pub fn next_record(&mut self) -> Result<Option<Record>> {
        let Some(record) = self.read()? else {
            return Ok(None);
        };
        self.apply(&record);
        Ok(Some(record))
    }

    /// Read on to the end of the file.
    pub fn run(&mut self) -> Result<()> {
        while let Some(record) = self.read()? {
            self.apply(&record);
        }
        Ok(())
    }

    /// Read on up to a moment (ticks, see [`crate::time`]): stop before the
    /// first BLOCK record later than `until`, other than the first BLOCK of
    /// the file. The book is then as it stood at that moment.
    ///
    /// Files start with META, USER records and the seed BLOCK the day starts
    /// from, followed by the day-start SNAPORDERs; the first BLOCK and
    /// everything up to the next one is always applied, so a moment before
    /// the day starts gives the day-start book, never an empty one.
    ///
    /// Blocks are the clock: other records can carry other times (a USER
    /// record, written before the account's first use, keeps the time of the
    /// block it first appears in), so they are always applied. The BLOCK
    /// read past the moment is kept and applied first by the next call.
    pub fn run_until(&mut self, until: i64) -> Result<()> {
        while let Some(record) = self.read()? {
            // The first block of the file is the seed block the day starts
            // from, and the day-start book follows it: never stop there.
            let later_block = matches!(record, Record::Block(ref block) if block.time > until);
            if later_block && self.block.is_some() {
                self.pending = Some(record);
                break;
            }
            self.apply(&record);
        }
        Ok(())
    }

    fn read(&mut self) -> Result<Option<Record>> {
        if let Some(record) = self.pending.take() {
            return Ok(Some(record));
        }
        self.reader.next().transpose()
    }

    fn apply(&mut self, record: &Record) {
        match record {
            Record::Order(order) => {
                if self.outcome_file && order.status.removes_order() {
                    if let Some(gone) = self.book.get(order.oid) {
                        self.removed_in_block.insert(gone.oid, gone.flags);
                    }
                }
                self.book.apply_order(order);
            }
            Record::Trade(trade) => {
                if self.outcome_file {
                    let maker = self.maker_flags(trade);
                    self.last_trade_kind = Some(trade_kind(trade.flags, trade.taker_side, maker));
                }
                self.book.apply_trade(trade);
            }
            Record::Block(block) => {
                self.block = Some(*block);
                self.removed_in_block.clear();
            }
            Record::SnapOrder(snap) => self.book.apply_snap(snap),
            Record::User(user) => {
                self.accounts.insert(user.user_id, user.address);
            }
            Record::Meta(meta) => {
                self.outcome_file = meta.is_outcome();
                self.meta = Some(meta.clone());
            }
            Record::Outcome(outcome) => self.outcome = Some(outcome.info()),
        }
    }

    fn maker_flags(&self, trade: &Trade) -> Option<u8> {
        if !trade.has_maker_oid() {
            return None;
        }
        match self.book.get(trade.maker_oid) {
            Some(maker) => Some(maker.flags),
            None => self.removed_in_block.get(&trade.maker_oid).copied(),
        }
    }
}

impl<R> Replay<R> {
    /// The book as it stands.
    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    /// The book, giving up the replay.
    pub fn into_book(self) -> OrderBook {
        self.book
    }

    /// The file's META record, once read.
    pub fn meta(&self) -> Option<&Meta> {
        self.meta.as_ref()
    }

    /// `true` once a META record says the file holds both books of a HIP-4
    /// outcome market.
    pub fn is_outcome(&self) -> bool {
        self.outcome_file
    }

    /// The outcome market's description, once its OUTCOME record is read.
    pub fn outcome(&self) -> Option<&OutcomeInfo> {
        self.outcome.as_ref()
    }

    /// The accounts seen so far: account id → address.
    pub fn accounts(&self) -> &HashMap<u32, Address> {
        &self.accounts
    }

    /// The address of an account id, or `None` when it has not been seen.
    pub fn address(&self, user_id: u32) -> Option<Address> {
        self.accounts.get(&user_id).copied()
    }

    /// The current block: the last BLOCK record applied.
    pub fn block(&self) -> Option<&Block> {
        self.block.as_ref()
    }

    /// What the last trade applied did, in an outcome file with both books;
    /// `None` in other files and before the first trade.
    pub fn last_trade_kind(&self) -> Option<TradeKind> {
        self.last_trade_kind
    }
}

impl<R: Read> Iterator for Replay<R> {
    type Item = Result<Record>;

    /// The same as [`Replay::next_record`]. A `for` loop holds the replay, so
    /// to look at the book inside the loop use `while let Some(record) =
    /// replay.next_record()?` instead.
    fn next(&mut self) -> Option<Self::Item> {
        self.next_record().transpose()
    }
}
