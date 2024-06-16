use core::{
    cell::RefCell,
    future::poll_fn,
    task::{Context, Poll},
};

use smoltcp::{
    iface::{Interface, SocketHandle},
    socket::tcp,
};

use crate::stack::SocketStack;

/// Error returned by TcpSocket read/write functions.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// The connection was reset.
    ///
    /// This can happen on receiving a RST packet, or on timeout.
    ConnectionReset,
}

/// Error returned by [`TcpSocket::connect`].
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
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
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AcceptError {
    /// The socket is already connected or listening.
    InvalidState,
    /// Invalid listen port
    InvalidPort,
    /// The remote host rejected the connection with a RST packet.
    ConnectionReset,
}

#[derive(Copy, Clone)]
struct TcpIO<'a> {
    stack: &'a RefCell<SocketStack>,
    handle: SocketHandle,
}

impl<'a> TcpIO<'a> {
    fn with<R>(&self, f: impl FnOnce(&tcp::Socket, &Interface) -> R) -> R {
        let stack = &*self.stack.borrow();
        let socket = stack.sockets.get::<tcp::Socket>(self.handle);
        f(socket, &stack.iface)
    }

    fn with_mut<R>(&mut self, f: impl FnOnce(&mut tcp::Socket, &mut Interface) -> R) -> R {
        let stack = &mut *self.stack.borrow_mut();
        let socket = stack.sockets.get_mut::<tcp::Socket>(self.handle);
        let ret = f(socket, &mut stack.iface);
        stack.waker.wake();
        ret
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        poll_fn(|cx| self.poll_read(cx, buf)).await
    }

    async fn read_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        let mut f = Some(f);
        poll_fn(move |cx| self.poll_read_with(cx, f.take().unwrap())).await
    }

    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize, Error>> {
        // CAUTION: smoltcp semantics around EOF are different to what you'd expect
        // from posix-like IO, so we have to tweak things here.
        self.with_mut(|s, _| match s.recv_slice(buf) {
            // No data ready
            Ok(0) => {
                s.register_recv_waker(cx.waker());
                Poll::Pending
            }
            // Data ready!
            Ok(n) => Poll::Ready(Ok(n)),
            // EOF
            Err(tcp::RecvError::Finished) => Poll::Ready(Ok(0)),
            // Connection reset. TODO: this can also be timeouts etc, investigate.
            Err(tcp::RecvError::InvalidState) => Poll::Ready(Err(Error::ConnectionReset)),
        })
    }

    fn poll_read_with<F, R>(&mut self, cx: &mut Context<'_>, f: F) -> Poll<Result<R, Error>>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.with_mut(|s, _| {
            if !s.can_recv() {
                if s.may_recv() {
                    // socket buffer is empty wait until it has atleast one byte has arrived
                    s.register_recv_waker(cx.waker());
                    Poll::Pending
                } else {
                    // if we can't receive because the receive half of the duplex connection is closed then return an error
                    Poll::Ready(Err(Error::ConnectionReset))
                }
            } else {
                Poll::Ready(match s.recv(f) {
                    // Connection reset. TODO: this can also be timeouts etc, investigate.
                    Err(tcp::RecvError::Finished) | Err(tcp::RecvError::InvalidState) => {
                        Err(Error::ConnectionReset)
                    }
                    Ok(r) => Ok(r),
                })
            }
        })
    }

    async fn write(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        poll_fn(|cx| self.poll_write(cx, buf)).await
    }

    async fn write_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        let mut f = Some(f);
        poll_fn(move |cx| self.poll_write_with(cx, f.take().unwrap())).await
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize, Error>> {
        self.with_mut(|s, _| match s.send_slice(buf) {
            // Not ready to send (no space in the tx buffer)
            Ok(0) => {
                s.register_send_waker(cx.waker());
                Poll::Pending
            }
            // Some data sent
            Ok(n) => Poll::Ready(Ok(n)),
            // Connection reset. TODO: this can also be timeouts etc, investigate.
            Err(tcp::SendError::InvalidState) => Poll::Ready(Err(Error::ConnectionReset)),
        })
    }

    fn poll_write_with<F, R>(&mut self, cx: &mut Context<'_>, f: F) -> Poll<Result<R, Error>>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.with_mut(|s, _| {
            if !s.can_send() {
                if s.may_send() {
                    // socket buffer is full wait until it has atleast one byte free
                    s.register_send_waker(cx.waker());
                    Poll::Pending
                } else {
                    // if we can't transmit because the transmit half of the duplex connection is closed then return an error
                    Poll::Ready(Err(Error::ConnectionReset))
                }
            } else {
                Poll::Ready(match s.send(f) {
                    // Connection reset. TODO: this can also be timeouts etc, investigate.
                    Err(tcp::SendError::InvalidState) => Err(Error::ConnectionReset),
                    Ok(r) => Ok(r),
                })
            }
        })
    }

    async fn flush(&mut self) -> Result<(), Error> {
        poll_fn(|cx| self.poll_flush(cx)).await
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.with_mut(|s, _| {
            let data_pending = s.send_queue() > 0;
            let fin_pending = matches!(
                s.state(),
                tcp::State::FinWait1 | tcp::State::Closing | tcp::State::LastAck
            );
            let rst_pending = s.state() == tcp::State::Closed && s.remote_endpoint().is_some();

            // If there are outstanding send operations, register for wake up and wait
            // smoltcp issues wake-ups when octets are dequeued from the send buffer
            if data_pending || fin_pending || rst_pending {
                s.register_send_waker(cx.waker());
                Poll::Pending
            // No outstanding sends, socket is flushed
            } else {
                Poll::Ready(Ok(()))
            }
        })
    }

    fn recv_capacity(&self) -> usize {
        self.with(|s, _| s.recv_capacity())
    }

    fn send_capacity(&self) -> usize {
        self.with(|s, _| s.send_capacity())
    }

    fn send_queue(&self) -> usize {
        self.with(|s, _| s.send_queue())
    }

    fn recv_queue(&self) -> usize {
        self.with(|s, _| s.recv_queue())
    }
}
