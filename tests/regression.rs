//! Regression tests that reproduce the bugs found in the static analysis.

use std::time::Duration;

use etherparse::{IpNumber, Ipv4Header, UdpHeader};
use futures::SinkExt;
use tokio::time::timeout;

use netstack_smoltcp::StackBuilder;

fn make_udp_ipv4(
    src_ip: [u8; 4],
    src_port: u16,
    dst_ip: [u8; 4],
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_hdr = UdpHeader::with_ipv4_checksum(
        src_port,
        dst_port,
        &Ipv4Header::new(
            (UdpHeader::LEN + payload.len()) as u16,
            64,
            IpNumber::UDP,
            src_ip,
            dst_ip,
        )
        .unwrap(),
        payload,
    )
    .unwrap();

    let ip_hdr = Ipv4Header::new(
        (UdpHeader::LEN + payload.len()) as u16,
        64,
        IpNumber::UDP,
        src_ip,
        dst_ip,
    )
    .unwrap();

    let mut buf = Vec::with_capacity(Ipv4Header::MIN_LEN + UdpHeader::LEN + payload.len());
    ip_hdr.write(&mut buf).unwrap();
    udp_hdr.write(&mut buf).unwrap();
    buf.extend_from_slice(payload);
    buf
}

/// before(include) a15e0b72bfc72cb032e67138070da01e325d66f8
/// sink_buf is used in `Stack` to hold a slot for sending any pkt
///
/// the original assumption is that the `poll_ready` -> `start_send` -> `poll_flush`
/// are called sequentially so the slot could be reused and will never get blocked.
///
/// but once the user calls `send_all` on `Stack`, which will not immediate flush the pkt(call `poll_flush`),
/// then `sink_buf` is could be Some(pkt), then it will trigger `Poll::Pending` branch in `Stack::poll_ready`,
/// who did not register the waker correctly, so it will got hanged forever.
#[tokio::test(flavor = "current_thread")]
async fn bug1_poll_ready_waker_registered_via_send_all() {
    let (mut stack, _runner, _udp_socket, _tcp) = StackBuilder::default()
        .enable_udp(true)
        .udp_buffer_size(64)
        .stack_buffer_size(64)
        .build()
        .unwrap();

    let pkt1 = make_udp_ipv4([1, 2, 3, 4], 1111, [5, 6, 7, 8], 9999, b"first");
    let pkt2 = make_udp_ipv4([1, 2, 3, 4], 1111, [5, 6, 7, 8], 9999, b"second");

    let mut stream = futures::stream::iter([Ok(pkt1), Ok(pkt2)]);

    let result = timeout(Duration::from_secs(1), stack.send_all(&mut stream)).await;
    // should be ok after the fix
    assert!(result.is_ok());
}
