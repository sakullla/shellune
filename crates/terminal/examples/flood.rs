//! Reproducible parser/scrollback benchmark; does not measure GPU rendering.
use shellune_terminal::TerminalCore;
use std::time::Instant;

fn main() {
    let mib: usize = std::env::args().nth(1).map_or(16, |n| n.parse().unwrap());
    let batch: usize = std::env::args().nth(2).map_or(8192, |n| n.parse().unwrap());
    let data = b"2026-11-16 INFO request completed status=200 elapsed=12ms payload=sample\r\n"
        .repeat(2048);
    for integrated in [false, true] {
        let mut terminal = TerminalCore::new(120, 40, 10_000);
        if integrated {
            terminal.feed(b"\x1b]133;A\x07\x1b]133;B\x07cat log\r\n\x1b]133;C\x07");
        }
        let started = Instant::now();
        let mut bytes = 0;
        let mut worst = std::time::Duration::ZERO;
        while bytes < mib * 1024 * 1024 {
            let n = batch.min(data.len()).min(mib * 1024 * 1024 - bytes);
            let tick = Instant::now();
            terminal.feed(&data[..n]);
            worst = worst.max(tick.elapsed());
            bytes += n;
        }
        println!(
            "integrated={integrated} MiB={mib} batch={batch} seconds={:.3} MiB/s={:.2} worst_feed_ms={:.2} history={} retained_block_bytes={}",
            started.elapsed().as_secs_f64(),
            mib as f64 / started.elapsed().as_secs_f64(),
            worst.as_secs_f64() * 1000.0,
            terminal.history(),
            terminal
                .command_blocks()
                .iter()
                .map(|b| b.output.len())
                .sum::<usize>()
        );
    }
}
