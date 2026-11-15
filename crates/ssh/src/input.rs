//! Keep a stalled SSH send window out of the shell's receive/control loop.
use shellune_core::io_budget::InputChunk;
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};

pub(crate) fn writer(
    channel: Arc<russh::ChannelWriteHalf<russh::client::Msg>>,
) -> (
    mpsc::UnboundedSender<InputChunk>,
    JoinHandle<Result<(), String>>,
) {
    // Every chunk already holds a per-session byte reservation, including the
    // in-flight write. Moving it here cannot grow that budget or copy its data.
    let (tx, mut rx) = mpsc::unbounded_channel::<InputChunk>();
    let task = tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            channel
                .data(&chunk.bytes[..])
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    (tx, task)
}
