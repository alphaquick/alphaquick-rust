//! The order-level (L3) book.

use std::collections::{btree_map, BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};

use super::{Order, Record, SnapOrder, Status, Tif, Trade};
use crate::Side;

/// A fast hash for the integer keys of the book (order ids). The keys come
/// from the file, not from an adversary over a network, so a multiplicative
/// hash is enough and much faster than the default SipHash.
#[derive(Default, Clone, Copy)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }
}

type IdMap<V> = HashMap<u64, V, BuildHasherDefault<IdHasher>>;

/// An order in the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RestingOrder {
    /// Order id.
    pub oid: u64,
    /// Bid or ask.
    pub side: Side,
    /// Price ×10⁸.
    pub price: i64,
    /// Remaining size ×10⁸.
    pub size: i64,
    /// Account id.
    pub user_id: u32,
    /// The order's flags, as its record carries them (e.g.
    /// [`super::ORDER_SECOND_SIDE`] in an outcome file).
    pub flags: u8,
    /// Arrival number: lower is earlier, so further ahead in its price's queue.
    pub seq: u64,
}

/// One price of one side, as [`OrderBook::depth`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PriceLevel {
    /// Price ×10⁸.
    pub price: i64,
    /// Total remaining size of the orders at the price ×10⁸.
    pub size: i64,
    /// Number of orders at the price with a size above 0.
    pub orders: usize,
}

/// The levels of one side in either direction, without boxing.
enum BestFirst<'a> {
    Up(btree_map::Iter<'a, i64, Level>),
    Down(std::iter::Rev<btree_map::Iter<'a, i64, Level>>),
}

impl<'a> Iterator for BestFirst<'a> {
    type Item = (&'a i64, &'a Level);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            BestFirst::Up(iter) => iter.next(),
            BestFirst::Down(iter) => iter.next(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Level {
    total: i64,
    /// Orders at the price with a size above 0.
    live: usize,
    /// Arrival number → order id, so the front of the queue comes first.
    queue: BTreeMap<u64, u64>,
}

/// The order-level (L3) book: every resting order, in queue order at each price.
///
/// No input makes it panic: level totals wrap around on overflow (they stay
/// the exact sum of the order sizes, modulo 2⁶⁴), which only nonsense sizes
/// near `i64::MAX` can cause.
///
/// Apply SNAPORDER, ORDER and TRADE records in file order with
/// [`OrderBook::apply`] ([`super::Replay`] does this for you). The rules are
/// the format's:
///
/// * SNAPORDER adds an order; the records come in queue order.
/// * ORDER `open` with a time in force other than [`Tif::NONE`] adds an order
///   at the back of its price (an open with `NONE` places a trigger order,
///   which waits off the book); correction 251 adds one; correction 252 sets
///   the remaining size and keeps the place in the queue.
/// * ORDER with a status that [removes the order](Status::removes_order)
///   removes it.
/// * TRADE reduces the maker and the taker order, where each rests and its
///   id is set. This is the only record of a partial fill.
/// * Everything else (rejects, `triggered`) leaves the book as it is.
///
/// Adding an order whose id is already in the book replaces it: the order
/// goes to the back of its (new) price. An order filled down to zero stays
/// listed with size 0 until the event that removes it, which follows in the
/// same block; prices whose orders are all at size 0 are not reported by
/// [`best_bid`](OrderBook::best_bid), [`best_ask`](OrderBook::best_ask) or
/// [`depth`](OrderBook::depth).
#[derive(Clone, Debug, Default)]
pub struct OrderBook {
    orders: IdMap<RestingOrder>,
    bids: BTreeMap<i64, Level>,
    asks: BTreeMap<i64, Level>,
    seq: u64,
}

impl OrderBook {
    /// An empty book.
    pub fn new() -> Self {
        OrderBook::default()
    }

    /// Number of orders in the book.
    pub fn len(&self) -> usize {
        self.orders.len()
    }

    /// `true` when no order rests.
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Number of orders on one side, including any filled down to size 0
    /// whose removal has not been applied yet.
    pub fn order_count(&self, side: Side) -> usize {
        self.levels_of(side).values().map(|l| l.queue.len()).sum()
    }

    /// A resting order by id.
    pub fn get(&self, oid: u64) -> Option<&RestingOrder> {
        self.orders.get(&oid)
    }

    /// `true` when the order rests in the book.
    pub fn contains(&self, oid: u64) -> bool {
        self.orders.contains_key(&oid)
    }

    /// Every resting order, in no particular order.
    pub fn orders(&self) -> impl Iterator<Item = &RestingOrder> + '_ {
        self.orders.values()
    }

    // -- changes -------------------------------------------------------------

    /// Apply one record. Records that do not change the book are ignored.
    pub fn apply(&mut self, record: &Record) {
        match record {
            Record::SnapOrder(snap) => self.apply_snap(snap),
            Record::Order(order) => self.apply_order(order),
            Record::Trade(trade) => self.apply_trade(trade),
            _ => {}
        }
    }

    /// Apply a SNAPORDER: the order is added at the back of its price.
    pub fn apply_snap(&mut self, snap: &SnapOrder) {
        self.insert(
            snap.oid,
            snap.side,
            snap.price,
            snap.size,
            snap.user_id,
            snap.flags,
        );
    }

    /// Apply an ORDER event.
    pub fn apply_order(&mut self, order: &Order) {
        let status = order.status;
        if status == Status::CORRECTION_SIZE {
            self.resize(order.oid, order.size);
        } else if (status == Status::OPEN && order.tif != Tif::NONE)
            || status == Status::CORRECTION_RESTS
        {
            self.insert(
                order.oid,
                order.side,
                order.price,
                order.size,
                order.user_id,
                order.flags,
            );
        } else if status.removes_order() {
            self.remove(order.oid);
        }
    }

    /// Apply a TRADE: the maker and the taker order lose its size.
    pub fn apply_trade(&mut self, trade: &Trade) {
        if trade.has_maker_oid() {
            self.reduce(trade.maker_oid, trade.size);
        }
        if trade.has_taker_oid() {
            self.reduce(trade.taker_oid, trade.size);
        }
    }

    /// Add an order at the back of its price. An order already there with
    /// this id is replaced.
    pub fn insert(
        &mut self,
        oid: u64,
        side: Side,
        price: i64,
        size: i64,
        user_id: u32,
        flags: u8,
    ) -> &RestingOrder {
        self.remove(oid);
        self.seq += 1;
        let seq = self.seq;
        let level = self.levels_of_mut(side).entry(price).or_default();
        level.total = level.total.wrapping_add(size);
        level.live += usize::from(size > 0);
        level.queue.insert(seq, oid);
        self.orders.entry(oid).or_insert(RestingOrder {
            oid,
            side,
            price,
            size,
            user_id,
            flags,
            seq,
        })
    }

    /// Remove an order; the order, or `None` when it is not in the book.
    pub fn remove(&mut self, oid: u64) -> Option<RestingOrder> {
        let order = self.orders.remove(&oid)?;
        let levels = self.levels_of_mut(order.side);
        if let Some(level) = levels.get_mut(&order.price) {
            level.total = level.total.wrapping_sub(order.size);
            level.live = level.live.saturating_sub(usize::from(order.size > 0));
            level.queue.remove(&order.seq);
            if level.queue.is_empty() {
                levels.remove(&order.price);
            }
        }
        Some(order)
    }

    /// Take `by` off an order's remaining size, not below zero. `false` when
    /// the order is not in the book.
    pub fn reduce(&mut self, oid: u64, by: i64) -> bool {
        let Some(order) = self.orders.get_mut(&oid) else {
            return false;
        };
        let was_live = order.size > 0;
        let taken = by.min(order.size);
        order.size = order.size.wrapping_sub(taken);
        let (side, price, live) = (order.side, order.price, order.size > 0);
        if let Some(level) = self.levels_of_mut(side).get_mut(&price) {
            level.total = level.total.wrapping_sub(taken);
            level.live = (level.live + usize::from(live)).saturating_sub(usize::from(was_live));
        }
        true
    }

    /// Set an order's remaining size and keep its place in the queue.
    /// `false` when the order is not in the book.
    pub fn resize(&mut self, oid: u64, size: i64) -> bool {
        let Some(order) = self.orders.get_mut(&oid) else {
            return false;
        };
        let was_live = order.size > 0;
        let change = size.wrapping_sub(order.size);
        order.size = size;
        let (side, price) = (order.side, order.price);
        if let Some(level) = self.levels_of_mut(side).get_mut(&price) {
            level.total = level.total.wrapping_add(change);
            level.live = (level.live + usize::from(size > 0)).saturating_sub(usize::from(was_live));
        }
        true
    }

    /// Remove every order.
    pub fn clear(&mut self) {
        self.orders.clear();
        self.bids.clear();
        self.asks.clear();
        self.seq = 0;
    }

    fn levels_of(&self, side: Side) -> &BTreeMap<i64, Level> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn levels_of_mut(&mut self, side: Side) -> &mut BTreeMap<i64, Level> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    // -- reading -------------------------------------------------------------

    /// The best bid price (×10⁸).
    pub fn best_bid(&self) -> Option<i64> {
        self.levels(Side::Bid).next().map(|l| l.price)
    }

    /// The best ask price (×10⁸).
    pub fn best_ask(&self) -> Option<i64> {
        self.levels(Side::Ask).next().map(|l| l.price)
    }

    /// The price levels of one side from the best price outwards, with their
    /// total size and the number of orders with a size above 0. Prices whose
    /// orders all have size 0 are left out.
    pub fn levels(&self, side: Side) -> impl Iterator<Item = PriceLevel> + '_ {
        let levels = self.levels_of(side).iter();
        let ordered = match side {
            Side::Bid => BestFirst::Down(levels.rev()),
            Side::Ask => BestFirst::Up(levels),
        };
        ordered
            .filter(|(_, level)| level.total > 0)
            .map(|(&price, level)| PriceLevel {
                price,
                size: level.total,
                orders: level.live,
            })
    }

    /// At most `n` price levels of one side, from the best price outwards
    /// (`usize::MAX` for all).
    pub fn depth(&self, side: Side, n: usize) -> Vec<PriceLevel> {
        self.levels(side).take(n).collect()
    }

    /// The orders resting at a price, front of the queue first.
    ///
    /// This includes orders filled down to size 0 whose removal event has
    /// not been applied yet (it follows in the same block); skip them with
    /// `.filter(|o| o.size > 0)` if you do not want them.
    pub fn queue(&self, side: Side, price: i64) -> impl Iterator<Item = &RestingOrder> + '_ {
        self.levels_of(side)
            .get(&price)
            .into_iter()
            .flat_map(|level| level.queue.values())
            .filter_map(|oid| self.orders.get(oid))
    }

    /// `(orders_ahead, size_ahead)` of a resting order in its price's queue,
    /// or `None` when it is not in the book. Orders ahead filled down to size
    /// 0 (still listed until their removal) are not counted.
    pub fn queue_position(&self, oid: u64) -> Option<(usize, i64)> {
        let order = self.orders.get(&oid)?;
        let level = self.levels_of(order.side).get(&order.price)?;
        let mut ahead = 0;
        let mut size: i64 = 0;
        for other in level.queue.range(..order.seq).map(|(_, oid)| oid) {
            let other_size = self.orders.get(other).map_or(0, |o| o.size);
            if other_size > 0 {
                ahead += 1;
                size = size.saturating_add(other_size);
            }
        }
        Some((ahead, size))
    }

    /// Every resting order of one account, earliest arrival first.
    pub fn orders_of(&self, user_id: u32) -> Vec<&RestingOrder> {
        let mut orders: Vec<&RestingOrder> = self
            .orders
            .values()
            .filter(|o| o.user_id == user_id)
            .collect();
        orders.sort_by_key(|o| o.seq);
        orders
    }
}
