# Netstack Smoltcp

> Is this really a fork? No, it's definitely not what you think. I lost my admin privileges on the original upstream org, but that wasn't me. Imagine how many orgs on GitHub are backed by companies, individuals, or something else entirely. You should submit issues and pull requests in this repository, not the upstream org. I'm fed up with those orgs masquerading as open source. They manipulate my privileges at will, and I've lost the confidence to continue maintaining them. Perhaps that's not confidence, but trust.

A netstack for the special purpose of turning packets from/to a TUN interface into TCP streams and UDP packets. It uses smoltcp-rs as the backend netstack.

[![Crates.io][crates-badge]][crates-url]
[![MIT licensed][mit-badge]][mit-url]
[![Apache licensed, Version 2.0][apache-badge]][apache-url]
[![Build Status][actions-badge]][actions-url]

[crates-badge]: https://img.shields.io/crates/v/netstack-smoltcp.svg
[crates-url]: https://crates.io/crates/netstack-smoltcp
[mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg
[mit-url]: https://github.com/automesh-network/netstack-smoltcp/blob/master/LICENSE-MIT
[apache-badge]: https://img.shields.io/badge/license-APACHE2.0-blue.svg
[apache-url]: https://github.com/automesh-network/netstack-smoltcp/blob/master/LICENSE-APACHE
[actions-badge]: https://github.com/automesh-network/netstack-smoltcp/workflows/CI/badge.svg
[actions-url]: https://github.com/automesh-network/netstack-smoltcp/actions?query=workflow%3ACI+branch%3Amain

## Features

- Supports Future Send and non-Send, mostly pepole use Send.
- Supports ICMP protocol drive by TCP runner to use ICMP ping.
- Supports filtering packets by source and destination IP addresses.
- Can read IP packets from netstack, write IP packets to netstack.
- Can receive TcpStream from TcpListener exposed from netstack.
- Can receive UDP datagram from UdpSocket exposed from netstack.
- Implements popular future streaming traits and asynchronous IO traits:
    * TcpListener implements futures Stream/Sink trait
    * TcpStream implements tokio AsyncRead/AsyncWrite trait
    * UdpSocket(ReadHalf/WriteHalf) implements futures Stream/Sink trait.

## Platforms

This crate provides lightweight netstack support for Linux, iOS, macOS, Android and Windows.
Currently, it works on most targets, but mainly tested the popular platforms which includes:
- linux-amd64: x86_64-unknown-linux-gnu
- android-arm64: aarch64-linux-android
- android-amd64: x86_64-linux-android
- macos-amd64: x86_64-apple-darwin
- macos-arm64: aarch64-apple-darwin
- ios-arm64: aarch64-apple-ios
- windows-amd64: x86_64-pc-windows-msvc
- windows-arm64: aarch64-pc-windows-msvc

## Example

```rust
// let device = tun2::create_as_async(&cfg)?;
// let framed = device.into_framed();

let (stack, runner, udp_socket, tcp_listener) = netstack_smoltcp::StackBuilder::default()
    .stack_buffer_size(512)
    .tcp_buffer_size(4096)
    .enable_udp(true)
    .enable_tcp(true)
    .enable_icmp(true)
    .build()
    .unwrap();
let mut udp_socket = udp_socket.unwrap(); // udp enabled
let mut tcp_listener = tcp_listener.unwrap(); // tcp/icmp enabled
if let Some(runner) = runner {
    tokio::spawn(runner);
}

let (mut stack_sink, mut stack_stream) = stack.split();
let (mut tun_sink, mut tun_stream) = framed.split();

// Reads packet from stack and sends to TUN.
tokio::spawn(async move {
    while let Some(pkt) = stack_stream.next().await {
        if let Ok(pkt) = pkt {
            tun_sink.send(pkt).await.unwrap();
        }
    }
});

// Reads packet from TUN and sends to stack.
tokio::spawn(async move {
    while let Some(pkt) = tun_stream.next().await {
        if let Ok(pkt) = pkt {
            stack_sink.send(pkt).await.unwrap();
        }
    }
});

// Extracts TCP connections from stack and sends them to the dispatcher.
tokio::spawn(async move {
    handle_inbound_stream(tcp_listener).await;
});

// Receive and send UDP packets between netstack and NAT manager. The NAT
// manager would maintain UDP sessions and send them to the dispatcher.
tokio::spawn(async move {
    handle_inbound_datagram(udp_socket).await;
});
```

## License

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in netstack-smoltcp by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.

## Inspired By

Special thanks to these amazing projects that inspired netstack-smoltcp (in no particular order):
- [shadowsocks-rust](https://github.com/shadowsocks/shadowsocks-rust/)
- [netstack-lwip](https://github.com/eycorsican/netstack-lwip/)
- [rust-tun-active](https://github.com/tun2proxy/rust-tun)
- [rust-tun](https://github.com/meh/rust-tun/)
- [smoltcp](https://github.com/smoltcp-rs/smoltcp)
