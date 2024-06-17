use core::{
    cell::RefCell,
    future::poll_fn,
    mem,
    net::SocketAddr,
    task::{Context, Poll},
};

use smoltcp::{
    iface::{Interface, SocketHandle},
    phy::Device,
    socket::tcp,
    time::Duration,
    wire::{IpEndpoint, IpListenEndpoint},
};

use super::{AcceptError, ConnectError, Error, Shutdown};
use crate::{stack::SocketStack, Stack};

/// A TCP socket to wrap TcpIO.
pub struct TcpSocket<'a> {
    io: TcpIO<'a>,
}

/// The reader half of a TCP socket.
pub struct TcpReader<'a> {
    io: TcpIO<'a>,
}

/// The writer half of a TCP socket.
pub struct TcpWriter<'a> {
    io: TcpIO<'a>,
}

impl<'a> TcpReader<'a> {
    /// Reads data from the socket.
    ///
    /// Returns how many bytes were read, or an error. If no data is available, it waits
    /// until there is at least one byte available.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.io.read(buf).await
    }

    /// Calls `f` with the largest contiguous slice of octets in the receive buffer,
    /// and dequeue the amount of elements returned by `f`.
    ///
    /// If no data is available, it waits until there is at least one byte available.
    pub async fn read_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.io.read_with(f).await
    }

    /// Returns the maximum number of bytes inside the transmit buffer.
    pub fn recv_capacity(&self) -> usize {
        self.io.recv_capacity()
    }

    /// Returns the amount of octets queued in the receive buffer. This value can be larger than
    /// the slice read by the next `recv` or `peek` call because it includes all queued octets,
    /// and not only the octets that may be returned as a contiguous slice.
    pub fn recv_queue(&self) -> usize {
        self.io.recv_queue()
    }
}

impl<'a> TcpWriter<'a> {
    /// Writes data to the socket.
    ///
    /// Returns how many bytes were written, or an error. If the socket is not ready to
    /// accept data, it waits until it is.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.io.write(buf).await
    }

    /// Flushes the written data to the socket.
    ///
    /// This waits until all data has been sent, and ACKed by the remote host. For a connection
    /// closed with [`abort()`](TcpSocket::abort) it will wait for the TCP RST packet to be sent.
    pub async fn flush(&mut self) -> Result<(), Error> {
        self.io.flush().await
    }

    /// Calls `f` with the largest contiguous slice of octets in the transmit buffer,
    /// and enqueue the amount of elements returned by `f`.
    ///
    /// If the socket is not ready to accept data, it waits until it is.
    pub async fn write_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.io.write_with(f).await
    }

    /// Returns the maximum number of bytes inside the transmit buffer.
    pub fn send_capacity(&self) -> usize {
        self.io.send_capacity()
    }

    /// Returns the amount of octets queued in the transmit buffer.
    pub fn send_queue(&self) -> usize {
        self.io.send_queue()
    }
}

impl<'a> TcpSocket<'a> {
    /// Creates a new TCP socket on the given stack, with the given buffers.
    pub fn new<D: Device>(
        stack: &'a Stack<D>,
        rx_buffer: &'a mut [u8],
        tx_buffer: &'a mut [u8],
    ) -> Self {
        let s = &mut *stack.socket_stack.borrow_mut();
        let rx_buffer: &'static mut [u8] = unsafe { mem::transmute(rx_buffer) };
        let tx_buffer: &'static mut [u8] = unsafe { mem::transmute(tx_buffer) };
        let handle = s.sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(rx_buffer),
            tcp::SocketBuffer::new(tx_buffer),
        ));
        Self {
            io: TcpIO {
                stack: &stack.socket_stack,
                handle,
            },
        }
    }

    /// Returns the maximum number of bytes inside the recv buffer.
    pub fn recv_capacity(&self) -> usize {
        self.io.recv_capacity()
    }

    /// Returns the maximum number of bytes inside the transmit buffer.
    pub fn send_capacity(&self) -> usize {
        self.io.send_capacity()
    }

    /// Returns the amount of octets queued in the transmit buffer.
    pub fn send_queue(&self) -> usize {
        self.io.send_queue()
    }

    /// Returns the amount of octets queued in the receive buffer. This value can be larger than
    /// the slice read by the next `recv` or `peek` call because it includes all queued octets,
    /// and not only the octets that may be returned as a contiguous slice.
    pub fn recv_queue(&self) -> usize {
        self.io.recv_queue()
    }

    /// Calls `f` with the largest contiguous slice of octets in the transmit buffer,
    /// and enqueue the amount of elements returned by `f`.
    ///
    /// If the socket is not ready to accept data, it waits until it is.
    pub async fn write_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.io.write_with(f).await
    }

    /// Calls `f` with the largest contiguous slice of octets in the receive buffer,
    /// and dequeue the amount of elements returned by `f`.
    ///
    /// If no data is available, it waits until there is at least one byte available.
    pub async fn read_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        self.io.read_with(f).await
    }

    /// Splits the socket into reader and a writer halves.
    pub fn split(&mut self) -> (TcpReader<'_>, TcpWriter<'_>) {
        (TcpReader { io: self.io }, TcpWriter { io: self.io })
    }

    /// Opens a TCP connection to a remote host.
    pub async fn connect<T: Into<IpEndpoint>>(
        &mut self,
        remote_endpoint: T,
    ) -> Result<(), ConnectError> {
        match {
            let local_port = self.io.stack.borrow_mut().next_local_port();
            self.io
                .with_mut(|s, i| s.connect(i.context(), remote_endpoint, local_port))
        } {
            Ok(()) => {}
            Err(tcp::ConnectError::InvalidState) => return Err(ConnectError::InvalidState),
            Err(tcp::ConnectError::Unaddressable) => return Err(ConnectError::NoRoute),
        }

        poll_fn(|cx| self.poll_connect(cx)).await
    }

    /// Polls the socket once to open a TCP connection to a remote host.
    #[inline]
    pub fn poll_connect(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ConnectError>> {
        self.io.with_mut(|s, _| match s.state() {
            tcp::State::Closed | tcp::State::TimeWait => {
                Poll::Ready(Err(ConnectError::ConnectionReset))
            }
            tcp::State::Listen => unreachable!(),
            tcp::State::SynSent | tcp::State::SynReceived => {
                s.register_send_waker(cx.waker());
                Poll::Pending
            }
            _ => Poll::Ready(Ok(())),
        })
    }

    /// Accepts a connection from a remote host.
    ///
    /// This function puts the socket in listening mode, and waits until a connection is received.
    pub async fn accept<T: Into<IpListenEndpoint>>(
        &mut self,
        local_endpoint: T,
    ) -> Result<(), AcceptError> {
        match self.io.with_mut(|s, _| s.listen(local_endpoint)) {
            Ok(()) => {}
            Err(tcp::ListenError::InvalidState) => return Err(AcceptError::InvalidState),
            Err(tcp::ListenError::Unaddressable) => return Err(AcceptError::InvalidPort),
        }
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    /// Polls the socket once to accept a connection from a remote host.
    #[inline]
    pub fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), AcceptError>> {
        self.io.with_mut(|s, _| match s.state() {
            tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived => {
                s.register_send_waker(cx.waker());
                Poll::Pending
            }
            _ => Poll::Ready(Ok(())),
        })
    }

    /// Reads data from the socket.
    ///
    /// Returns how many bytes were read, or an error. If no data is available, it waits
    /// until there is at least one byte available.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.io.read(buf).await
    }

    /// Polls the socket once to read data from the socket.
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        self.io.poll_read(cx, buf)
    }

    /// Peeks data from the remote address to which it is connected, without removing
    /// that data from the queue. On success, returns the number of bytes peeked.
    pub async fn peek(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.io.peek(buf).await
    }

    /// Polls the socket once to peeks data from the remote address.
    pub fn poll_peek(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        self.io.poll_peek(cx, buf)
    }

    /// Writes data to the socket.
    ///
    /// Returns how many bytes were written, or an error. If the socket is not ready to
    /// accept data, it waits until it is.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.io.write(buf).await
    }

    /// Polls the socket once to write data to the socket.
    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        self.io.poll_write(cx, buf)
    }

    /// Flushes the written data to the socket.
    ///
    /// This waits until all data has been sent, and ACKed by the remote host. For a connection
    /// closed with [`abort()`](TcpSocket::abort) it will wait for the TCP RST packet to be sent.
    pub async fn flush(&mut self) -> Result<(), Error> {
        self.io.flush().await
    }

    /// Polls the socket once to flush the written data to the socket.
    pub fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.io.poll_flush(cx)
    }

    /// Sets the timeout for the socket.
    ///
    /// If the timeout is set, the socket will be closed if no data is received for the
    /// specified duration.
    pub fn set_timeout<D: Into<Duration>>(&mut self, duration: Option<D>) {
        self.io
            .with_mut(|s, _| s.set_timeout(duration.map(|d| d.into())))
    }

    /// Sets the keep-alive interval for the socket.
    ///
    /// If the keep-alive interval is set, the socket will send keep-alive packets after
    /// the specified duration of inactivity.
    ///
    /// If not set, the socket will not send keep-alive packets.
    pub fn set_keep_alive<D: Into<Duration>>(&mut self, interval: Option<D>) {
        self.io
            .with_mut(|s, _| s.set_keep_alive(interval.map(|d| d.into())))
    }

    /// Sets the time-to-live (IPv4) or hop limit (IPv6) value used in outgoing packets.
    /// A socket without an explicitly set hop limit value uses the default IANA recommended value (64)
    pub fn set_hop_limit(&mut self, hop_limit: Option<u8>) {
        self.io.with_mut(|s, _| s.set_hop_limit(hop_limit))
    }

    /// Returns the local endpoint of the socket.
    ///
    /// Returns `None` if the socket is not bound (listening) or not connected.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.io
            .with(|s, _| s.local_endpoint())
            .map(smoltcp_helper::socket_addr_from)
    }

    /// Returns the remote endpoint of the socket.
    ///
    /// Returns `None` if the socket is not connected.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.io
            .with(|s, _| s.remote_endpoint())
            .map(smoltcp_helper::socket_addr_from)
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O on the specified
    /// portions to return immediately with an appropriate value (see the
    /// documentation of `Shutdown`).
    pub fn shutdown(&mut self, how: Shutdown) -> Result<(), Error> {
        match how {
            Shutdown::Read => Err(Error::InvalidInput),
            Shutdown::Write => {
                self.close();
                Ok(())
            }
            Shutdown::Both => {
                self.abort();
                Ok(())
            }
        }
    }

    /// Closes the write half of the socket.
    ///
    /// This closes only the write half of the socket. The read half side remains open, the
    /// socket can still receive data.
    ///
    /// Data that has been written to the socket and not yet sent (or not yet ACKed) will still
    /// still sent. The last segment of the pending to send data is sent with the FIN flag set.
    pub fn close(&mut self) {
        self.io.with_mut(|s, _| s.close())
    }

    /// Forcibly closes the socket.
    ///
    /// This instantly closes both the read and write halves of the socket. Any pending data
    /// that has not been sent will be lost.
    ///
    /// Note that the TCP RST packet is not sent immediately - if the `TcpSocket` is dropped too soon
    /// the remote host may not know the connection has been closed.
    /// `abort()` callers should wait for a [`flush()`](TcpSocket::flush) call to complete before
    /// dropping or reusing the socket.
    pub fn abort(&mut self) {
        self.io.with_mut(|s, _| s.abort())
    }
}

impl<'a> Drop for TcpSocket<'a> {
    fn drop(&mut self) {
        self.io.stack.borrow_mut().sockets.remove(self.io.handle);
    }
}

/// A TcpIO to wrap smoltcp tcp::Socket Input/Output.
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
        self.with_mut(|s, _| match (s.can_recv(), s.may_recv()) {
            (true, _) => {
                Poll::Ready(match s.recv(f) {
                    // Connection reset. TODO: this can also be timeouts etc, investigate.
                    Err(tcp::RecvError::Finished) | Err(tcp::RecvError::InvalidState) => {
                        Err(Error::ConnectionReset)
                    }
                    Ok(r) => Ok(r),
                })
            }
            (false, true) => {
                // socket buffer is empty wait until it has atleast one byte has arrived
                s.register_recv_waker(cx.waker());
                Poll::Pending
            }
            (false, false) => {
                // if we can't receive because the receive half of the duplex connection is closed then return an error
                Poll::Ready(Err(Error::ConnectionReset))
            }
        })
    }

    async fn peek(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        poll_fn(|cx| self.poll_peek(cx, buf)).await
    }

    fn poll_peek(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize, Error>> {
        // CAUTION: smoltcp semantics around EOF are different to what you'd expect
        // from posix-like IO, so we have to tweak things here.
        self.with_mut(|s, _| match s.peek_slice(buf) {
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

    fn poll_peek_with<F, R>(
        &mut self,
        cx: &mut Context<'_>,
        size: usize,
        f: F,
    ) -> Poll<Result<R, Error>>
    where
        F: FnOnce(&[u8]) -> R,
    {
        if size == 0 {
            return Poll::Ready(Err(Error::InvalidInput));
        }
        self.with_mut(|s, _| match (s.can_recv(), s.may_recv()) {
            (true, _) => {
                Poll::Ready(match s.peek(size) {
                    // Connection reset. TODO: this can also be timeouts etc, investigate.
                    Err(tcp::RecvError::Finished) | Err(tcp::RecvError::InvalidState) => {
                        Err(Error::ConnectionReset)
                    }
                    Ok(buf) => Ok(f(buf)),
                })
            }
            (false, true) => {
                // socket buffer is empty wait until it has atleast one byte has arrived
                s.register_recv_waker(cx.waker());
                Poll::Pending
            }
            (false, false) => {
                // if we can't receive because the receive half of the duplex connection is closed then return an error
                Poll::Ready(Err(Error::ConnectionReset))
            }
        })
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        poll_fn(|cx| self.poll_write(cx, buf)).await
    }

    async fn write_with<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut [u8]) -> (usize, R),
    {
        let mut f = Some(f);
        poll_fn(move |cx| self.poll_write_with(cx, f.take().unwrap())).await
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
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
        self.with_mut(|s, _| match (s.can_send(), s.may_send()) {
            (true, _) => {
                Poll::Ready(match s.send(f) {
                    // Connection reset. TODO: this can also be timeouts etc, investigate.
                    Err(tcp::SendError::InvalidState) => Err(Error::ConnectionReset),
                    Ok(r) => Ok(r),
                })
            }
            (false, true) => {
                // socket buffer is full wait until it has atleast one byte free
                s.register_send_waker(cx.waker());
                Poll::Pending
            }
            (false, false) => {
                // if we can't transmit because the transmit half of the duplex connection is closed then return an error
                Poll::Ready(Err(Error::ConnectionReset))
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

mod smoltcp_helper {
    use core::net::SocketAddr;
    use smoltcp::wire::IpEndpoint;

    pub(crate) fn socket_addr_from(endpoint: IpEndpoint) -> SocketAddr {
        (endpoint.addr, endpoint.port).into()
    }
}
