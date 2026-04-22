use super::Inner;
use event_listener::{EventListener, Listener};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{ready, Context, Poll},
};

/// A [`Future`] that resolves when shutdown has been initiated on the
/// associated [`Swansong`][crate::Swansong].
///
/// Unlike [`ShutdownCompletion`][crate::ShutdownCompletion], this future
/// resolves as soon as shutdown has been signaled — either by
/// [`Swansong::shut_down`][crate::Swansong::shut_down], by propagation from a
/// parent, or by the last root [`Swansong`][crate::Swansong] handle being
/// dropped — without waiting for outstanding [`Guard`][crate::Guard]s to drop.
///
/// This type can also be used in blocking contexts with [`ShuttingDown::block`].
#[derive(Debug)]
pub struct ShuttingDown(Arc<Inner>, Option<EventListener>);

impl ShuttingDown {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self(Arc::clone(inner), None)
    }

    /// Blocks the current thread until shutdown has been initiated.
    ///
    /// Do not use this in async contexts. Instead, await this [`ShuttingDown`].
    pub fn block(self) {
        let Self(inner, mut stop_listener) = self;
        loop {
            if inner.is_stopped_relaxed() {
                return;
            }

            let listener = if let Some(listener) = stop_listener.take() {
                listener
            } else {
                let listener = inner.listen_stop();
                if inner.is_stopped() {
                    return;
                }
                listener
            };

            listener.wait();
        }
    }
}

impl Future for ShuttingDown {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self(inner, stop_listener) = &mut *self;

        loop {
            if inner.is_stopped_relaxed() {
                log::trace!("stopped, shutting_down ready");
                return Poll::Ready(());
            }
            let listener = if let Some(listener) = stop_listener {
                listener
            } else {
                log::trace!("registering new stop listener");
                let listener = stop_listener.insert(inner.listen_stop());

                if inner.is_stopped() {
                    log::trace!("stopped after registering listener");
                    return Poll::Ready(());
                }
                listener
            };
            ready!(Pin::new(listener).poll(cx));
            log::trace!("stop event notified!");
            *stop_listener = None;
        }
    }
}
