//! What a handle and its protocol worker share: cancellation, the certificate
//! question, and the state the host reads back synchronously.
//!
//! Both engines use this, so the rules that a cancel unblocks a read, that no
//! answer is a refusal, and that the published state and the event stream agree
//! are written once.
use crate::{Event, Failure, State};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Where an event goes. The crate's mailbox or handler is wrapped once here so a
/// session never has to know which one it is.
pub(crate) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// Shared between the handle and its worker.
pub(crate) struct Control {
    cancel: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
    state: AtomicU8,
    failure: Mutex<Failure>,
    certificate: Mutex<Question>,
    next_question: AtomicU32,
}

struct Question {
    id: u32,
    answer: Option<bool>,
}

impl Default for Control {
    fn default() -> Control {
        Control {
            cancel: AtomicBool::new(false),
            socket: Mutex::new(None),
            state: AtomicU8::new(0),
            failure: Mutex::new(Failure::None),
            certificate: Mutex::new(Question {
                id: 0,
                answer: None,
            }),
            next_question: AtomicU32::new(0),
        }
    }
}

impl Control {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Keeps a second handle to the socket so a cancel can unblock a read.
    pub(crate) fn attach(&self, socket: &TcpStream) {
        if let Ok(mut slot) = self.socket.lock() {
            if self.cancelled() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            } else {
                *slot = socket.try_clone().ok();
            }
        }
    }

    pub(crate) fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(slot) = self.socket.lock()
            && let Some(socket) = slot.as_ref()
        {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Asks the host whether to trust a certificate and waits for its answer. The
    /// wait is bounded: no answer is a refusal, never a silent acceptance.
    pub(crate) fn ask(
        &self,
        emit: &Emit,
        host: &str,
        subject: String,
        issuer: String,
        fingerprint: String,
    ) -> bool {
        let id = self.next_question.fetch_add(1, Ordering::SeqCst) + 1;
        match self.certificate.lock() {
            Ok(mut question) => *question = Question { id, answer: None },
            Err(_) => return false,
        }
        emit(Event::Certificate {
            request_id: id,
            host: host.to_owned(),
            subject,
            issuer,
            fingerprint,
            flags: 0,
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if self.cancelled() || Instant::now() > deadline {
                return false;
            }
            match self.certificate.lock() {
                Ok(question) if question.id == id => {
                    if let Some(answer) = question.answer {
                        return answer;
                    }
                }
                Ok(_) => return false,
                Err(_) => return false,
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn answer(&self, request_id: u32, accept: bool) -> bool {
        match self.certificate.lock() {
            Ok(mut question) if question.id == request_id && question.answer.is_none() => {
                question.answer = Some(accept);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn state(&self) -> State {
        let failure = self
            .failure
            .lock()
            .map(|failure| *failure)
            .unwrap_or(Failure::Internal);
        decode(self.state.load(Ordering::SeqCst), failure)
    }

    fn record(&self, state: &State, failure: Failure) {
        if let State::Failed(_) = state
            && let Ok(mut slot) = self.failure.lock()
        {
            *slot = failure;
        }
        self.state.store(encode(state), Ordering::SeqCst);
    }
}

fn encode(state: &State) -> u8 {
    match state {
        State::Idle => 0,
        State::Connecting => 1,
        State::Connected => 2,
        State::Closing => 3,
        State::Closed => 4,
        State::Failed(_) => 5,
    }
}

fn decode(code: u8, failure: Failure) -> State {
    match code {
        0 => State::Idle,
        1 => State::Connecting,
        2 => State::Connected,
        3 => State::Closing,
        4 => State::Closed,
        5 => State::Failed(failure),
        _ => State::Failed(Failure::Internal),
    }
}

/// Publishes one state change: the host's synchronous reader and the event stream
/// always see the same value.
pub(crate) fn publish(
    emit: &Emit,
    control: &Control,
    state: &State,
    failure: Failure,
    reason: &str,
) {
    control.record(state, failure);
    emit(Event::State {
        state: *state,
        failure,
        detail: 0,
        reason: reason.to_owned(),
    });
}
