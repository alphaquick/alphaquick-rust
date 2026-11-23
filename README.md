# alphaquick

A Rust library that reads AlphaQuick market data files (`.bin.lz4`):

- **L2 depth & trades**: every depth update and trade print of one
  instrument for one UTC day, from Binance, Bybit, OKX, Hyperliquid and other
  venues. File names look like `BTCUSDT_BINANCE_USDM_20261115.bin.lz4`.
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
use alphaquick::L2Book;

fn main() -> alphaquick::Result<()> {
    let mut book = L2Book::new();
    let mut trades = 0;
    for event in l2::Reader::open("BTC_HYPERLIQUID_20261101.bin.lz4")? {
        match event? {
            Event::Depth(update) => book.apply(&update),
            Event::Trade(_) => trades += 1,
            _ => {} // kinds a later version may add
        }
    }
    println!("{trades} trades");
    println!("best bid {:?}, best ask {:?}", book.best_bid(), book.best_ask());
    Ok(())
}
```

`l2::book_at(reader, ticks, uncross)` gives the book at a moment. Trades
whose price or volume is not positive are skipped, as the format says; the
reader's `include_non_market(true)` keeps them.

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

`Replay` applies the format's rules for rebuilding the book, keeps the
accounts, the current block and the file's description, and can be run on in
steps: each `run_until`, `run` or `next_record` continues where the last one
stopped. `run_until` stops before the first BLOCK later than the moment,
except the file's first BLOCK (the seed block the day-start orders follow),
so a moment before the day gives the day-start book. Orders filled down to
size 0, which stay listed until their removal later in the same block, are
not counted by `depth` and `queue_position`.
For a HIP-4 outcome market with both books in one file,
`replay.last_trade_kind()` says whether a trade exchanged tokens on one book,
minted a pair or merged one, and `outcome::as_placed` turns an order back
into the side and price it was placed at.

### Lower levels

- `orderlog::Reader` and `l2::Reader` iterate over the decoded records;
  `OrderBook` and `L2Book` can be fed by hand.
- `container::Blocks` and `container::RawRecords` give the decompressed
  blocks and the raw records, for record kinds of your own.
- `names` reads file names (`parse_l2_name`, `parse_orderlog_name`),
  `detect` tells the dataset from a file's first record, and `time` converts
  the 100 ns ticks the records use.

## Examples

```sh
# What a file holds: counts, time range, the book at the end
cargo run --release --example info -- ZEC_HLC_20261014_sample.bin.lz4

# The book at 00:15 UTC on the file's day, ten levels, the first five orders at each best price
cargo run --release --example book_at -- ZEC_HLC_20261014_sample.bin.lz4 00:15 --levels 10 --queue 5
```

Both work with L2 and order-log files. Free sample files to try them on are
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

## License

MIT, see [LICENSE](LICENSE).
