use std::{
    io,
    net::IpAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, Stream};
use smoltcp::wire::IpProtocol;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::{debug, trace};

use crate::{
    filter::{IpFilter, IpFilters},
    packet::{AnyIpPktFrame, IpPacket},
    runner::Runner,
    tcp::TcpListener,
    udp::UdpSocket,
};

pub struct StackBuilder {
    stack_buffer_size: usize,
    udp_buffer_size: usize,
    tcp_buffer_size: usize,
    ip_filters: IpFilters<'static>,
}

impl Default for StackBuilder {
    fn default() -> Self {
        Self {
            stack_buffer_size: 1024,
            udp_buffer_size: 512,
            tcp_buffer_size: 512,
            ip_filters: IpFilters::with_non_broadcast(),
        }
    }
}

#[allow(unused)]
impl StackBuilder {
    pub fn stack_buffer_size(mut self, size: usize) -> Self {
        self.stack_buffer_size = size;
        self
    }

    pub fn udp_buffer_size(mut self, size: usize) -> Self {
        self.udp_buffer_size = size;
        self
    }

    pub fn tcp_buffer_size(mut self, size: usize) -> Self {
        self.tcp_buffer_size = size;
        self
    }

    pub fn set_ip_filters(mut self, filters: IpFilters<'static>) -> Self {
        self.ip_filters = filters;
        self
    }

    pub fn add_ip_filter(mut self, filter: IpFilter<'static>) -> Self {
        self.ip_filters.add(filter);
        self
    }

    pub fn add_ip_filter_fn<F>(mut self, filter: F) -> Self
    where
        F: Fn(&IpAddr, &IpAddr) -> bool + Send + Sync + 'static,
    {
        self.ip_filters.add_fn(filter);
        self
    }

    pub fn build(self) -> (Runner, UdpSocket, TcpListener, Stack) {
        let (stack_tx, stack_rx) = channel(self.stack_buffer_size);
        let (udp_tx, udp_rx) = channel(self.udp_buffer_size);
        let (tcp_tx, tcp_rx) = channel(self.tcp_buffer_size);

        let udp_socket = UdpSocket::new(udp_rx, stack_tx.clone());
        let (tcp_runner, tcp_listener) = TcpListener::new(tcp_rx, stack_tx);
        let stack = Stack {
            ip_filters: self.ip_filters,
            sink_buf: None,
            stack_rx,
            udp_tx,
            tcp_tx,
        };

        (tcp_runner, udp_socket, tcp_listener, stack)
    }

    pub fn run(self) -> (UdpSocket, TcpListener, Stack) {
        let (tcp_runner, udp_socket, tcp_listener, stack) = self.build();
        tokio::task::spawn(tcp_runner);
        (udp_socket, tcp_listener, stack)
    }

    pub fn run_local(self) -> (UdpSocket, TcpListener, Stack) {
        let (tcp_runner, udp_socket, tcp_listener, stack) = self.build();
        tokio::task::spawn_local(tcp_runner);
        (udp_socket, tcp_listener, stack)
    }
}

pub struct Stack {
    ip_filters: IpFilters<'static>,
    sink_buf: Option<AnyIpPktFrame>,
    udp_tx: Sender<AnyIpPktFrame>,
    tcp_tx: Sender<AnyIpPktFrame>,
    stack_rx: Receiver<AnyIpPktFrame>,
}

// Recv from stack.
impl Stream for Stack {
    type Item = io::Result<AnyIpPktFrame>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.stack_rx.poll_recv(cx) {
            Poll::Ready(Some(pkt)) => Poll::Ready(Some(Ok(pkt))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// Send to stack.
impl Sink<AnyIpPktFrame> for Stack {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.sink_buf.is_none() {
            Poll::Ready(Ok(()))
        } else {
            self.poll_flush(cx)
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: AnyIpPktFrame) -> Result<(), Self::Error> {
        self.sink_buf.replace(item);
        Ok(())
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let Some(item) = self.sink_buf.take() else {
            return Poll::Ready(Ok(()));
        };

        if item.is_empty() {
            return Poll::Ready(Ok(()));
        }

        let packet = IpPacket::new_checked(item.as_slice()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid IP packet: {}", err),
            )
        })?;

        let src_ip = packet.src_addr();
        let dst_ip = packet.dst_addr();

        let addr_allowed = self.ip_filters.is_allowed(&src_ip, &dst_ip);
        if !addr_allowed {
            trace!(
                "IP packet {} -> {} (allowed? {}) throwing away",
                src_ip,
                dst_ip,
                addr_allowed,
            );
            return Poll::Ready(Ok(()));
        }

        match packet.protocol() {
            IpProtocol::Tcp => {
                self.tcp_tx
                    .try_send(item)
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
                Poll::Ready(Ok(()))
            }
            IpProtocol::Udp => {
                self.udp_tx
                    .try_send(item)
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
                Poll::Ready(Ok(()))
            }
            IpProtocol::Icmp | IpProtocol::Icmpv6 => {
                // ICMP is handled by TCP's Interface.
                // smoltcp's interface will always send replies to EchoRequest
                self.tcp_tx
                    .try_send(item)
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
                Poll::Ready(Ok(()))
            }
            protocol => {
                debug!("tun IP packet ignored (protocol: {:?})", protocol);
                Poll::Ready(Ok(()))
            }
        }
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.stack_rx.close();
        Poll::Ready(Ok(()))
    }
}
