//! What a file holds: counts, time range, the book at the end.
//!
//! ```text
//! cargo run --release --example info -- FILE.bin.lz4
//! ```

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::ExitCode;

use alphaquick::l2::{self, Event};
use alphaquick::orderlog::{Record, Replay, Status};
use alphaquick::outcome::TradeKind;
use alphaquick::time::{format_fixed, UtcDateTime};
use alphaquick::{continuity, names, FileKind, L2Book, Side};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: info FILE.bin.lz4");
        return ExitCode::from(2);
    };
    match info(Path::new(&path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("info: {error}");
            ExitCode::FAILURE
        }
    }
}

fn price(value: Option<i64>) -> String {
    value.map_or_else(|| "-".to_string(), format_fixed)
}

fn info(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let size = std::fs::metadata(path)?.len();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // MB of 1024 x 1024 bytes, as alphaquick.io gives file sizes.
    println!("{name}  ({:.1} MB)", size as f64 / 1_048_576.0);
    let kind = match names::file_kind(path) {
        Some(kind) => kind,
        None => alphaquick::detect(File::open(path)?)?.ok_or("the file holds no records")?,
    };
    match kind {
        FileKind::L2 => l2_info(path),
        FileKind::OrderLog => orderlog_info(path),
        _ => Err("a kind of file this example does not know".into()),
    }
}

fn l2_info(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(name) = names::parse_l2_name(path) {
        println!(
            "L2 depth & trades: {} on {}, {}",
            name.symbol, name.venue, name.day
        );
    }
    let sidecar = continuity::path_of(path);
    let lines = if sidecar.is_file() {
        continuity::read(&sidecar)?
    } else {
        println!("no continuity file next to it: gaps, if any, are not known");
        Vec::new()
    };
    let mut book = L2Book::new();
    let (mut depth, mut trades, mut snapshots, mut gaps) = (0u64, 0u64, 0u64, 0u64);
    let mut range: Option<(i64, i64)> = None;
    for event in l2::Reader::open(path)?.with_continuity(&lines) {
        let event = event?;
        book.apply_event(&event);
        match event {
            Event::Trade(_) => trades += 1,
            Event::Depth(update) => {
                depth += 1;
                snapshots += u64::from(update.clears_book());
            }
            Event::Gap(_) => {
                gaps += 1;
                continue; // a gap's time is the recorder's, not a record's
            }
            _ => {}
        }
        let t = event.time();
        range = Some(range.map_or((t, t), |(first, _)| (first, t)));
    }
    println!("{depth} depth updates ({snapshots} snapshots start), {trades} trades, {gaps} gaps");
    if !lines.is_empty() {
        let seeds = lines.iter().filter(|l| l.is_seed()).count();
        println!(
            "continuity file: {} lines, {seeds} seeds (the data file has {snapshots})",
            lines.len()
        );
    }
    if let Some((first, last)) = range {
        println!(
            "from {} to {} UTC",
            UtcDateTime::from_ticks(first),
            UtcDateTime::from_ticks(last)
        );
    }
    println!(
        "book at the end: best bid {}, best ask {}, {} bid / {} ask levels{}",
        price(book.best_bid().map(|(p, _)| p)),
        price(book.best_ask().map(|(p, _)| p)),
        book.bids().len(),
        book.asks().len(),
        if book.is_known() {
            ""
        } else {
            " (in a gap: out of date)"
        }
    );
    Ok(())
}

fn orderlog_info(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut replay = Replay::open(path)?;
    let (mut snaps, mut blocks, mut orders, mut trades) = (0u64, 0u64, 0u64, 0u64);
    let mut statuses: BTreeMap<Status, u64> = BTreeMap::new();
    let mut kinds: BTreeMap<TradeKind, u64> = BTreeMap::new();
    let mut first_block = None;
    let mut system_prints = 0u64;
    while let Some(record) = replay.next_record()? {
        match record {
            Record::SnapOrder(_) => snaps += 1,
            Record::Block(block) => {
                blocks += 1;
                first_block.get_or_insert(block);
            }
            Record::Order(order) => {
                orders += 1;
                *statuses.entry(order.status).or_default() += 1;
            }
            Record::Trade(trade) => {
                trades += 1;
                system_prints += u64::from(trade.is_system_print());
                if let Some(kind) = replay.last_trade_kind() {
                    *kinds.entry(kind).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    if let Some(meta) = replay.meta() {
        let market = names::parse_orderlog_name(path)
            .map(|n| format!(" ({})", n.market))
            .unwrap_or_default();
        // The OUTCOME record has an outcome market's title (META has `HIP4:<id>`).
        let coin = replay
            .outcome()
            .map_or(meta.coin.as_str(), |o| o.title.as_str());
        println!(
            "Order log: {coin}{market}, day {}, starts from block {}",
            meta.day, meta.start_block
        );
    }
    if let Some(outcome) = replay.outcome() {
        let question = if outcome.question.is_empty() {
            String::new()
        } else {
            format!(", question: {}", outcome.question)
        };
        println!(
            "HIP-4 outcome market {}{question}",
            outcome
                .outcome
                .map_or_else(|| "?".to_string(), |id| id.to_string()),
        );
        println!(
            "  sides: {} / {}; prices are {}'s, as shares of 1",
            outcome.sides[0], outcome.sides[1], outcome.sides[0]
        );
    }
    println!(
        "{snaps} orders resting at 00:00 UTC, {blocks} blocks, {orders} order events, {trades} trades \
         ({system_prints} system prints), {} accounts",
        replay.accounts().len()
    );
    if let (Some(first), Some(last)) = (first_block, replay.block()) {
        println!(
            "blocks from {} to {} UTC",
            UtcDateTime::from_unix_ns(first.time_ns as i64),
            UtcDateTime::from_unix_ns(last.time_ns as i64)
        );
    }
    if !statuses.is_empty() {
        let mut common: Vec<(Status, u64)> = statuses.into_iter().collect();
        common.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let common: Vec<String> = common
            .iter()
            .take(6)
            .map(|(s, n)| format!("{s} {n}"))
            .collect();
        println!("order events: {}", common.join(", "));
    }
    if replay.is_outcome() {
        let kinds: Vec<String> = TradeKind::ALL
            .iter()
            .map(|k| format!("{k} {}", kinds.get(k).copied().unwrap_or(0)))
            .collect();
        println!("trades: {}", kinds.join(", "));
    }
    let book = replay.book();
    println!(
        "book at the end: best bid {}, best ask {}, {} bid / {} ask orders",
        price(book.best_bid()),
        price(book.best_ask()),
        book.order_count(Side::Bid),
        book.order_count(Side::Ask)
    );
    Ok(())
}
