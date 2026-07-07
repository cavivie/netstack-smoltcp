use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use smoltcp::{
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
};
use tokio::sync::mpsc::{unbounded_channel, Permit, Sender, UnboundedReceiver, UnboundedSender};

use crate::packet::AnyIpPktFrame;

pub(super) struct VirtualDevice {
    in_buf_avail: Arc<AtomicBool>,
    in_buf: UnboundedReceiver<Vec<u8>>,
    out_buf: Sender<AnyIpPktFrame>,
    mtu: usize,
}

impl VirtualDevice {
    pub(super) fn new(
        iface_egress_tx: Sender<AnyIpPktFrame>,
        mtu: usize,
    ) -> (Self, UnboundedSender<Vec<u8>>, Arc<AtomicBool>) {
        let iface_ingress_tx_avail = Arc::new(AtomicBool::new(false));
        let (iface_ingress_tx, iface_ingress_rx) = unbounded_channel();
        (
            Self {
                in_buf_avail: iface_ingress_tx_avail.clone(),
                in_buf: iface_ingress_rx,
                out_buf: iface_egress_tx,
                mtu,
            },
            iface_ingress_tx,
            iface_ingress_tx_avail,
        )
    }
}

impl Device for VirtualDevice {
    type RxToken<'a> = VirtualRxToken;
    type TxToken<'a> = VirtualTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // First, we reserve space in out_buf: if it is full, we exit without touching in_buf,
        // so that the waiting packet remains in the queue and is not extracted
        // and immediately discarded, and so that in_buf_avail is not reset while it is still true.
        let Ok(permit) = self.out_buf.try_reserve() else {
            return None;
        };

        let Ok(buffer) = self.in_buf.try_recv() else {
            self.in_buf_avail.store(false, Ordering::Release);
            return None;
        };

        Some((Self::RxToken { buffer }, Self::TxToken { permit }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        match self.out_buf.try_reserve() {
            Ok(permit) => Some(Self::TxToken { permit }),
            Err(_) => None,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = self.mtu;
        capabilities
    }
}

pub(super) struct VirtualRxToken {
    buffer: Vec<u8>,
}

impl RxToken for VirtualRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer[..])
    }
}

pub(super) struct VirtualTxToken<'a> {
    permit: Permit<'a, Vec<u8>>,
}

impl<'a> TxToken for VirtualTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        self.permit.send(buffer);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::sync::mpsc::channel;

    /// Regression test for the receive() ordering bug: reserving out_buf
    /// capacity before dequeuing in_buf must not lose the in_buf packet, and
    /// must not clear in_buf_avail while a real packet is still queued.
    #[test]
    fn receive_keeps_in_buf_packet_when_out_buf_is_full() {
        let (out_tx, mut out_rx) = channel::<AnyIpPktFrame>(1);
        let (mut device, ingress_tx, in_buf_avail)
            = VirtualDevice::new(out_tx.clone(), 1500);

        // Saturate the bounded out_buf (capacity 1) so try_reserve() fails.
        out_tx.try_send(vec![0u8; 4]).unwrap();

        // Queue an in_buf packet and mark it available, as handle_packet would.
        ingress_tx.send(b"payload".to_vec()).unwrap();
        in_buf_avail.store(true, Ordering::Release);

        // out_buf is full: receive() must yield nothing this poll...
        assert!(device.receive(Instant::from_millis(0)).is_none());
        // ...without discarding the queued in_buf packet or claiming there's no more work.
        assert!(
            in_buf_avail.load(Ordering::Acquire),
            "in_buf_avail must stay true: the in_buf packet is still pending, \
             only out_buf capacity was the blocker"
        );

        // Free up out_buf capacity.
        out_rx.try_recv().unwrap();

        // The packet must still be there, untouched, once out_buf has room again.
        let (rx_token, _tx_token) = device
            .receive(Instant::from_millis(1))
            .expect("in_buf packet must not have been dropped while out_buf was full");
        assert_eq!(rx_token.consume(|buf| buf.to_vec()), b"payload");
    }
}
