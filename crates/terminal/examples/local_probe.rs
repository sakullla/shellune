//! Real local PTY + parser flood probe, including a deliberately slow consumer.
use portable_pty::CommandBuilder;
use shellune_terminal::{GridSize, LocalSession, PtyEvent, TerminalCore};
use std::time::{Duration, Instant};

fn main() {
    let lines: usize = std::env::args()
        .nth(1)
        .map_or(100_000, |n| n.parse().unwrap());
    let mut command = CommandBuilder::new(if cfg!(windows) { "python" } else { "python3" });
    command.arg("-u");
    command.arg("-c");
    command.arg(format!("import os,time; os.write(1,b'LOCAL-BEGIN\\n'); time.sleep(0.1); block=b'log-line-0123456789abcdef\\n'; [os.write(1,block*min(4096,{lines}-i)) for i in range(0,{lines},4096)]; os.write(1,b'LOCAL-END\\n')"));
    let mut session =
        LocalSession::spawn_command(command, GridSize::new(120, 40), "python").unwrap();
    let mut terminal = TerminalCore::new(120, 40, 10_000);
    let start = Instant::now();
    let mut paused = false;
    let mut exited = false;
    let mut bytes = 0usize;
    while start.elapsed() < Duration::from_secs(180) && !exited {
        for event in session.drain() {
            match event {
                PtyEvent::Data(data) => {
                    bytes += data.len();
                    let effects = terminal.feed(&data);
                    if !effects.writes.is_empty() {
                        session.write(&effects.writes).unwrap();
                    }
                    if !paused
                        && terminal
                            .snapshot()
                            .text()
                            .join("\n")
                            .contains("LOCAL-BEGIN")
                    {
                        paused = true;
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                PtyEvent::Exited(report) => {
                    assert!(!report.is_abnormal());
                    exited = true;
                }
                PtyEvent::ReadFailed(reason) => panic!("PTY failed: {reason}"),
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(exited, "process did not finish");
    assert!(
        terminal.snapshot().text().join("\n").contains("LOCAL-END"),
        "tail output lost before exit"
    );
    println!(
        "PASS lines_generated={lines} raw_bytes={bytes} seconds={:.3} history={} slow_consumer={paused}",
        start.elapsed().as_secs_f64(),
        terminal.history()
    );
    session.close();
}
