# Changelog

## 0.1.0 (unreleased)

First version.

- `container`: the frames and blocks of a `.bin.lz4` file, from any `std::io::Read`, and its raw records.
- `l2`: depth updates and trades, the price-level book `L2Book` (with an optional uncross rule), `book_at`.
- `orderlog`: the order-log records, the order-level book `OrderBook` with the queue at every price, and `Replay`, which applies the records, keeps accounts, blocks and the file's description, runs up to a moment and on in steps.
- `outcome`: HIP-4 outcome markets with both books in one file: the market's description, orders as placed, and what each trade did (one book, mint or merge).
- `names`: what a file is, from its name. `time`: ticks, Unix time, calendar dates and fixed-point numbers.
- Examples `info` and `book_at`.
