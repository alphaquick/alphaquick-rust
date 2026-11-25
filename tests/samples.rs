//! The real files: the free samples, checked against the numbers the Python
//! package and AlphaQuick's replay tools produce for them.
//!
//! These tests run when `ALPHAQUICK_SAMPLES_DIR` names a directory holding
//! the sample files (`tests/samples.txt` lists them, with their URLs and
//! SHA-256); each is skipped when its file is missing. A few tests read
//! larger files that are not free samples (`xyz_CL_HLC_20261101`, and the
//! current-build order logs of 17 November 2026 from
//! `hyperliquid/hlc_v2/20261117/`); CI does not have them, and they run only
//! where someone put them in that directory.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use alphaquick::l2::{self, Event};
use alphaquick::orderlog::{Reader, Record, Replay, DEX_SPOT, ORDER_SECOND_SIDE};
use alphaquick::outcome::TradeKind;
use alphaquick::time::UtcDateTime;
use alphaquick::{names, L2Book, OrderBook, Side};

fn sample(name: &str) -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("ALPHAQUICK_SAMPLES_DIR") else {
        eprintln!("skipped: ALPHAQUICK_SAMPLES_DIR is not set");
        return None;
    };
    let path = PathBuf::from(dir).join(name);
    if !path.is_file() {
        eprintln!("skipped: {} is missing", path.display());
        return None;
    }
    Some(path)
}

#[derive(Debug, Default, PartialEq)]
struct Counts {
    snap_orders: usize,
    blocks: usize,
    orders: usize,
    trades: usize,
    kinds: BTreeMap<TradeKind, usize>,
}

fn count<R: std::io::Read>(replay: &mut Replay<R>) -> Counts {
    let mut counts = Counts::default();
    while let Some(record) = replay.next_record().unwrap() {
        match record {
            Record::SnapOrder(_) => counts.snap_orders += 1,
            Record::Block(_) => counts.blocks += 1,
            Record::Order(_) => counts.orders += 1,
            Record::Trade(_) => {
                counts.trades += 1;
                if let Some(kind) = replay.last_trade_kind() {
                    *counts.kinds.entry(kind).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    counts
}

fn sides(book: &OrderBook) -> (usize, usize) {
    (book.order_count(Side::Bid), book.order_count(Side::Ask))
}

#[test]
fn zec_order_log() {
    let Some(path) = sample("ZEC_HLC_20261014_sample.bin.lz4") else {
        return;
    };
    let mut replay = Replay::open(&path).unwrap();
    let counts = count(&mut replay);
    assert_eq!(
        (
            counts.snap_orders,
            counts.blocks,
            counts.orders,
            counts.trades
        ),
        (10_134, 19_764, 306_433, 2_815)
    );
    assert!(counts.kinds.is_empty());
    assert_eq!(replay.accounts().len(), 1_799);
    let meta = replay.meta().unwrap();
    assert_eq!(
        (meta.coin.as_str(), meta.day, meta.is_outcome()),
        ("ZEC", 20261014, false)
    );
    let book = replay.book();
    assert_eq!(sides(book), (6_625, 3_409));
    assert_eq!(book.orders().filter(|o| o.side == Side::Bid).count(), 6_625);
    assert_eq!(
        (book.best_bid(), book.best_ask()),
        (Some(123_100_000_000), Some(123_110_000_000))
    );
    let front = book
        .queue(Side::Bid, book.best_bid().unwrap())
        .next()
        .unwrap();
    assert_eq!(front.size, 1_624_000_000);
    assert_eq!(
        replay.address(front.user_id).unwrap().to_string(),
        "0x41a914768eda813d6d4b2ef5a3cdcb872a496a32"
    );
    assert_eq!(
        UtcDateTime::from_unix_ns(replay.block().unwrap().time_ns as i64).to_string()[..19],
        *"2026-10-14 00:29:59"
    );
}

#[test]
fn zec_run_until_applies_blocks_and_runs_on_in_steps() {
    let Some(path) = sample("ZEC_HLC_20261014_sample.bin.lz4") else {
        return;
    };
    let moment = UtcDateTime::new(2026, 10, 14, 0, 15, 0, 0)
        .unwrap()
        .to_ticks();
    let mut stepped = Replay::open(&path).unwrap();
    stepped.run_until(moment).unwrap();
    // USER records carry the time of later blocks: they must not stop the replay.
    let block = *stepped.block().expect("blocks before 00:15 were applied");
    assert!(block.time <= moment);
    assert!(
        block.time > moment - 10_000_000,
        "the last block is within a second of 00:15"
    );
    let at_moment = (
        stepped.book().len(),
        stepped.book().best_bid(),
        stepped.book().best_ask(),
    );

    // The same moment again changes nothing; the next record is the block after it.
    stepped.run_until(moment).unwrap();
    assert_eq!(stepped.block().unwrap().number, block.number);
    match stepped.next_record().unwrap() {
        Some(Record::Block(next)) => assert!(next.time > moment && next.number > block.number),
        other => panic!("expected the next block, got {other:?}"),
    }
    stepped.run().unwrap();

    let mut whole = Replay::open(&path).unwrap();
    whole.run().unwrap();
    assert_eq!(stepped.block(), whole.block());
    assert_eq!(stepped.accounts(), whole.accounts());
    let (a, b) = (stepped.book(), whole.book());
    assert_eq!(
        (a.len(), a.best_bid(), a.best_ask()),
        (b.len(), b.best_bid(), b.best_ask())
    );
    assert_eq!(
        a.depth(Side::Bid, usize::MAX),
        b.depth(Side::Bid, usize::MAX)
    );
    assert_eq!(
        a.depth(Side::Ask, usize::MAX),
        b.depth(Side::Ask, usize::MAX)
    );
    for order in b.orders() {
        assert_eq!(a.get(order.oid).map(|o| o.size), Some(order.size));
        assert_eq!(a.queue_position(order.oid), b.queue_position(order.oid));
    }

    // A second run to the moment, from a fresh replay, gives the same book.
    let mut again = Replay::open(&path).unwrap();
    again.run_until(moment).unwrap();
    let book = again.book();
    assert_eq!((book.len(), book.best_bid(), book.best_ask()), at_moment);
}

#[test]
fn hip4_outcome_market_with_both_books() {
    let Some(path) = sample("HIP4_3600_Fed-Oct-no-change_HLC_20261023.bin.lz4") else {
        return;
    };
    let mut replay = Replay::open(&path).unwrap();
    let counts = count(&mut replay);
    assert_eq!(
        (
            counts.snap_orders,
            counts.blocks,
            counts.orders,
            counts.trades
        ),
        (241, 16_537, 23_765, 3_279)
    );
    let kinds: Vec<(TradeKind, usize)> = counts.kinds.into_iter().collect();
    assert_eq!(
        kinds,
        vec![
            (TradeKind::FirstBook, 183),
            (TradeKind::SecondBook, 1_810),
            (TradeKind::Mint, 552),
            (TradeKind::Merge, 734),
        ]
    );
    assert_eq!(replay.accounts().len(), 82);
    let meta = replay.meta().unwrap();
    assert!(meta.is_outcome() && meta.flags == 1);
    let info = replay.outcome().unwrap();
    assert_eq!(info.outcome, Some(3600));
    assert!(info.title.ends_with("No change"), "{}", info.title);
    assert_eq!(info.sides, ["Yes".to_string(), "No".to_string()]);
    let book = replay.book();
    assert_eq!(sides(book), (213, 39));
    assert_eq!(
        (book.best_bid(), book.best_ask()),
        (Some(93_451_000), Some(93_452_000))
    );
    let second = book
        .orders()
        .filter(|o| o.flags & ORDER_SECOND_SIDE != 0)
        .count();
    assert!(second > 0 && second < book.len());

    // HIP-4 markets allow no priority fees, so every fill takes the order at
    // the front of its own book's queue: queues are kept per side book.
    let fronts = maker_fills_at_the_front(&path);
    assert_eq!(fronts, (3_278, 3_278, 1_759));
}

/// `(maker fills, makers at the front of their own book's queue, makers that
/// would be at the front of one queue of both books)`, for the fills of
/// resting orders other than system prints.
fn maker_fills_at_the_front(path: &std::path::Path) -> (usize, usize, usize) {
    let mut book = OrderBook::new();
    let (mut fills, mut own, mut both) = (0, 0, 0);
    for record in Reader::open(path).unwrap() {
        let record = record.unwrap();
        if let Record::Trade(trade) = &record {
            let maker = trade.has_maker_oid() && !trade.is_system_print();
            if let Some(maker) = book.get(trade.maker_oid).filter(|o| maker && o.size > 0) {
                fills += 1;
                own += usize::from(book.queue_position(maker.oid).unwrap().0 == 0);
                let first = book.queue(maker.side, maker.price).find(|o| o.size > 0);
                both += usize::from(first.map(|o| o.oid) == Some(maker.oid));
            }
        }
        book.apply(&record);
    }
    (fills, own, both)
}

fn l2_day(path: &std::path::Path) -> (usize, usize, L2Book) {
    let mut book = L2Book::new();
    let (mut depth, mut trades) = (0, 0);
    for event in l2::Reader::open(path).unwrap() {
        match event.unwrap() {
            Event::Depth(update) => {
                depth += 1;
                book.apply(&update);
            }
            Event::Trade(_) => trades += 1,
            other => panic!("unexpected event {other:?}"),
        }
    }
    (depth, trades, book)
}

#[test]
fn l2_whole_day() {
    let Some(path) = sample("BTC_HYPERLIQUID_20261101.bin.lz4") else {
        return;
    };
    let (depth, trades, book) = l2_day(&path);
    assert_eq!((depth, trades), (489_807, 163_895));
    assert_eq!(book.best_bid(), Some((7_832_600_000_000, 150_219_000)));
    assert_eq!(book.best_ask(), Some((7_832_700_000_000, 1_010_900_000)));
    assert_eq!((book.bids().len(), book.asks().len()), (20, 20));
}

#[test]
fn l2_depth_replay_sample() {
    let Some(path) = sample("BTC_HYPERLIQUID_20260622_sample.bin.lz4") else {
        return;
    };
    let (depth, trades, book) = l2_day(&path);
    assert_eq!(depth + trades, 347_278);
    assert_eq!(book.best_bid().unwrap().0, 6_405_500_000_000);
    assert_eq!(book.best_ask().unwrap().0, 6_405_600_000_000);
    assert_eq!((book.bids().len(), book.asks().len()), (210, 286));
}

/// The 88 MB (MB of 1024×1024 bytes, as on alphaquick.io) free sample `xyz_CL_HLC_20261101.bin.lz4` (not downloaded by CI).
#[test]
fn xyz_cl_whole_day() {
    let Some(path) = sample("xyz_CL_HLC_20261101.bin.lz4") else {
        return;
    };
    let mut replay = Replay::open(&path).unwrap();
    let counts = count(&mut replay);
    assert_eq!(
        (counts.snap_orders, counts.orders, counts.trades),
        (5_503, 4_408_655, 42_128)
    );
    assert_eq!(replay.accounts().len(), 1_681);
    let book = replay.book();
    assert_eq!(
        (book.best_bid(), book.best_ask()),
        (Some(10_224_000_000), Some(10_225_000_000))
    );
    assert_eq!(sides(book), (3_299, 2_074));
}

#[test]
fn a_moment_before_the_day_gives_the_day_start_book() {
    let Some(path) = sample("HIP4_3600_Fed-Oct-no-change_HLC_20261023.bin.lz4") else {
        return;
    };
    let mut replay = Replay::open(&path).unwrap();
    replay.run_until(0).unwrap();
    assert_eq!(replay.book().len(), 241);
    assert!(replay.block().is_some(), "the seed block is applied");
    let mut whole = Replay::open(&path).unwrap();
    whole.run().unwrap();
    replay.run().unwrap();
    assert_eq!(replay.book().len(), whole.book().len());
    assert_eq!(
        replay.book().depth(Side::Bid, usize::MAX),
        whole.book().depth(Side::Bid, usize::MAX)
    );
}

/// The order-log files of the current build of 17 November 2026
/// (`hyperliquid/hlc_v2/20261117/`), not free samples.
const PURR_V2: &str = "PURR_USDC_HLC_20261117.bin.lz4";
const HIP4_V2: &str = "HIP4_1473_2026-2027-English-Premier-League-winner-A_HLC_20261117.bin.lz4";

/// `(blocks, blocks whose book is crossed at their end)`, the book at the
/// end of the file and each order's last status.
fn crossed_blocks(path: &std::path::Path) -> (usize, usize, OrderBook, HashMap<u64, u8>) {
    let crossed = |book: &OrderBook| matches!((book.best_bid(), book.best_ask()), (Some(b), Some(a)) if b >= a);
    let mut book = OrderBook::new();
    let mut last_status = HashMap::new();
    let (mut blocks, mut crossings) = (0, 0);
    for record in Reader::open(path).unwrap() {
        let record = record.unwrap();
        match &record {
            Record::Block(block) => {
                blocks += 1;
                // The files carry no time of receipt: the block time twice.
                assert_eq!(block.local_time_ns, block.time_ns);
                if blocks > 1 {
                    crossings += usize::from(crossed(&book)); // the end of the block before
                }
            }
            Record::Order(order) => {
                last_status.insert(order.oid, order.status.code());
            }
            _ => {}
        }
        book.apply(&record);
    }
    crossings += usize::from(crossed(&book));
    (blocks, crossings, book, last_status)
}

/// Regression for the statuses added in 0.2.0: with the 0.1.0 set, 279
/// orders cancelled with `insufficientWeiCanceled` (27) stayed in the book
/// and crossed it at the end of 168,217 of the 199,677 blocks.
#[test]
fn purr_usdc_v2_is_never_crossed() {
    let Some(path) = sample(PURR_V2) else {
        return;
    };
    let (blocks, crossings, book, last_status) = crossed_blocks(&path);
    assert_eq!((blocks, crossings), (199_677, 0));
    let wei_cancelled = last_status.values().filter(|&&s| s == 27).count();
    assert_eq!(wei_cancelled, 279);
    for order in book.orders() {
        let status = last_status.get(&order.oid).copied();
        assert!(
            !matches!(status, Some(s) if alphaquick::orderlog::Status(s).removes_order()),
            "order {} rests after status {status:?}",
            order.oid
        );
    }
    assert_eq!(book.len(), 4_671);
    let meta = Replay::open(&path).unwrap().next_record().unwrap();
    let Some(Record::Meta(meta)) = meta else {
        panic!("{meta:?}")
    };
    assert_eq!((meta.coin.as_str(), meta.dex), ("PURR/USDC", DEX_SPOT));
}

#[test]
fn hip4_v2_outcome_file() {
    let Some(path) = sample(HIP4_V2) else {
        return;
    };
    let name = names::parse_orderlog_name(&path).unwrap();
    assert_eq!((name.market.as_str(), name.day), ("outcome", 20261117));
    let mut replay = Replay::open(&path).unwrap();
    let (mut prints, mut print_kinds) = (0, BTreeMap::new());
    while let Some(record) = replay.next_record().unwrap() {
        if let Record::Trade(trade) = record {
            if trade.is_system_print() {
                prints += 1;
                *print_kinds.entry(replay.last_trade_kind()).or_insert(0) += 1;
            }
        }
    }
    let meta = replay.meta().unwrap();
    assert_eq!(
        (meta.coin.as_str(), meta.dex, meta.asset, meta.size_decimals),
        ("HIP4:1473", DEX_SPOT, 65_535, 0)
    );
    assert!(meta.is_outcome());
    assert_eq!(replay.outcome().unwrap().outcome, Some(1473));
    // System prints have no maker in the book.
    assert_eq!(prints, 136);
    assert_eq!(
        print_kinds.into_iter().collect::<Vec<_>>(),
        [(Some(TradeKind::Unknown), 136)]
    );
    let (blocks, crossings, _, _) = crossed_blocks(&path);
    assert_eq!((blocks, crossings), (94_162, 0));
    let (fills, own, both) = maker_fills_at_the_front(&path);
    assert_eq!((fills, own, both), (6_825, 6_815, 3_235));
}
