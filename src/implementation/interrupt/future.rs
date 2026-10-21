use super::Interrupt;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

impl<T: Future> Future for Interrupt<T> {
    type Output = Option<T::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if this.stop_listener.is_stopped() {
            log::trace!("stopped, cancelling");
            return Poll::Ready(None);
        }

        match this.wrapped_type.poll(cx) {
            Poll::Ready(output) => Poll::Ready(Some(output)),
            Poll::Pending => this.stop_listener.poll_stopped(cx).map(|()| None),
        }
    }
}
