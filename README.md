# alphaquick

A Rust library that reads AlphaQuick market data files (`.bin.lz4`):

- **L2 depth & trades**: every depth update and trade print of one
  instrument for one UTC day, from Binance, Bybit, OKX, Hyperliquid and other
  venues. File names look like `BTCUSDT_BINANCE_USDM_20261115.bin.lz4`; the
  continuity file next to each (`….continuity.jsonl`) says where the feed
  broke and the book is not known.
- **Hyperliquid order log**: every order and every trade of one Hyperliquid
  instrument for a day, with accounts and queue positions (Level 3). File
  names look like `BTC_HLC_20261101.bin.lz4`. This covers main perpetuals,
  HIP-3 markets, spot and HIP-4 outcome markets.

The crate reads the container, decodes the records and rebuilds the books:
a price-level book for L2 files, and an order-level book with the queue at
every price for the order log. Prices and sizes stay integers scaled by 10⁸,
exactly as the files store them. It reads a file one frame at a time, so
memory stays small whatever the file's size: a full replay of the 88 MB (MB of 1024×1024 bytes, as on alphaquick.io) order
log of `xyz:CL` for one day (4.4 million order events) takes about half a
second.

The only dependency is [`lz4_flex`](https://crates.io/crates/lz4_flex), and
the crate has no unsafe code. Minimum Rust version: 1.81.

## Install

Until the crate is published on crates.io, add it from GitHub:

```toml
[dependencies]
alphaquick = { git = "https://github.com/alphaquick/alphaquick-rust" }
```

## Quick start

### L2 depth & trades

```rust,no_run
use alphaquick::l2::{self, Event};
use alphaquick::{continuity, L2Book};

fn main() -> alphaquick::Result<()> {
    let data = "BTCUSDT_BINANCE_USDM_20261126.bin.lz4";
    // Where the feed broke: BTCUSDT_BINANCE_USDM_20261126.continuity.jsonl
    let lines = continuity::read(continuity::path_of(data))?;
    let mut book = L2Book::new();
    let mut trades = 0;
    for event in l2::Reader::open(data)?.with_continuity(&lines) {
        match event? {
            Event::Depth(update) => book.apply(&update),
            Event::Gap(gap) => book.apply_gap(&gap), // not known until the next Clear
            Event::Trade(_) => trades += 1,
            _ => {} // kinds a later version may add
        }
    }
    println!("{trades} trades");
    println!("best bid {:?}, best ask {:?}", book.best_bid(), book.best_ask());
    if !book.is_known() {
        println!("the file ends in a gap: that book is out of date");
    }
    Ok(())
}
```

A `gap` line of the continuity file (from 26 November 2026) means the feed
broke: the data file then holds no depth updates until the next Clear (a
fresh copy of the book), only trades, so a rebuilt book stays frozen at its
last state. `Reader::with_continuity` merges the gaps into the events
(`Event::Gap`), placed by time between the Clears the continuity file's seed
lines describe, and `L2Book::is_known()` is `false` from a gap to the next
Clear. Read the book as the venue's only when
`book.is_consistent() && book.is_known()`. Without the continuity file a
reader cannot tell a stale book from a current one. `continuity::read` gives
every line with all its fields, and accepts a last line the recorder was
still writing.

`Reader::book_at(ticks, uncross)` (or `l2::book_at(reader, ticks, uncross)`
for the data file alone) gives the book at a moment; check `book.gap()`
before you present it as the book at that moment. Trades whose price or
volume is not positive are skipped, as the format says; the reader's
`include_non_market(true)` keeps them. A trade's `trade_id` is its number
in the file, given by the recorder, not the venue's trade id.

#### Files up to 25 November 2026

These were written by the previous recorder:

- **`BINANCE_USDM` and `ASTER` books are not reliable**: the venues' depth
  diffs were applied without their snapshot and sequence procedure, so levels
  that should have been removed stay, and the rebuilt book is crossed.
  `L2Book::with_uncross(true)` (as Depth Replay does) hides the crossed part
  near the touch, but stale levels deeper in the book remain. Files from
  26 November 2026 are sequence-checked and seeded again after every gap.
- Prices and sizes have **6 decimal places** (digits beyond 10⁻⁶ were cut
  off); from 26 November 2026 they are exact.
- The trade number starts again at 1 after each recorder restart, so it can
  repeat within a file. `LIGHTER` files mark every trade as a buy (side 1).
- The continuity files have no `gap` lines: lost messages were not detected.

### Hyperliquid order log

```rust,no_run
use alphaquick::orderlog::{Record, Replay};
use alphaquick::time::{format_fixed, UtcDateTime, TICKS_PER_SECOND};
use alphaquick::Side;

fn main() -> alphaquick::Result<()> {
    let mut replay = Replay::open("ZEC_HLC_20261014_sample.bin.lz4")?;

    // The book at 00:15 UTC ...
    let midnight = UtcDateTime::from_yyyymmdd(20261014).unwrap().to_ticks();
    replay.run_until(midnight + 15 * 60 * TICKS_PER_SECOND)?;
    let book = replay.book();
    if let Some(best) = book.best_bid() {
        println!("best bid {}", format_fixed(best));
        for order in book.queue(Side::Bid, best).take(3) {
            let address = replay.address(order.user_id).map_or("?".into(), |a| a.to_string());
            println!("  {} {address}", format_fixed(order.size));
        }
    }

    // ... then on to the end, record by record.
    let mut trades = 0;
    while let Some(record) = replay.next_record()? {
        if let Record::Trade(_) = record {
            trades += 1;
        }
    }
    println!("{trades} trades after 00:15");
    Ok(())
}
```

`Replay` applies the format's rules for rebuilding the book, record by record
in file order, keeps the accounts, the current block and the file's
description, and can be run on in steps: each `run_until`, `run` or
`next_record` continues where the last one stopped. `run_until` stops before
the first BLOCK later than the moment, except the file's first BLOCK (the
seed block the day-start orders follow), so a moment before the day gives the
day-start book. Orders filled down to size 0, which stay listed until their
removal later in the same block, are not counted by `depth` and
`queue_position`.

- The statuses that remove an order are `orderlog::REMOVING_STATUSES` (1, 2,
  9, 13, 15, 17, 18, 19, 20, 22, 24, 26, 27 and the converter's 250).
- Codes 250 to 252 are written by AlphaQuick's converter where the node logs
  nothing for a change the exchange made; their flags say why: a rebuilt
  order of a system account (251 with `ORDER_REBUILT`), a triggered stop or
  take-profit that starts resting (251 without flags), a reduce-only cap
  shrink after the block's trades (252 with `ORDER_RO_CAP`), or a rare
  correction against a node snapshot (`ORDER_SNAPSHOT_FIX`).
- `Trade::is_system_print()`: a trade booked without a match in the book
  (HIP-4 settlements and outcome actions, the settlement of a delisted
  market, lending-pool liquidations, vault nettings). Hyperliquid's candles
  do not count them: leave them out of volume and price statistics if you
  compare with those.
- Queue order is the order of the open events. It is exact except among
  orders at the same price placed within 400 ms of each other, where
  Hyperliquid's priority fees can put a later post-only order first (the fee
  is not in the data). HIP-4 markets allow no priority fees.
- `Block::local_time_ns` equals the block time: the files carry no time of
  receipt, so add your own feed delay to model when data could be seen.

For a HIP-4 outcome market with both books in one file (META flag 1, coin
`HIP4:<outcome id>`, file `HIP4_<id>_<short name>_HLC_<day>.bin.lz4` in the
day's `hip4/` folder), `replay.last_trade_kind()` says whether a trade
exchanged tokens on one book, minted a pair or merged one, and
`outcome::as_placed` turns an order back into the side and price it was
placed at. Both books share the price levels, but an order of one book is
never ahead of an order of the other: `queue_position` counts only the
order's own book, and `queue_of_book` lists one book's queue.

### Lower levels

- `orderlog::Reader` and `l2::Reader` iterate over the decoded records;
  `OrderBook` and `L2Book` can be fed by hand.
- `container::Blocks` and `container::RawRecords` give the decompressed
  blocks and the raw records, for record kinds of your own.
- `names` reads file names (`parse_l2_name`, `parse_orderlog_name`),
  `detect` tells the dataset from a file's first record, and `time` converts
  the 100 ns ticks the records use.
- `continuity` reads continuity files: `read`, `parse`, `Line::parse`.

## Examples

```sh
# What a file holds: counts, time range, the book at the end
cargo run --release --example info -- ZEC_HLC_20261014_sample.bin.lz4

# The book at 00:15 UTC on the file's day, ten levels, the first five orders at each best price
cargo run --release --example book_at -- ZEC_HLC_20261014_sample.bin.lz4 00:15 --levels 10 --queue 5
```

Both work with L2 and order-log files. For an L2 file they read the
continuity file next to it when there is one, and `book_at` says so when the
moment is in a gap. Free sample files to try them on are
listed on the format pages below, for example
[ZEC_HLC_20261014_sample.bin.lz4](https://alphaquick.io/orderlog/sample/ZEC_HLC_20261014_sample.bin.lz4)
and
[BTC_HYPERLIQUID_20261101.bin.lz4](https://alphaquick.io/api/data/samples/l2-depth-trades/BTC_HYPERLIQUID_20261101.bin.lz4).

## Formats

- [L2 Depth & Trades History](https://docs.alphaquick.io/data/l2-depth-trades): the container and the L2 records
- [Hyperliquid Order Log History](https://docs.alphaquick.io/data/hyperliquid-order-log): the order-log records, codes, outcome markets and the rules for rebuilding the book
- [Code examples](https://docs.alphaquick.io/data/examples): other languages and tools

The same reader for Python: [alphaquick-python](https://github.com/alphaquick/alphaquick-python).

## Tests

```sh
cargo test
```

The unit tests write small synthetic files. The tests in `tests/samples.rs`
read the free sample files listed in `tests/samples.txt`, with their URLs and
SHA-256, and run when `ALPHAQUICK_SAMPLES_DIR` names a directory that holds
them:

```sh
mkdir -p samples && cd samples
while read -r sum name url; do curl -fsSL -o "$name" "$url"; done < ../tests/samples.txt
cd .. && ALPHAQUICK_SAMPLES_DIR=samples cargo test
```

Each sample test is skipped when its file is missing. `zec_order_log`,
`zec_run_until_applies_blocks_and_runs_on_in_steps`,
`hip4_outcome_market_with_both_books`,
`a_moment_before_the_day_gives_the_day_start_book`, `l2_whole_day` and
`l2_depth_replay_sample` read the pinned samples. `xyz_cl_whole_day`,
`purr_usdc_v2_is_never_crossed` and `hip4_v2_outcome_file` read larger
files that are not free samples (`xyz_CL_HLC_20261101`, and
`PURR_USDC_HLC_20261117` and `HIP4_1473_…_HLC_20261117` from
`hyperliquid/hlc_v2/20261117/`); put them in the same directory to run them.

## License

MIT, see [LICENSE](LICENSE).
