//! Byte accounting for input waiting on a slow terminal or network.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Maximum queued input per terminal, including the write currently in progress.
pub const INPUT_LIMIT: usize = 1024 * 1024;

/// A shared reservation counter. Small messages are charged for queue overhead too.
#[derive(Clone, Debug, Default)]
pub struct InputBudget(Arc<AtomicUsize>);

/// Input whose reservation lasts until delivery completes or is cancelled.
pub struct InputChunk {
    /// Owned bytes, allocated only after the reservation succeeds.
    pub bytes: Vec<u8>,
    budget: InputBudget,
    charge: usize,
}

impl std::fmt::Debug for InputChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputChunk")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl InputBudget {
    /// Reserves an entire write atomically; `None` means nothing was accepted.
    pub fn reserve(&self, bytes: &[u8]) -> Option<InputChunk> {
        let charge = bytes.len().max(256);
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(charge).filter(|next| *next <= INPUT_LIMIT)
            })
            .ok()?;
        Some(InputChunk {
            bytes: bytes.to_vec(),
            budget: self.clone(),
            charge,
        })
    }
}

impl Drop for InputChunk {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_include_inflight_writes_and_release_on_drop() {
        let budget = InputBudget::default();
        let held = budget.reserve(&vec![0; INPUT_LIMIT]).unwrap();
        assert!(budget.reserve(b"x").is_none());
        drop(held);
        assert!(budget.reserve(b"x").is_some());
        assert!(budget.reserve(&vec![0; INPUT_LIMIT + 1]).is_none());
    }
}
