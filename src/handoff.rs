use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

/// Something a message carries that can be taken out only once, such as a
/// watcher or a cache, which cannot be cloned while a message must be.
///
/// Clones share it: whichever clone is taken from first gets it, and the
/// others find it gone. A clone made on the way never loses it.
pub struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Handoff<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    /// The value, if no clone has taken it yet.
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl<T> Clone for Handoff<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> fmt::Debug for Handoff<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handoff").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Handoff;

    #[test]
    fn a_clone_shares_the_value_and_it_is_taken_once() {
        let handoff = Handoff::new(String::from("watcher"));
        let clone = handoff.clone();
        assert_eq!(clone.take().as_deref(), Some("watcher"));
        assert_eq!(handoff.take(), None);
        assert_eq!(clone.take(), None);
    }
}
