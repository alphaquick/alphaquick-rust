# Changelog

## 0.2.0 (unreleased)

Changes to follow the current build of the order log (`hyperliquid/hlc_v2/`)
and the L2 recorder used from 26 November 2026.

### Order log

- **Statuses 24 to 28**: `vaultWithdrawalCanceled` (24), `delistedCanceled`
  (26) and `insufficientWeiCanceled` (27) remove the order;
  `perpMaxPositionRejected` (25) and `priorityAloReduceOnlyRejected` (28) are
  rejects. 0.1.0 kept orders cancelled with 24, 26 or 27 in the book: on
  PURR/USDC on 17 November 2026, 279 orders cancelled with 27 stayed and
  crossed the book at the end of 168,217 of its 199,677 blocks (none now).
  New: the `Status` constants, `REMOVING_STATUSES`, `REJECT_STATUSES`.
- Order flags `ORDER_RO_CAP` (0x10), `ORDER_REBUILT` (0x20) and
  `ORDER_SNAPSHOT_FIX` (0x40), and `Order::is_ro_cap`, `is_rebuilt` and
  `is_snapshot_fix`. The docs of 250 to 252 say what each is: most are not
  corrections (rebuilt orders of system accounts, triggered stops that start
  resting, reduce-only cap shrinks, which follow the block's trades with the
  size after them; only those with 0x40 are corrections against a node
  snapshot). The book is applied in file order, as before, which is right for
  the cap shrinks.
- `Trade::is_system_print` and `TRADE_DIRECTION_SYSTEM`: direction override
  3, a trade booked without a match in the book. Leave them out of volume and
  price statistics when comparing with exchange candles.
- HIP-4 outcome files: `OrderBook::queue_position` counts only the orders of
  the order's own side book (`ORDER_SECOND_SIDE`), since an order of one
  book is never ahead of an order of the other; new
  `OrderBook::queue_of_book` and `RestingOrder::on_second_book`. Other files
  never set that flag and are not affected.
- Docs: META dex (`DEX_SPOT`, 254, for spot and HIP-4), asset 65535 and coin
  `HIP4:<id>` in HIP-4 files; BLOCK local time equals the block time (no time
  of receipt is published); the 400 ms limit of queue order (priority fees).
- Names: `HIP4_<id>` without a short name is an outcome market (0.1.0 read
  it as a spot pair); outcome files live in `<day>/hip4/`.

### L2

- **Continuity files and gaps**: new module `continuity` (`read`, `parse`,
  `Line`, `path_of`), reading every line with all its fields, unknown kinds
  and fields included, and leaving out a torn last line.
  `l2::Reader::with_continuity` merges the gaps into the events as
  `Event::Gap(Gap)` (new variant), placed before the first record at or
  after the gap's time and never past the Clear that ends it.
  `L2Book::apply_gap`, `L2Book::apply_event`, `L2Book::is_known` and
  `L2Book::gap`: a book is not known from a gap to the next Clear.
- `book_at` stops at the first depth update after the moment; a trade
  stamped later no longer ends the rebuild early. New `Reader::book_at`, to
  use it with a continuity file.
- Names: the venue `HYPERLIQUID_BBO` (`BTC_HYPERLIQUID_BBO_…` was read as
  the instrument `BTC_HYPERLIQUID` on a venue `bbo`).
- Docs: the trade id is the recorder's number of the trade in the file, not
  the venue's id, and restarts after a recorder restart in files up to
  2026-11-25; only sides 1 and 2 are written (old `LIGHTER` files have 1 on
  every trade); the stale levels of old `BINANCE_USDM` and `ASTER` files and
  what uncrossing does and does not hide; prices and sizes have 6 decimal
  places in files up to 2026-11-25.
- `time::unix_ms_to_ticks` and `ticks_to_unix_ms`.
- The examples read the continuity file next to an L2 file, and `book_at`
  says when the moment is in a gap; for an outcome market it lists the
  queue of each side's book.

## 0.1.0 (unreleased)

First version.

- `container`: the frames and blocks of a `.bin.lz4` file, from any `std::io::Read`, and its raw records.
- `l2`: depth updates and trades, the price-level book `L2Book` (with an optional uncross rule), `book_at`.
- `orderlog`: the order-log records, the order-level book `OrderBook` with the queue at every price, and `Replay`, which applies the records, keeps accounts, blocks and the file's description, runs up to a moment and on in steps.
- `outcome`: HIP-4 outcome markets with both books in one file: the market's description, orders as placed, and what each trade did (one book, mint or merge).
- `names`: what a file is, from its name. `time`: ticks, Unix time, calendar dates and fixed-point numbers.
- Examples `info` and `book_at`.
