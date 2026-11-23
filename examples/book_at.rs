//! The book at a moment of the file's day (UTC), or at the end of the file.
//!
//! ```text
//! cargo run --release --example book_at -- FILE.bin.lz4 [HH:MM[:SS]] [--levels N] [--queue N]
//! ```
//!
//! For an order-log file it also lists the first orders in the queue at the
//! best bid and the best ask, with their accounts.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::ExitCode;

use alphaquick::l2::{self, Event};
use alphaquick::orderlog::{self, Record, Replay};
use alphaquick::time::{format_fixed, UtcDateTime, TICKS_PER_SECOND};
use alphaquick::{names, FileKind, L2Book, Side};

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
    let book = match until {
        Some(until) => l2::book_at(BufReader::new(File::open(path)?), until, false)?,
        None => {
            let mut book = L2Book::new();
            for event in l2::Reader::open(path)? {
                if let Event::Depth(update) = event? {
                    book.apply(&update);
                }
            }
            book
        }
    };
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
    for (side, best) in [(Side::Bid, book.best_bid()), (Side::Ask, book.best_ask())] {
        let Some(best) = best else { continue };
        println!(
            "\nqueue at the best {side} {}, front first:",
            format_fixed(best)
        );
        for (place, order) in book.queue(side, best).take(args.queue).enumerate() {
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
