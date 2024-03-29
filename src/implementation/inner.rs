use event_listener::{Event, EventListener, IntoNotification};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[derive(Debug, Default)]
pub(crate) struct Inner {
    stop_event: Event,
    zero_event: Event,
    guard_count: AtomicUsize,
    stopped: AtomicBool,
}

impl Inner {
    pub(crate) fn stop(&self) {
        log::trace!("intending to stop");

        if self.stopped.swap(true, Ordering::SeqCst) {
            log::trace!("was already stopped");
        } else {
            log::trace!("stopped");
            self.stop_event.notify(usize::MAX.relaxed());
            if self.is_zero() {
                self.zero_event.notify(usize::MAX.relaxed());
            }
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub(crate) fn is_stopped_relaxed(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    pub(crate) fn is_zero(&self) -> bool {
        0 == self.guard_count.load(Ordering::SeqCst)
    }

    pub(crate) fn is_zero_relaxed(&self) -> bool {
        0 == self.guard_count_relaxed()
    }

    pub(crate) fn guard_count_relaxed(&self) -> usize {
        self.guard_count.load(Ordering::Relaxed)
    }

    pub(crate) fn listen_zero(&self) -> EventListener {
        self.zero_event.listen()
    }

    pub(crate) fn listen_stop(&self) -> EventListener {
        self.stop_event.listen()
    }

    pub(crate) fn decrement(&self) -> usize {
        let mut current = self.guard_count.load(Ordering::Relaxed);
        loop {
            let new = current.saturating_sub(1);
            log::trace!("intending to decrement from {current} to {new}");
            match self.guard_count.compare_exchange_weak(
                current,
                new,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if new == 0 && self.is_stopped() {
                        log::trace!("successfully decremented from {current} to {new}");
                        self.zero_event.notify(usize::MAX.relaxed());
                    }
                    log::trace!("successfully decremented from {current} to {new}");
                    break new;
                }

                Err(new_current) => {
                    log::trace!("failed to decrement from {current} to {new}, retrying");
                    current = new_current;
                }
            }
        }
    }

    pub(crate) fn increment(&self) -> usize {
        let mut current = self.guard_count.load(Ordering::Relaxed);
        loop {
            let new = current.checked_add(1).unwrap(); // panics on overflow
            log::trace!("intending to increment from {current} to {new}");
            match self.guard_count.compare_exchange_weak(
                current,
                new,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    log::trace!("successfully incremented from {current} to {new}");
                    break new;
                }

                Err(new_current) => {
                    log::trace!("failed to increment from {current} to {new}, retrying");
                    current = new_current;
                }
            }
        }
    }
}
