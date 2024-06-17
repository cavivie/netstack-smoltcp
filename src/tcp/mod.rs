mod smoltcp;

use smoltcp::TcpSocket;

use core::{
    net::SocketAddr,
    task::{Context, Poll},
};

/// Possible values which can be passed to the [`TcpStream::shutdown`] method.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Shutdown {
    /// The reading portion of the [`TcpStream`] should be shut down.
    ///
    /// All currently blocked and future [reads] will return <code>[Ok]\(0)</code>.
    ///
    /// [reads]: crate::io::Read "io::Read"
    Read,
    /// The writing portion of the [`TcpStream`] should be shut down.
    ///
    /// All currently blocked and future [writes] will return an error.
    ///
    /// [writes]: crate::io::Write "io::Write"
    Write,
    /// Both the reading and the writing portions of the [`TcpStream`] should be shut down.
    ///
    /// See [`Shutdown::Read`] and [`Shutdown::Write`] for more information.
    Both,
}

/// Error returned by TcpSocket read/write functions.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// The connection was reset.
    ///
    /// This can happen on receiving a RST packet, or on timeout.
    ConnectionReset,
    /// A parameter was incorrect.
    InvalidInput,
}

/// Error returned by [`TcpSocket::connect`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ConnectError {
    /// The socket is already connected or listening.
    InvalidState,
    /// The remote host rejected the connection with a RST packet.
    ConnectionReset,
    /// Connect timed out.
    TimedOut,
    /// No route to host.
    NoRoute,
}

/// Error returned by [`TcpSocket::accept`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AcceptError {
    /// The socket is already connected or listening.
    InvalidState,
    /// Invalid listen port
    InvalidPort,
    /// The remote host rejected the connection with a RST packet.
    ConnectionReset,
}

pub struct TcpStream<'a> {
    io: TcpSocket<'a>,
}

impl<'a> TcpStream<'a> {
    /// Opens a TCP connection to a remote host.
    pub async fn connect(addr: SocketAddr) -> Result<Self, ConnectError> {
        let socket: TcpSocket = todo!();
        socket.connect(addr).await?;
        Ok(Self { io: socket })
    }

    /// Returns the local endpoint of the socket.
    ///
    /// Returns `None` if the socket is not bound (listening) or not connected.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.io.local_addr()
    }

    /// Returns the remote endpoint of the socket.
    ///
    /// Returns `None` if the socket is not connected.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.io.peer_addr()
    }

    pub async fn peek(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.io.peek(buf).await
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O on the specified
    /// portions to return immediately with an appropriate value (see the
    /// documentation of `Shutdown`).
    pub fn shutdown<H: Into<Shutdown>>(&mut self, how: H) -> Result<(), Error> {
        self.io.shutdown(how.into())
    }

    pub(crate) fn poll_read_priv(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        self.io.poll_read(cx, buf)
    }

    pub(crate) fn poll_write_priv(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        self.io.poll_write(cx, buf)
    }

    pub(crate) fn poll_flush_priv(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.io.poll_flush(cx)
    }
}

#[cfg(feature = "std")]
mod _std {
    use std::net::Shutdown;

    use super::{Error, Shutdown as TcpShutdown};

    impl From<Shutdown> for TcpShutdown {
        fn from(value: Shutdown) -> Self {
            match value {
                Shutdown::Read => TcpShutdown::Read,
                Shutdown::Write => TcpShutdown::Write,
                Shutdown::Both => TcpShutdown::Both,
            }
        }
    }

    pub(crate) fn io_error_from(err: Error) -> std::io::Error {
        match err {
            Error::ConnectionReset => std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "the connection was reset",
            ),
            Error::InvalidInput => std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "A parameter was incorrect",
            ),
        }
    }
}

#[cfg(all(feature = "std", feature = "tokio-io"))]
mod _tokio_io {
    use std::{
        io::Result,
        net::Shutdown,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    use super::{TcpStream, _std::io_error_from};

    impl<'a> AsyncRead for TcpStream<'a> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<Result<()>> {
            self.poll_read_priv(cx, buf.initialize_unfilled())
                .map(|res| match res {
                    Ok(n) => {
                        buf.advance(n);
                        Ok(())
                    }
                    Err(err) => Err(io_error_from(err)),
                })
        }
    }

    impl<'a> AsyncWrite for TcpStream<'a> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize>> {
            self.poll_write_priv(cx, buf).map(|res| match res {
                Ok(n) => Ok(n),
                Err(err) => Err(io_error_from(err)),
            })
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.poll_flush_priv(cx).map_err(io_error_from)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.shutdown(Shutdown::Write).map_err(io_error_from)?;
            Poll::Ready(Ok(()))
        }
    }
}

#[cfg(all(feature = "std", feature = "futures-io"))]
mod _futures_io {
    use futures::{AsyncRead, AsyncWrite};
    use std::{
        io::Result,
        net::Shutdown,
        pin::Pin,
        task::{Context, Poll},
    };

    use super::{TcpStream, _std::io_error_from};

    impl<'a> AsyncRead for TcpStream<'a> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<Result<usize>> {
            self.poll_read_priv(cx, buf).map_err(io_error_from)
        }
    }

    impl<'a> AsyncWrite for TcpStream<'a> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize>> {
            self.poll_write_priv(cx, buf).map(|res| match res {
                Ok(n) => Ok(n),
                Err(err) => Err(io_error_from(err)),
            })
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.poll_flush_priv(cx).map_err(io_error_from)
        }

        fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.shutdown(Shutdown::Write).map_err(io_error_from)?;
            Poll::Ready(Ok(()))
        }
    }
}
