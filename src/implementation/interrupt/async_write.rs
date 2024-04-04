use crate::Interrupt;
use futures_io::AsyncWrite;
use std::{
    io::{IoSlice, Result},
    pin::Pin,
    task::{Context, Poll},
};

impl<T: AsyncWrite> AsyncWrite for Interrupt<T> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        if self.is_stopped_relaxed() {
            Poll::Ready(Ok(0))
        } else {
            self.project().wrapped_type.poll_write(cx, buf)
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.project().wrapped_type.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.project().wrapped_type.poll_close(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize>> {
        if self.is_stopped_relaxed() {
            Poll::Ready(Ok(0))
        } else {
            self.project().wrapped_type.poll_write_vectored(cx, bufs)
        }
    }
}
