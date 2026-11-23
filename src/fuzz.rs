//! A small deterministic fuzz test: damaged files through every reader and
//! book must give records or errors, never a panic (debug builds check
//! every overflow).

use crate::container::{decode_frame, records_of_block, Blocks, RawRecords};
use crate::orderlog::{self, OrderBook, Record, Replay};
use crate::outcome::{as_placed, trade_kind};
use crate::testutil::*;
use crate::time::{format_fixed, UtcDateTime};
use crate::{detect, l2, names, L2Book, Side};

/// xorshift64*: the same bytes on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const EXTREMES: [i64; 6] = [i64::MIN, i64::MIN + 1, -1, 0, i64::MAX - 1, i64::MAX];

/// Records with every kind, some of them with extreme prices and sizes.
fn seeds() -> Vec<Vec<u8>> {
    let mut records = vec![
        meta("xyz:CL", orderlog::META_OUTCOME),
        outcome("outcome=1\nside0=A\nside1=B"),
        user(1, 0xAA, T0),
        block(1, T0),
        snap(10, 1, 100.0, 1.0, 1, 0),
        snap(11, 2, 101.0, 2.0, 1, orderlog::ORDER_SECOND_SIDE),
        block(2, T0 + 1),
        order(0, 12, 1, 100.0, 4.0, 1, T0 + 1),
        order(252, 12, 1, 100.0, 1.0, 1, T0 + 1),
        trade(1, 100.0, 0.5, 10, 12, 1, 1, 3, T0 + 1),
        order(2, 10, 1, 100.0, 0.0, 1, T0 + 1),
        trade(2, 100.0, 0.5, 10, 11, 1, 1, 0x83, T0 + 1),
        depth(T0, 100.0, 1.0, l2::BUY | l2::CLEAR),
        depth(T0, 99.0, 1.0, l2::SELL),
        l2_trade(T0, 100.0, 1.0, 1, 5),
        unknown(99, T0),
    ];
    // The same records again with extreme numbers in their price and size fields.
    let mut extreme = Vec::new();
    for (i, &value) in EXTREMES.iter().enumerate() {
        let mut o = order(
            if i % 2 == 0 { 0 } else { 251 },
            20 + i as u64,
            1,
            1.0,
            1.0,
            1,
            T0 + 2,
        );
        o[25..33].copy_from_slice(&value.to_le_bytes());
        o[33..41].copy_from_slice(&EXTREMES[5 - i].to_le_bytes());
        extreme.push(o);
        let mut t = trade(2, 1.0, 1.0, 20 + i as u64, 21 + i as u64, 1, 1, 3, T0 + 2);
        t[22..30].copy_from_slice(&value.to_le_bytes());
        extreme.push(t);
        let mut r = order(252, 20 + i as u64, 1, 1.0, 1.0, 1, T0 + 2);
        r[33..41].copy_from_slice(&value.to_le_bytes());
        extreme.push(r);
        let mut d = depth(T0, 1.0, 1.0, if i % 2 == 0 { l2::BUY } else { l2::SELL });
        d[12..20].copy_from_slice(&value.to_le_bytes());
        d[20..28].copy_from_slice(&EXTREMES[(i + 2) % 6].to_le_bytes());
        extreme.push(d);
    }
    records.extend(extreme);
    records
}

/// Read a file every way the crate offers; only panics matter.
fn exercise(data: &[u8]) {
    let _ = detect(data);
    for block in Blocks::new(data) {
        let Ok(block) = block else { break };
        for record in records_of_block(&block) {
            let Ok(record) = record else { break };
            let _ = Record::decode(record);
            let _ = l2::Event::decode(record);
        }
    }
    let mut raw = RawRecords::new(data);
    while let Ok(Some(_)) = raw.next_record() {}

    for uncross in [false, true] {
        let mut book = L2Book::new().with_uncross(uncross);
        for event in l2::Reader::new(data).include_non_market(true) {
            match event {
                Ok(l2::Event::Depth(update)) => book.apply(&update),
                Ok(_) => {}
                Err(_) => break,
            }
            let _ = (book.spread(), book.mid(), book.depth(3));
        }
        let _ = l2::book_at(data, T0, uncross);
    }

    let mut replay = Replay::new(data);
    let _ = replay.run_until(T0);
    while let Ok(Some(record)) = replay.next_record() {
        if let Record::Order(o) = &record {
            let _ = as_placed(o.side, o.price, o.flags);
            let _ = replay.book().queue_position(o.oid);
        }
        if let Record::Trade(t) = &record {
            let _ = trade_kind(t.flags, t.taker_side, Some(t.flags));
            let _ = UtcDateTime::from_ticks(t.time).to_ticks();
        }
        if let Record::Block(b) = &record {
            let _ = UtcDateTime::from_unix_ns(b.time_ns as i64).to_string();
        }
    }
    look_at(replay.book());
}

fn look_at(book: &OrderBook) {
    for side in [Side::Bid, Side::Ask] {
        let _ = book.depth(side, usize::MAX);
        let _ = book.order_count(side);
    }
    let _ = (book.best_bid(), book.best_ask());
    for order in book.orders() {
        let _ = book.queue_position(order.oid);
        let _ = book.queue(order.side, order.price).count();
        let _ = book.orders_of(order.user_id);
        let _ = format_fixed(order.price);
    }
}

#[test]
fn damaged_files_never_panic() {
    let seeds = seeds();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..600 {
        // A file of a random selection of records, in stored or LZ4 frames.
        let mut blocks = Vec::new();
        for _ in 0..1 + rng.below(4) {
            let mut block = Vec::new();
            for _ in 0..1 + rng.below(10) {
                block.extend_from_slice(&seeds[rng.below(seeds.len())]);
            }
            blocks.push(block);
        }
        let mut data = container(&blocks, round % 3 == 0);
        exercise(&data);
        // Damage it: bit flips, random bytes, a cut, random garbage.
        match round % 4 {
            0 => {
                for _ in 0..1 + rng.below(8) {
                    let at = rng.below(data.len());
                    data[at] ^= 1 << rng.below(8);
                }
            }
            1 => {
                for _ in 0..1 + rng.below(8) {
                    let at = rng.below(data.len());
                    data[at] = rng.next() as u8;
                }
            }
            2 => data.truncate(rng.below(data.len())),
            _ => {
                data = (0..rng.below(300)).map(|_| rng.next() as u8).collect();
                if data.len() >= 8 && rng.below(2) == 0 {
                    // A plausible header and frame length, so the frame decoder sees the garbage.
                    let len = (data.len() - 8) as i32;
                    data[..4].copy_from_slice(&65536i32.to_le_bytes());
                    data[4..8].copy_from_slice(&len.to_le_bytes());
                }
            }
        }
        exercise(&data);
        let _ = decode_frame(&data);
    }
}

#[test]
fn books_take_extreme_numbers() {
    let mut book = OrderBook::new();
    for (i, &price) in EXTREMES.iter().enumerate() {
        for (j, &size) in EXTREMES.iter().enumerate() {
            let oid = (i * 10 + j) as u64;
            let side = if j % 2 == 0 { Side::Bid } else { Side::Ask };
            book.insert(oid, side, price, size, 1, 0);
            book.reduce(oid, EXTREMES[(i + j) % 6]);
            book.resize(oid, EXTREMES[(i + 2 * j) % 6]);
            book.reduce(oid, size);
            look_at(&book);
        }
    }
    for oid in 0..60 {
        book.remove(oid);
    }
    assert!(book.is_empty());
    for &price in &EXTREMES {
        let _ = as_placed(Side::Bid, price, orderlog::ORDER_SECOND_SIDE);
    }
    let mut l2 = L2Book::new().with_uncross(true);
    for &price in &EXTREMES {
        for bid in [true, false] {
            l2.apply(&l2::DepthUpdate {
                time: price,
                price,
                volume: 1,
                flags: if bid { l2::BUY } else { l2::SELL },
            });
            let _ = (l2.spread(), l2.mid());
        }
    }
    let mut plain = L2Book::new();
    plain.apply(&l2::DepthUpdate {
        time: 0,
        price: i64::MIN,
        volume: 1,
        flags: l2::SELL,
    });
    plain.apply(&l2::DepthUpdate {
        time: 0,
        price: i64::MAX,
        volume: 1,
        flags: l2::BUY,
    });
    assert_eq!(plain.spread(), Some(i64::MIN));
}

#[test]
fn names_take_any_text() {
    let mut rng = Rng(7);
    let alphabet = "_ABCxyz019#.-:@/HLC4é\n".chars().collect::<Vec<_>>();
    for _ in 0..3000 {
        let mut name: String = (0..rng.below(30))
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect();
        match rng.below(3) {
            0 => name.push_str("_20261101.bin.lz4"),
            1 => name.push_str("_HLC_20261101.bin.lz4"),
            _ => {}
        }
        let _ = (
            names::parse_l2_name(&name),
            names::parse_orderlog_name(&name),
        );
        let _ = (
            names::file_kind(&name),
            names::orderlog_coin(&name),
            names::split_l2_stem(&name),
        );
        let _ = names::orderlog_stem(&name);
    }
}

#[test]
fn time_text_and_outcome_text_take_anything() {
    let mut rng = Rng(11);
    for _ in 0..2000 {
        let ticks = rng.next() as i64;
        let moment = UtcDateTime::from_ticks(ticks);
        assert_eq!(moment.to_ticks(), ticks);
        let _ = UtcDateTime::from_unix_ns(ticks).to_ticks();
        let _ = UtcDateTime::from_yyyymmdd(rng.next() as u32);
        let text: String = (0..rng.below(40))
            .map(|_| {
                [
                    "=", "\n", "|", ":", "outcome", "settled", "1e999", "nan", "x",
                ][rng.below(9)]
            })
            .collect();
        let _ = crate::outcome::OutcomeInfo::parse(&text);
    }
}
