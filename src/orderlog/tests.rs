use std::io::Cursor;

use super::*;
use crate::outcome::TradeKind;
use crate::testutil::*;
use crate::time::UtcDateTime;

const BID: u8 = 1;
const ASK: u8 = 2;
/// Three block times, 0.1 s apart.
const B1: i64 = T0 + 1_000_000;
const B2: i64 = T0 + 2_000_000;
const B3: i64 = T0 + 3_000_000;

/// A small day: two accounts, a resting book, then three blocks of events.
/// `user_time` is the time of the USER records, written before the blocks.
fn capture_with_users_at(user_time: i64) -> Vec<u8> {
    container(
        &[
            meta("xyz:CL", 0),
            [
                user(1, 0xAA, user_time),
                user(2, 0xBB, user_time),
                snap(10, BID, 100.0, 1.0, 1, 0),
                snap(11, BID, 100.0, 2.0, 2, 0),
                snap(12, ASK, 101.0, 3.0, 1, 0),
                depth(T0, 100.0, 3.0, 1), // the aggregated L2 view: skipped
            ]
            .concat(),
            [
                block(500, B1),
                order(0, 13, BID, 100.0, 4.0, 2, B1), // back of the 100 queue
                order_with(0, 14, ASK, 105.0, 1.0, 1, 0, 2, 0, B1), // a trigger order waits off the book
                order(3, 15, BID, 101.0, 1.0, 2, B1),               // rejected: never rested
                trade(
                    ASK,
                    100.0,
                    0.5,
                    10,
                    99,
                    1,
                    2,
                    TRADE_MAKER_OID | TRADE_TAKER_OID,
                    B1,
                ), // partial fill of 10
                order(2, 11, BID, 100.0, 2.0, 2, B1),
            ]
            .concat(),
            [
                block(501, B2),
                order(251, 16, ASK, 102.0, 2.0, 2, B2),
                order(252, 13, BID, 100.0, 3.5, 2, B2), // new size, same place
                trade(ASK, 100.0, 0.5, 10, 0, 1, 2, TRADE_MAKER_OID, B2), // fills the rest of 10
                order(1, 10, BID, 100.0, 0.0, 1, B2),
            ]
            .concat(),
            [block(502, B3), order(0, 17, BID, 100.0, 1.0, 1, B3)].concat(),
        ],
        true,
    )
}

fn capture() -> Vec<u8> {
    capture_with_users_at(T0)
}

fn records(data: &[u8]) -> Vec<Record> {
    Reader::new(data).map(|r| r.unwrap()).collect()
}

fn oids(book: &OrderBook) -> Vec<u64> {
    let mut oids: Vec<u64> = book.orders().map(|o| o.oid).collect();
    oids.sort_unstable();
    oids
}

fn queue_oids(book: &OrderBook, side: Side, price: i64) -> Vec<u64> {
    book.queue(side, price).map(|o| o.oid).collect()
}

fn level(price: i64, size: i64, orders: usize) -> PriceLevel {
    PriceLevel {
        price,
        size,
        orders,
    }
}

#[test]
fn records_come_back_typed() {
    let records = records(&capture());
    let kinds: Vec<i16> = records.iter().map(Record::kind).collect();
    assert_eq!(&kinds[..6], &[16, 21, 21, 20, 20, 20]);
    assert!(!kinds.contains(&0));
    let Record::Meta(m) = &records[0] else {
        panic!()
    };
    assert_eq!(
        (
            m.coin.as_str(),
            m.day,
            m.size_decimals,
            m.asset,
            m.start_block,
            m.is_outcome()
        ),
        ("xyz:CL", 20261023, 2, 7, 1000, false)
    );
    assert_eq!(m.start_block_time_ns, 1_792_713_600_000_000_000);
    assert_eq!(
        records[1],
        Record::User(User {
            time: T0,
            user_id: 1,
            address: Address([0xAA; 20])
        })
    );
    let Record::User(u) = &records[1] else {
        panic!()
    };
    assert_eq!(u.address.to_string(), format!("0x{}", "aa".repeat(20)));
    assert_eq!(
        records[3],
        Record::SnapOrder(SnapOrder {
            time: T0,
            side: Side::Bid,
            tif: Tif::GTC,
            flags: 0,
            oid: 10,
            price: 100 * P,
            size: P,
            user_id: 1
        })
    );
    let o = records.iter().find_map(|r| match r {
        Record::Order(o) => Some(*o),
        _ => None,
    });
    let o = o.unwrap();
    assert_eq!(
        (
            o.status,
            o.side,
            o.oid,
            o.price,
            o.size,
            o.user_id,
            o.status.to_string()
        ),
        (
            Status::OPEN,
            Side::Bid,
            13,
            100 * P,
            4 * P,
            2,
            "open".to_string()
        )
    );
    let t = records.iter().find_map(|r| match r {
        Record::Trade(t) => Some(*t),
        _ => None,
    });
    let t = t.unwrap();
    assert_eq!(
        (
            t.taker_side,
            t.price,
            t.size,
            t.maker_oid,
            t.taker_oid,
            t.has_maker_oid(),
            t.has_taker_oid()
        ),
        (Side::Ask, 100 * P, P / 2, 10, 99, true, true)
    );
    assert_eq!((t.maker_position, t.taker_position), (150_000_000, -2 * P));
    assert!(!t.on_second_side() && !t.maker_is_buyer() && t.direction_override() == 0);
    let b = records.iter().find_map(|r| match r {
        Record::Block(b) => Some(*b),
        _ => None,
    });
    let b = b.unwrap();
    assert_eq!(b.number, 500);
    assert_eq!(
        UtcDateTime::from_unix_ns(b.time_ns as i64).to_string(),
        "2026-10-23 00:00:00.100000"
    );
    assert_eq!(b.local_time_ns, b.time_ns + 300_000_000);
}

#[test]
fn the_book_follows_the_format_rules() {
    let mut replay = Replay::new(Cursor::new(capture()));
    replay.run().unwrap();
    let book = replay.book();
    assert_eq!(replay.accounts().len(), 2);
    assert_eq!(replay.address(2), Some(Address([0xBB; 20])));
    assert_eq!(replay.address(3), None);
    assert_eq!(replay.block().unwrap().number, 502);
    // 10 filled, 11 canceled, 14 waits off the book, 15 rejected.
    assert_eq!(oids(book), vec![12, 13, 16, 17]);
    assert_eq!(queue_oids(book, Side::Bid, 100 * P), vec![13, 17]);
    assert_eq!(book.get(13).unwrap().size, 350_000_000);
    assert_eq!(book.queue_position(17), Some((1, 350_000_000)));
    assert_eq!(book.queue_position(13), Some((0, 0)));
    assert_eq!(book.queue_position(404), None);
    assert_eq!(
        (book.best_bid(), book.best_ask()),
        (Some(100 * P), Some(101 * P))
    );
    assert_eq!(
        book.depth(Side::Bid, 10),
        vec![level(100 * P, 450_000_000, 2)]
    );
    assert_eq!(
        book.depth(Side::Ask, 10),
        vec![level(101 * P, 3 * P, 1), level(102 * P, 2 * P, 1)]
    );
    assert_eq!(book.depth(Side::Ask, 1).len(), 1);
    let of_1: Vec<u64> = book.orders_of(1).iter().map(|o| o.oid).collect();
    assert_eq!(of_1, vec![12, 17]);
    assert_eq!(
        (book.order_count(Side::Bid), book.order_count(Side::Ask)),
        (2, 2)
    );
    assert_eq!(replay.last_trade_kind(), None);
}

#[test]
fn run_until_stops_before_later_blocks() {
    let mut replay = Replay::new(Cursor::new(capture()));
    replay.run_until(B1).unwrap();
    let book = replay.book();
    assert_eq!(replay.block().unwrap().number, 500);
    assert_eq!(queue_oids(book, Side::Bid, 100 * P), vec![10, 13]); // 11 canceled in this block
    assert_eq!(book.get(10).unwrap().size, P / 2); // the partial fill only
    assert!(!book.contains(16));

    // The first block is always read: here it is block 500, and 11 is canceled in it.
    let mut start = Replay::new(Cursor::new(capture()));
    start.run_until(T0).unwrap();
    assert_eq!(start.block().unwrap().number, 500);
    assert!(!start.book().contains(11));
    assert!(start.meta().is_some());
}

#[test]
fn a_moment_before_the_day_gives_the_day_start_book() {
    // Real files put the day-start orders after the seed block, whose time is
    // before 00:00 UTC; a moment before it must still give that book.
    let seeded = container(
        &[
            [meta("BTC", 0), user(1, 0xAA, T0)].concat(),
            [
                block(499, T0 - 1000),
                snap(10, BID, 100.0, 1.0, 1, 0),
                snap(11, ASK, 101.0, 1.0, 1, 0),
            ]
            .concat(),
            [block(500, B1), order(2, 10, BID, 100.0, 1.0, 1, B1)].concat(),
        ],
        true,
    );
    let mut replay = Replay::new(Cursor::new(seeded));
    replay.run_until(0).unwrap();
    assert_eq!(replay.block().unwrap().number, 499);
    assert_eq!(oids(replay.book()), vec![10, 11]);
    replay.run_until(B1).unwrap();
    assert_eq!(oids(replay.book()), vec![11]);
}

#[test]
fn run_until_applies_user_records_whatever_their_time() {
    // The USER records carry the time of a later block (as real files do),
    // but are written before the first block.
    let data = capture_with_users_at(B3);
    let mut replay = Replay::new(&data[..]);
    replay.run_until(B1).unwrap();
    assert_eq!(replay.block().unwrap().number, 500);
    assert_eq!(replay.accounts().len(), 2);
    assert_eq!(queue_oids(replay.book(), Side::Bid, 100 * P), vec![10, 13]);
    let mut first = Replay::new(&data[..]);
    first.run_until(T0).unwrap(); // the first block is applied whatever the moment
    assert_eq!(first.block().unwrap().number, 500);
    assert_eq!(
        (oids(first.book()), first.accounts().len()),
        (vec![10, 12, 13], 2)
    );
}

#[test]
fn a_replay_runs_on_in_steps() {
    let data = capture();
    let mut stepped = Replay::new(&data[..]);
    stepped.run_until(B1).unwrap();
    assert_eq!(stepped.block().unwrap().number, 500);
    assert!(!stepped.book().contains(16));
    stepped.run_until(B1).unwrap(); // the same moment again: nothing changes
    assert_eq!(stepped.block().unwrap().number, 500);
    stepped.run_until(B2).unwrap();
    assert_eq!(stepped.block().unwrap().number, 501);
    assert!(stepped.book().contains(16) && !stepped.book().contains(10));
    let rest: Vec<i16> = stepped.by_ref().map(|r| r.unwrap().kind()).collect();
    assert_eq!(rest, vec![KIND_BLOCK, KIND_ORDER]); // block 502 and its open, nothing read twice

    let mut whole = Replay::new(&data[..]);
    whole.run().unwrap();
    assert_eq!(oids(stepped.book()), oids(whole.book()));
    assert_eq!(
        stepped.book().queue_position(17),
        whole.book().queue_position(17)
    );
    assert!(stepped.next_record().unwrap().is_none()); // at the end: nothing more
    stepped.run().unwrap();
    assert_eq!(stepped.block().unwrap().number, 502);

    // Every record exactly once, whatever the steps.
    let all = records(&data);
    let mut seen = Vec::new();
    let mut steps = Replay::new(&data[..]);
    for until in [T0 - 1, T0, B1, B1, B2 + 5] {
        steps.run_until(until).unwrap();
        while let Some(record) = steps.next_record().unwrap() {
            seen.push(record.clone());
            if matches!(record, Record::Block(_)) {
                break;
            }
        }
    }
    while let Some(record) = steps.next_record().unwrap() {
        seen.push(record);
    }
    // The records next_record returned come in file order, none twice (the
    // others were applied by run_until).
    let mut rest = all.iter();
    for record in &seen {
        assert!(
            rest.any(|r| r == record),
            "{record:?} out of order or repeated"
        );
    }
    assert_eq!(
        seen.iter()
            .filter(|r| matches!(r, Record::Block(_)))
            .count(),
        2
    );
    assert_eq!(oids(steps.book()), oids(whole.book()));
}

#[test]
fn next_record_yields_each_record_after_applying_it() {
    let mut replay = Replay::new(Cursor::new(capture()));
    let mut seen = Vec::new();
    while let Some(record) = replay.next_record().unwrap() {
        if let Record::Order(o) = record {
            if o.oid == 13 && o.status == Status::OPEN {
                seen.push(replay.book().queue_position(13));
            }
        }
    }
    assert_eq!(seen, vec![Some((2, 3 * P))]);
}

#[test]
fn book_operations_directly() {
    let mut book = OrderBook::new();
    book.insert(1, Side::Bid, 10 * P, P, 7, 0);
    book.insert(2, Side::Bid, 10 * P, 2 * P, 8, 0);
    book.insert(1, Side::Bid, 11 * P, P, 7, 0); // the same id again: replaced, at the back of its new price
    assert_eq!(book.best_bid(), Some(11 * P));
    assert_eq!(queue_oids(&book, Side::Bid, 10 * P), vec![2]);
    book.insert(3, Side::Bid, 10 * P, P, 8, 0);
    assert!(book.reduce(2, 5 * P)); // not below zero; stays until removed
    assert_eq!(book.get(2).unwrap().size, 0);
    assert_eq!(queue_oids(&book, Side::Bid, 10 * P), vec![2, 3]); // still listed, at size 0
    assert_eq!(book.queue_position(3), Some((0, 0))); // but not counted ahead
    assert_eq!(
        book.depth(Side::Bid, 10),
        vec![level(11 * P, P, 1), level(10 * P, P, 1)]
    );
    assert!(book.resize(2, P / 2)); // back above 0: counted again
    assert_eq!(book.queue_position(3), Some((1, P / 2)));
    assert_eq!(book.depth(Side::Bid, 10)[1], level(10 * P, 3 * P / 2, 2));
    assert!(book.reduce(3, P) && book.reduce(2, P));
    assert_eq!(book.depth(Side::Bid, 10), vec![level(11 * P, P, 1)]); // all at 0: not reported
    book.remove(3);
    assert!(!book.reduce(99, P) && !book.resize(99, P));
    assert_eq!(book.remove(2).unwrap().oid, 2);
    assert!(book.remove(2).is_none());
    assert!(book.resize(1, 3 * P));
    assert_eq!(book.depth(Side::Bid, 10), vec![level(11 * P, 3 * P, 1)]);
    book.clear();
    assert!(book.is_empty() && book.best_bid().is_none() && book.orders().count() == 0);
}

fn open_order(status: u8, oid: u64, tif: u8, size: f64) -> Order {
    let raw = order_with(status, oid, BID, 100.0, size, 3, tif, 0, 0, B1);
    match Record::decode(
        crate::container::records_of_block(&raw)
            .next()
            .unwrap()
            .unwrap(),
    ) {
        Ok(Some(Record::Order(o))) => o,
        other => panic!("{other:?}"),
    }
}

/// Orders 1, 2, 3 at one price, then one ORDER event for order 2.
fn after_event(status: u8, tif: u8) -> OrderBook {
    let mut book = OrderBook::new();
    for oid in 1..=3 {
        book.insert(oid, Side::Bid, 100 * P, P, 1, 0);
    }
    book.apply_order(&open_order(status, 2, tif, 0.25));
    book
}

#[test]
fn every_status_does_what_the_format_says() {
    let removing = [1u8, 2, 9, 13, 15, 17, 18, 19, 20, 22, 250];
    for code in 0..=255u8 {
        let book = after_event(code, 1);
        let queue = queue_oids(&book, Side::Bid, 100 * P);
        let size = book.get(2).map(|o| o.size);
        match code {
            // open and "correction: rests" add the order again: at the back, with the event's size
            0 | 251 => assert_eq!((queue, size), (vec![1, 3, 2], Some(P / 4)), "{code}"),
            // "correction: size" keeps the place in the queue
            252 => assert_eq!((queue, size), (vec![1, 2, 3], Some(P / 4)), "{code}"),
            c if removing.contains(&c) => assert_eq!((queue, size), (vec![1, 3], None), "{code}"),
            // rejects, triggered, unknown codes: nothing changes
            _ => assert_eq!((queue, size), (vec![1, 2, 3], Some(P)), "{code}"),
        }
    }
    // An open with time in force NONE places a trigger order: off the book.
    let mut book = OrderBook::new();
    book.apply_order(&open_order(0, 9, 0, 1.0));
    assert!(book.is_empty());
    // 251 adds an order whatever its time in force; 252 of an unknown order does nothing.
    book.apply_order(&open_order(251, 9, 0, 1.0));
    book.apply_order(&open_order(252, 8, 1, 1.0));
    assert_eq!(oids(&book), vec![9]);
}

#[test]
fn trades_reduce_the_orders_whose_ids_are_set() {
    let mut book = OrderBook::new();
    book.insert(1, Side::Bid, 100 * P, 2 * P, 1, 0);
    book.insert(2, Side::Ask, 100 * P, 2 * P, 2, 0);
    let fill = |flags: u8| Trade {
        time: B1,
        taker_side: Side::Ask,
        flags,
        price: 100 * P,
        size: P / 2,
        maker_oid: 1,
        taker_oid: 2,
        maker_user_id: 1,
        taker_user_id: 2,
        maker_position: 0,
        taker_position: 0,
    };
    book.apply_trade(&fill(0)); // neither id set
    assert_eq!(
        (book.get(1).unwrap().size, book.get(2).unwrap().size),
        (2 * P, 2 * P)
    );
    book.apply_trade(&fill(TRADE_MAKER_OID));
    assert_eq!(
        (book.get(1).unwrap().size, book.get(2).unwrap().size),
        (3 * P / 2, 2 * P)
    );
    book.apply_trade(&fill(TRADE_MAKER_OID | TRADE_TAKER_OID));
    assert_eq!(
        (book.get(1).unwrap().size, book.get(2).unwrap().size),
        (P, 3 * P / 2)
    );
    assert_eq!(book.depth(Side::Bid, 1), vec![level(100 * P, P, 1)]);
}

#[test]
fn format_errors() {
    let mut bad_magic = meta("BTC", 0);
    bad_magic[12..16].copy_from_slice(b"XXXX");
    let mut reader = Reader::new(Cursor::new(container(&[bad_magic], true)));
    assert!(matches!(reader.next(), Some(Err(Error::Format(m))) if m.contains("HLC1")));
    assert!(reader.next().is_none());

    let mut short = order(0, 1, BID, 1.0, 1.0, 1, T0);
    short[2..4].copy_from_slice(&20u16.to_le_bytes());
    short.truncate(20);
    let mut reader = Reader::new(Cursor::new(container(&[short], true)));
    assert!(matches!(reader.next(), Some(Err(Error::Format(m))) if m.contains("fewer than")));

    let mut long_coin = meta("BTC", 0);
    long_coin[44] = 200;
    let mut reader = Reader::new(Cursor::new(container(&[long_coin], true)));
    assert!(matches!(reader.next(), Some(Err(Error::Format(m))) if m.contains("coin name")));

    // An error ends a replay too.
    let mut replay = Replay::new(Cursor::new(
        container(&[meta("BTC", 0)], true)[..20].to_vec(),
    ));
    assert!(matches!(replay.run(), Err(Error::Truncated(_))));
    assert!(replay.next_record().unwrap().is_none());
}

// -- HIP-4 outcome markets ---------------------------------------------------------

const SECOND: u8 = ORDER_SECOND_SIDE;

/// Both books of one outcome, in the first side's terms.
fn outcome_capture() -> Vec<u8> {
    container(
        &[
            [
                meta("Fed decision", META_OUTCOME),
                outcome("outcome=3600\ntitle=Fed decision\nside0=Yes\nside1=No"),
            ]
            .concat(),
            [
                user(1, 1, T0),
                user(2, 2, T0),
                snap(1, BID, 0.60, 10.0, 1, 0),      // Yes buy 0.60
                snap(2, ASK, 0.65, 10.0, 1, SECOND), // No buy 0.35, shown as an ask at 0.65
                snap(3, ASK, 0.70, 10.0, 2, 0),      // Yes sell 0.70
                snap(4, BID, 0.55, 10.0, 2, SECOND), // No sell 0.45, shown as a bid at 0.55
            ]
            .concat(),
            [
                block(1, B1),
                // taker buys Yes from the first book's ask: tokens change hands
                trade(BID, 0.70, 1.0, 3, 90, 2, 1, 3, B1),
                // taker buys Yes (first book) against a No buyer (second book): a mint
                trade(BID, 0.65, 1.0, 2, 91, 1, 2, 3, B1),
                // taker on the second book sells No (placed: a sell) against a Yes seller: a merge
                trade(BID, 0.70, 1.0, 3, 92, 2, 1, 3 | TRADE_SECOND_SIDE, B1),
                // taker on the second book buys No (placed: a buy) against a No seller: second book
                trade(ASK, 0.55, 1.0, 4, 93, 2, 1, 3 | TRADE_SECOND_SIDE, B1),
                // maker not in the book
                trade(BID, 0.70, 1.0, 777, 94, 2, 1, 3, B1),
                // the maker's removal written before its trade in the same block: still known
                order(2, 1, BID, 0.60, 9.0, 1, B1),
                trade(ASK, 0.60, 1.0, 1, 95, 1, 2, 3, B1),
                // a second-book maker removed before its trade
                order(2, 2, ASK, 0.65, 9.0, 1, B1),
                trade(BID, 0.65, 1.0, 2, 96, 1, 2, 3, B1),
            ]
            .concat(),
            [
                block(2, B2),
                // a removal in an earlier block is forgotten: the maker is unknown now
                trade(ASK, 0.60, 1.0, 1, 97, 1, 2, 3, B2),
                // no maker id: unknown
                trade(BID, 0.70, 1.0, 3, 98, 2, 1, TRADE_TAKER_OID, B2),
            ]
            .concat(),
        ],
        true,
    )
}

#[test]
fn trade_kinds_of_an_outcome_market() {
    use TradeKind::*;
    let mut replay = Replay::new(Cursor::new(outcome_capture()));
    let mut kinds = Vec::new();
    while let Some(record) = replay.next_record().unwrap() {
        if let Record::Trade(_) = record {
            kinds.push(replay.last_trade_kind().unwrap());
        }
    }
    assert_eq!(
        kinds,
        vec![FirstBook, Mint, Merge, SecondBook, Unknown, FirstBook, Mint, Unknown, Unknown]
    );
    assert!(replay.is_outcome() && replay.meta().unwrap().is_outcome());
    let info = replay.outcome().unwrap();
    assert_eq!(
        (info.outcome, info.title.as_str()),
        (Some(3600), "Fed decision")
    );
    // The book itself is applied in file order: 1 and 2 are gone.
    assert_eq!(oids(replay.book()), vec![3, 4]);
}

#[test]
fn trade_kind_is_none_outside_outcome_files() {
    let mut replay = Replay::new(Cursor::new(capture()));
    while replay.next_record().unwrap().is_some() {
        assert_eq!(replay.last_trade_kind(), None);
    }
    assert!(!replay.is_outcome() && replay.outcome().is_none());
}
