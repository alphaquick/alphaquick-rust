//! The book at a moment of the file's day (UTC), or at the end of the file.
//!
//! ```text
//! cargo run --release --example book_at -- FILE.bin.lz4 [HH:MM[:SS]] [--levels N] [--queue N]
//! ```
//!
//! For an L2 file it reads the continuity file next to it, when there is one,
//! and says so when the moment is in a gap: the feed broke and the book shown
//! is the last known, out of date. For an order-log file it also lists the
//! first orders in the queue at the best bid and the best ask, with their
//! accounts (for a HIP-4 outcome market, the queue of each side's book).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::ExitCode;

use alphaquick::orderlog::{self, Record, Replay};
use alphaquick::time::{format_fixed, UtcDateTime, TICKS_PER_SECOND};
use alphaquick::{continuity, l2, names, FileKind, L2Book, Side};

const USAGE: &str = "usage: book_at FILE.bin.lz4 [HH:MM[:SS]] [--levels N] [--queue N]";

struct Args {
    path: String,
    at: Option<String>,
    levels: usize,
    queue: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        path: String::new(),
        at: None,
        levels: 10,
        queue: 5,
    };
    let mut rest = std::env::args().skip(1);
    let mut positional = Vec::new();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--levels" | "--queue" => {
                let value = rest.next().ok_or(USAGE)?;
                let n: usize = value
                    .parse()
                    .map_err(|_| format!("{arg}: not a number: {value}"))?;
                if arg == "--levels" {
                    args.levels = n;
                } else {
                    args.queue = n;
                }
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    args.path = positional.next().ok_or(USAGE)?;
    args.at = positional.next();
    if positional.next().is_some() {
        return Err(USAGE.to_string());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("book_at: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `HH:MM`, `HH:MM:SS` or `HH:MM:SS.fff` as ticks after midnight.
fn time_of_day(text: &str) -> Option<i64> {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) || (fraction.is_some() && parts.len() != 3) {
        return None;
    }
    let number = |s: &str, max: i64| -> Option<i64> {
        let ok = !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit());
        ok.then(|| s.parse().ok()).flatten().filter(|&n| n < max)
    };
    let hours = number(parts[0], 24)?;
    let minutes = number(parts[1], 60)?;
    let seconds = parts.get(2).map_or(Some(0), |s| number(s, 60))?;
    let mut ticks = ((hours * 60 + minutes) * 60 + seconds) * TICKS_PER_SECOND;
    if let Some(fraction) = fraction {
        if fraction.is_empty()
            || fraction.len() > 7
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        ticks += format!("{fraction:0<7}").parse::<i64>().ok()?;
    }
    Some(ticks)
}

/// The UTC day of a file: from its name, or else from its first record.
fn file_day(path: &Path, kind: FileKind) -> Result<UtcDateTime, Box<dyn std::error::Error>> {
    let from_name = match kind {
        FileKind::OrderLog => names::parse_orderlog_name(path).map(|n| n.day),
        FileKind::L2 => names::parse_l2_name(path).map(|n| n.day),
        _ => None,
    };
    if let Some(day) = from_name.and_then(UtcDateTime::from_yyyymmdd) {
        return Ok(day);
    }
    let first = match kind {
        FileKind::OrderLog => match orderlog::Reader::open(path)?.next().transpose()? {
            Some(Record::Meta(meta)) => UtcDateTime::from_yyyymmdd(meta.day),
            _ => None,
        },
        FileKind::L2 => l2::Reader::open(path)?
            .next()
            .transpose()?
            .map(|event| UtcDateTime::from_ticks(event.time())),
        _ => None,
    };
    let day = first.ok_or("the file's day is not known")?;
    Ok(day.start_of_day())
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(&args.path);
    let kind = match names::file_kind(path) {
        Some(kind) => kind,
        None => alphaquick::detect(File::open(path)?)?.ok_or("the file holds no records")?,
    };
    let until = match &args.at {
        Some(text) => {
            let offset = time_of_day(text).ok_or_else(|| format!("not a time of day: {text}"))?;
            let moment = file_day(path, kind)?.to_ticks() + offset;
            println!("book at {} UTC", UtcDateTime::from_ticks(moment));
            Some(moment)
        }
        None => {
            println!("book at the end of the file");
            None
        }
    };
    match kind {
        FileKind::L2 => l2_book(path, until, args),
        FileKind::OrderLog => orderlog_book(path, until, args),
        _ => Err("a kind of file this example does not know".into()),
    }
}

fn l2_book(path: &Path, until: Option<i64>, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // The continuity file says where the feed broke; without it a gap cannot be seen.
    let sidecar = continuity::path_of(path);
    let lines = if sidecar.is_file() {
        Some(continuity::read(&sidecar)?)
    } else {
        None
    };
    let mut reader = l2::Reader::new(BufReader::new(File::open(path)?));
    if let Some(lines) = &lines {
        reader = reader.with_continuity(lines);
    }
    let book = match until {
        Some(until) => reader.book_at(until, false)?,
        None => {
            let mut book = L2Book::new();
            for event in reader {
                book.apply_event(&event?);
            }
            book
        }
    };
    match (&lines, book.gap()) {
        (_, Some(gap)) => println!(
            "NOT KNOWN: the feed broke at {} UTC ({}) and the book was not seeded again \
             before this moment; the levels below are the last known, out of date",
            UtcDateTime::from_ticks(gap.time),
            gap.reason
        ),
        (None, None) => println!(
            "(no {} next to the file: whether the moment is in a gap is not known)",
            sidecar.file_name().unwrap_or_default().to_string_lossy()
        ),
        (Some(_), None) => {}
    }
    let (bids, asks) = book.depth(args.levels);
    let row = |level: Option<&(i64, i64)>| level.map(|&(price, size)| (price, size, None));
    ladder((0..bids.len().max(asks.len())).map(|i| (row(bids.get(i)), row(asks.get(i)))));
    if !book.is_consistent() {
        println!("(the book is in the middle of a transaction at this moment)");
    }
    Ok(())
}

fn orderlog_book(
    path: &Path,
    until: Option<i64>,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut replay = Replay::open(path)?;
    match until {
        Some(until) => replay.run_until(until)?,
        None => replay.run()?,
    }
    if let Some(block) = replay.block() {
        println!(
            "last block applied: {} at {} UTC",
            block.number,
            UtcDateTime::from_unix_ns(block.time_ns as i64)
        );
    } else {
        println!("no block applied yet: the book at 00:00 UTC");
    }
    let book = replay.book();
    let bids = book.depth(Side::Bid, args.levels);
    let asks = book.depth(Side::Ask, args.levels);
    let row =
        |level: Option<&orderlog::PriceLevel>| level.map(|l| (l.price, l.size, Some(l.orders)));
    ladder((0..bids.len().max(asks.len())).map(|i| (row(bids.get(i)), row(asks.get(i)))));
    if args.queue == 0 {
        return Ok(());
    }
    // In an outcome file both books share the levels, but each book has its
    // own queue: an order of one is never ahead of an order of the other.
    let books: Vec<(Option<bool>, String)> = match replay.outcome() {
        Some(outcome) => vec![
            (Some(false), format!(" on the {} book", outcome.sides[0])),
            (Some(true), format!(" on the {} book", outcome.sides[1])),
        ],
        None => vec![(None, String::new())],
    };
    for (side, best) in [(Side::Bid, book.best_bid()), (Side::Ask, book.best_ask())] {
        let Some(best) = best else { continue };
        for (second_book, which) in &books {
            let queue: Vec<&orderlog::RestingOrder> = match second_book {
                Some(second) => book.queue_of_book(side, best, *second).collect(),
                None => book.queue(side, best).collect(),
            };
            if queue.is_empty() {
                continue;
            }
            println!(
                "\nqueue at the best {side} {}{which}, front first:",
                format_fixed(best)
            );
            for (place, order) in queue.into_iter().take(args.queue).enumerate() {
                let address = replay
                    .address(order.user_id)
                    .map_or_else(|| "?".to_string(), |a| a.to_string());
                println!(
                    "  {:>3}. {:>14}  {address}  oid {}",
                    place + 1,
                    format_fixed(order.size),
                    order.oid
                );
            }
        }
    }
    if !replay.is_outcome() {
        // HIP-4 markets allow no priority fees, so their queues are exact.
        println!("\n(queue order is exact except among orders placed within 400 ms of each other)");
    }
    Ok(())
}

type Row = Option<(i64, i64, Option<usize>)>;

fn ladder(rows: impl Iterator<Item = (Row, Row)>) {
    let header = format!(
        "{:>7} {:>16} {:>14} | {:<14} {:<16} {:<7}",
        "orders", "bid size", "bid", "ask", "ask size", "orders"
    );
    println!("{}", header.trim_end());
    let count = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
    for (bid, ask) in rows {
        let left = match bid {
            Some((price, size, n)) => format!(
                "{:>7} {:>16} {:>14}",
                count(n),
                format_fixed(size),
                format_fixed(price)
            ),
            None => " ".repeat(39),
        };
        let right = match ask {
            Some((price, size, n)) => format!(
                "{:<14} {:<16} {:<7}",
                format_fixed(price),
                format_fixed(size),
                count(n)
            ),
            None => String::new(),
        };
        println!("{}", format!("{left} | {right}").trim_end());
    }
}

#[cfg(test)]
mod tests {
    use super::time_of_day;

    #[test]
    fn times_of_day() {
        assert_eq!(time_of_day("00:15"), Some(15 * 60 * 10_000_000));
        assert_eq!(time_of_day("12:00:30"), Some((12 * 3600 + 30) * 10_000_000));
        assert_eq!(
            time_of_day("12:00:30.5"),
            Some((12 * 3600 + 30) * 10_000_000 + 5_000_000)
        );
        assert_eq!(time_of_day("24:00"), None);
        assert_eq!(time_of_day("12"), None);
        assert_eq!(time_of_day("12:00.5"), None);
    }
}
