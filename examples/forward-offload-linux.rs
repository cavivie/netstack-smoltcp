#[cfg(target_os = "linux")]
mod inner {
    use futures::{SinkExt, StreamExt};
    use netstack_smoltcp::{StackBuilder, TcpListener, UdpSocket};
    use std::{net::SocketAddr, sync::Arc};
    use structopt::StructOpt;
    use tokio::net::{TcpSocket, TcpStream};
    use tracing::{error, info, warn};
    use tun_rs::{DeviceBuilder, IDEAL_BATCH_SIZE, VIRTIO_NET_HDR_LEN};

    // Patched forward example: tun2 → tun-rs with Linux GRO/GSO offload.
    // For further reading, check out https://blog.cloudflare.com/virtual-networking-101-understanding-tap
    //
    // Key changes vs forward.rs:
    //   1. Use tun-rs DeviceBuilder with .offload(true) on Linux (enables
    //      IFF_VNET_HDR + TUN_F_CSUM/TSO4/TSO6/USO4/USO6).
    //   2. TX (stack → TUN): prepend 10-byte zero virtio_net_hdr (GSO_NONE)
    //      so the kernel accepts the write when IFF_VNET_HDR is set.
    //   3. RX (TUN → stack): use recv_multiple() for batch GSO splitting;
    //      buffers sized to 1600 to fit smoltcp's 1504-byte MTU segments.
    #[derive(Debug, StructOpt)]
    #[structopt(name = "forward", about = "Simply forward tun tcp/udp traffic.")]
    struct Opt {
        /// Outbound interface to bind forwarded connections to.
        #[structopt(short = "i", long = "interface")]
        interface: String,
        /// Name of the TUN device.
        #[structopt(short = "n", long = "name", default_value = "utun8")]
        name: String,
        /// Tracing log level.
        #[structopt(long = "log-level", default_value = "debug")]
        log_level: tracing::Level,
        /// Use current-thread Tokio runtime (default: multi-thread).
        #[structopt(long = "current-thread")]
        current_thread: bool,
        /// Use spawn_local instead of spawn.
        #[structopt(long = "local-task")]
        local_task: bool,
    }

    pub(super) fn main() {
        let opt = Opt::from_args();
        let rt = if opt.current_thread {
            tokio::runtime::Builder::new_current_thread()
        } else {
            tokio::runtime::Builder::new_multi_thread()
        }
        .enable_all()
        .build()
        .unwrap();
        rt.block_on(main_exec(opt));
    }

    async fn main_exec(opt: Opt) {
        macro_rules! tokio_spawn {
            ($fut:expr) => {
                if opt.local_task {
                    tokio::task::spawn_local($fut)
                } else {
                    tokio::task::spawn($fut)
                }
            };
        }
        tracing::subscriber::set_global_default(
            tracing_subscriber::FmtSubscriber::builder()
                .with_max_level(opt.log_level)
                .finish(),
        )
        .unwrap();

        // Build TUN device with GRO/GSO offload on Linux.
        let builder = DeviceBuilder::new()
            .name(opt.name)
            .ipv4("10.10.10.2", 24, Some("10.10.10.1"))
            .mtu(9000);
        let builder = builder.offload(true);
        let dev = Arc::new(builder.build_async().unwrap());

        let (stack, runner, udp_socket, tcp_listener) = StackBuilder::default()
            .enable_tcp(true)
            .enable_udp(true)
            .enable_icmp(true)
            .build()
            .unwrap();
        let udp_socket = udp_socket.unwrap();
        let tcp_listener = tcp_listener.unwrap();
        if let Some(runner) = runner {
            tokio_spawn!(runner);
        }
        let (mut stack_sink, mut stack_stream) = stack.split();

        let mut futs = vec![];

        // stack → TUN
        // With IFF_VNET_HDR every write must start with a virtio_net_hdr.
        // We use all-zero (gso_type = GSO_NONE, flags = 0): plain packet,
        // checksum already valid (smoltcp always computes checksums itself).
        let dev1 = dev.clone();
        futs.push(tokio_spawn!(async move {
            while let Some(pkt) = stack_stream.next().await {
                if let Ok(pkt) = pkt {
                    let result = {
                        let mut buf = vec![0u8; VIRTIO_NET_HDR_LEN + pkt.len()];
                        buf[VIRTIO_NET_HDR_LEN..].copy_from_slice(&pkt);
                        dev1.send(&buf).await
                    };
                    if let Err(e) = result {
                        warn!("failed to send packet to TUN: {:?}", e);
                    }
                }
            }
        }));

        // TUN → stack
        // recv_multiple() does one read() syscall and returns N individual IP
        // packets after splitting any incoming GRO super-packet.
        // Buffer size 1600 > smoltcp MTU (1504) to avoid an out-of-bounds panic
        // when the kernel segments at MSS=1464 with 40-byte IP+TCP headers.
        futs.push(tokio_spawn!(async move {
            let mut orig = vec![0u8; VIRTIO_NET_HDR_LEN + 65535];
            let mut bufs = vec![vec![0u8; 1600]; IDEAL_BATCH_SIZE];
            let mut sizes = vec![0usize; IDEAL_BATCH_SIZE];
            while let Ok(n) = dev.recv_multiple(&mut orig, &mut bufs, &mut sizes, 0).await {
                for i in 0..n {
                    let pkt = &bufs[i][..sizes[i]];
                    if let Err(e) = stack_sink.send(pkt.to_vec()).await {
                        warn!("failed to send packet to stack: {:?}", e);
                    }
                }
            }
        }));

        futs.push(tokio_spawn!({
            let iface = opt.interface.clone();
            async move {
                handle_inbound_stream(tcp_listener, iface).await;
            }
        }));

        futs.push(tokio_spawn!(async move {
            handle_inbound_datagram(udp_socket, opt.interface).await;
        }));

        futures::future::join_all(futs).await.iter().for_each(|r| {
            if let Err(e) = r {
                error!("{:?}", e);
            }
        });
    }

    async fn handle_inbound_stream(mut tcp_listener: TcpListener, interface: String) {
        while let Some((mut stream, local, remote)) = tcp_listener.next().await {
            let interface = interface.clone();
            tokio::spawn(async move {
                info!("tcp: {:?} => {:?}", local, remote);
                match new_tcp_stream(remote, &interface).await {
                    Ok(mut r) => {
                        if let Err(e) = tokio::io::copy_bidirectional(&mut stream, &mut r).await {
                            warn!(
                                "failed to copy tcp stream {:?}=>{:?}: {:?}",
                                local, remote, e
                            );
                        }
                    }
                    Err(e) => warn!(
                        "failed to open tcp stream {:?}=>{:?}: {:?}",
                        local, remote, e
                    ),
                }
            });
        }
    }

    async fn handle_inbound_datagram(udp_socket: UdpSocket, interface: String) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut read_half, mut write_half) = udp_socket.split();
        tokio::spawn(async move {
            while let Some((data, local, remote)) = rx.recv().await {
                let _ = write_half.send((data, remote, local)).await;
            }
        });
        while let Some((data, local, remote)) = read_half.next().await {
            let tx = tx.clone();
            let interface = interface.clone();
            tokio::spawn(async move {
                match new_udp_packet(remote, &interface).await {
                    Ok(sock) => {
                        let _ = sock.send(&data).await;
                        loop {
                            let mut buf = vec![0; 1024];
                            match sock.recv_from(&mut buf).await {
                                Ok((n, _)) => {
                                    let _ = tx.send((buf[..n].to_vec(), local, remote));
                                }
                                Err(e) => {
                                    warn!("udp recv {:?}: {:?}", remote, e);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => warn!("failed to open udp socket {:?}: {:?}", remote, e),
                }
            });
        }
    }

    async fn new_tcp_stream(addr: SocketAddr, iface: &str) -> std::io::Result<TcpStream> {
        use socket2_ext::{AddressBinding, BindDeviceOption};
        let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
        s.bind_to_device(BindDeviceOption::v4(iface))?;
        s.set_keepalive(true)?;
        s.set_nodelay(true)?;
        s.set_nonblocking(true)?;
        Ok(TcpSocket::from_std_stream(s.into()).connect(addr).await?)
    }

    async fn new_udp_packet(
        addr: SocketAddr,
        iface: &str,
    ) -> std::io::Result<tokio::net::UdpSocket> {
        use socket2_ext::{AddressBinding, BindDeviceOption};
        let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
        s.bind_to_device(BindDeviceOption::v4(iface))?;
        s.set_nonblocking(true)?;
        let sock = tokio::net::UdpSocket::from_std(s.into())?;
        sock.connect(addr).await?;
        Ok(sock)
    }
}

#[cfg(not(target_os = "linux"))]
mod inner {
    pub(super) fn main() {}
}

fn main() {
    inner::main();
}
