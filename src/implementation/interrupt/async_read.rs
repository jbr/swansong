use crate::Interrupt;
use futures_io::AsyncRead;
use std::{
    io::{IoSliceMut, Result},
    pin::Pin,
    task::{Context, Poll},
};

impl<T: AsyncRead> AsyncRead for Interrupt<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize>> {
        if self.is_stopped() {
            self.project().wrapped_type.poll_read(cx, buf)
        } else {
            Poll::Ready(Ok(0))
        }
    }

    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<Result<usize>> {
        if self.is_stopped() {
            self.project().wrapped_type.poll_read_vectored(cx, bufs)
        } else {
            Poll::Ready(Ok(0))
        }
    }
}
