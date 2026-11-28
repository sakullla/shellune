//! CPU-only viewport and scrollback workloads for comparing terminal changes.
//! Run with `cargo run --release -p shellune-terminal --example viewport_search`.
use shellune_terminal::TerminalCore;
use shellune_terminal::core::{HistoryPosition, SearchOptions};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let mut terminal = TerminalCore::new(120, 40, 10_000);
    let line = "entry ".repeat(18) + "\r\n";
    for _ in 0..10_040 {
        terminal.feed(line.as_bytes());
    }

    // Warm up highlighting and other lazily initialized state.
    black_box(terminal.snapshot());
    let started = Instant::now();
    for _ in 0..1_000 {
        black_box(terminal.snapshot());
    }
    println!("1000 viewport snapshots: {:?}", started.elapsed());

    let started = Instant::now();
    let hits = terminal
        .search_history_positions("entry", SearchOptions::literal())
        .unwrap();
    let elapsed = started.elapsed();
    let retained = hits.capacity() * size_of::<HistoryPosition>();
    println!(
        "{} hits: {elapsed:?}, {retained} bytes of result storage",
        hits.len()
    );
}
