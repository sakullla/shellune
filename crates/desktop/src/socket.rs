//! Wake a single connection worker for socket traffic, queued input or a timer.
use polling::{Event, Events, Poller};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

struct Readiness {
    poller: Arc<Poller>,
    events: Events,
    rearm: bool,
    wait: Duration,
}

pub(crate) struct PollingSocket {
    socket: TcpStream,
    readiness: Option<Readiness>,
}

impl PollingSocket {
    pub(crate) fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            readiness: None,
        }
    }

    pub(crate) fn poll_reads_every(&mut self, wait: Duration) -> io::Result<&Arc<Poller>> {
        if self.readiness.is_none() {
            let poller = Arc::new(Poller::new()?);
            self.socket.set_read_timeout(None)?;
            // SAFETY: Drop removes this registration before the socket closes.
            unsafe { poller.add(&self.socket, Event::readable(0))? };
            self.readiness = Some(Readiness {
                poller,
                events: Events::with_capacity(NonZeroUsize::new(2).unwrap()),
                rearm: false,
                wait,
            });
        }
        let ready = self.readiness.as_mut().unwrap();
        ready.wait = wait;
        Ok(&ready.poller)
    }
}

impl Drop for PollingSocket {
    fn drop(&mut self) {
        if let Some(ready) = &self.readiness {
            let _ = ready.poller.delete(&self.socket);
        }
    }
}

impl Read for PollingSocket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Some(ready) = &mut self.readiness {
            // Unix can probe for bytes without changing the blocking mode of
            // writes. Skip the kernel event wait on a continuously busy link.
            #[cfg(unix)]
            let mut try_recv = || {
                rustix::net::recv(&self.socket, &mut *buffer, rustix::net::RecvFlags::DONTWAIT)
                    .map(|(_, n)| n)
            };
            #[cfg(unix)]
            match try_recv() {
                Err(rustix::io::Errno::AGAIN) => {}
                result => return result.map_err(Into::into),
            }
            if ready.rearm {
                ready.poller.modify(&self.socket, Event::readable(0))?;
                ready.rearm = false;
            }
            ready.events.clear();
            ready.poller.wait(&mut ready.events, Some(ready.wait))?;
            if ready.events.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            // Rearm after the single reader consumes this readiness event.
            ready.rearm = true;
            #[cfg(unix)]
            return try_recv().map_err(Into::into);
        }
        self.socket.read(buffer)
    }
}

impl Write for PollingSocket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.socket.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn input_wakes_a_silent_connection_and_keeps_the_next_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut socket = PollingSocket::new(listener.accept().unwrap().0);
        let (tx, rx) = crate::input::channel(None);
        rx.wake_on_input(socket.poll_reads_every(Duration::from_secs(10)).unwrap());
        let (woke, result) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut byte = [0];
            let err = socket.read(&mut byte).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(rx.pop(), Some(42));
            woke.send(()).unwrap();
            socket.read_exact(&mut byte).unwrap();
            byte
        });
        tx.send(42, None).unwrap();
        result.recv_timeout(Duration::from_secs(2)).unwrap();
        peer.write_all(&[7]).unwrap();
        assert_eq!(thread.join().unwrap(), [7]);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn cancellation_wakes_an_idle_poll_even_when_registration_races() {
        for already_cancelled in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let mut socket = PollingSocket::new(listener.accept().unwrap().0);
            let control = crate::control::Control::default();
            if already_cancelled {
                control.stop();
            }
            control.wake_on_cancel(socket.poll_reads_every(Duration::from_secs(10)).unwrap());
            let (woke, result) = mpsc::channel();
            let thread = std::thread::spawn(move || {
                woke.send(socket.read(&mut [0]).unwrap_err().kind())
                    .unwrap();
            });
            if !already_cancelled {
                control.stop();
            }
            assert_eq!(
                result.recv_timeout(Duration::from_secs(2)).unwrap(),
                io::ErrorKind::WouldBlock
            );
            thread.join().unwrap();
        }
    }
}
