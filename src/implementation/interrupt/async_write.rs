use crate::Interrupt;
use std::{
    io::{IoSlice, Result},
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(feature = "futures-io")]
impl<T: futures_io::AsyncWrite> futures_io::AsyncWrite for Interrupt<T> {
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

#[cfg(feature = "tokio")]
impl<T: tokio::io::AsyncWrite> tokio::io::AsyncWrite for Interrupt<T> {
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

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.project().wrapped_type.poll_shutdown(cx)
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

    fn is_write_vectored(&self) -> bool {
        self.wrapped_type.is_write_vectored()
    }
}
