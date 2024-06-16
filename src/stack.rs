use smoltcp::iface::{Interface, SocketSet};

use crate::waker::WrapWaker;

use core::{
    cell::RefCell,
    future::{poll_fn, Future},
    pin::pin,
    task::{Context, Poll},
};

use smoltcp::phy::Device;

pub(crate) struct SocketStack {
    pub(crate) sockets: SocketSet<'static>,
    pub(crate) iface: Interface,
    pub(crate) waker: WrapWaker,
}

pub struct Stack<D: Device> {
    pub(crate) socket_stack: RefCell<SocketStack>,
    pub(crate) stack_inner: RefCell<StackInner<D>>,
}

impl<D: Device> Stack<D> {
    pub fn new(mut device: D) -> Self {
        let mut iface_cfg = smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip);
        iface_cfg.random_seed = rand::random();
        let mut iface = Interface::new(iface_cfg, &mut device, smoltcp_helper::instant_now());
        {
            use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address, Ipv6Address};
            iface.update_ip_addrs(|ip_addrs| {
                ip_addrs
                    .push(IpCidr::new(IpAddress::v4(0, 0, 0, 1), 0))
                    .expect("iface IPv4");
                ip_addrs
                    .push(IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 0))
                    .expect("iface IPv6");
            });
            iface
                .routes_mut()
                .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
                .expect("IPv4 default route");
            iface
                .routes_mut()
                .add_default_ipv6_route(Ipv6Address::new(0, 0, 0, 0, 0, 0, 0, 1))
                .expect("IPv6 default route");
            iface.set_any_ip(true);
        }
        let sockets = SocketSet::new(vec![]);
        let socket_stack = SocketStack {
            sockets,
            iface,
            waker: WrapWaker::new(),
        };

        let stack_inner = StackInner { device };

        Self {
            socket_stack: RefCell::new(socket_stack),
            stack_inner: RefCell::new(stack_inner),
        }
    }

    /// Run the network stack.
    ///
    /// You must call this in a background task, to process network events.
    pub async fn run(&self) -> ! {
        poll_fn(|cx| self.poll(cx)).await;
        unreachable!()
    }

    /// Poll the network stack once.
    ///
    /// You must call this in a background task loop, to process network events.
    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.with_mut(|s, i| i.poll(cx, s));
        Poll::<()>::Pending
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut SocketStack, &mut StackInner<D>) -> R) -> R {
        f(
            &mut *self.socket_stack.borrow_mut(),
            &mut *self.stack_inner.borrow_mut(),
        )
    }

    #[allow(unused)]
    fn with<R>(&self, f: impl FnOnce(&SocketStack, &StackInner<D>) -> R) -> R {
        f(&*self.socket_stack.borrow(), &*self.stack_inner.borrow())
    }
}

pub(crate) struct StackInner<D: Device> {
    device: D,
}

impl<D: Device> StackInner<D> {
    fn poll(&mut self, cx: &mut Context<'_>, stack: &mut SocketStack) {
        stack.waker.register(cx.waker());

        let timestamp = smoltcp_helper::instant_now();
        let device = &mut self.device;
        stack.iface.poll(timestamp, device, &mut stack.sockets);

        if let Some(poll_at) = stack.iface.poll_at(timestamp, &mut stack.sockets) {
            let t = pin!(smoltcp_helper::timer_from(poll_at));
            if t.poll(cx).is_ready() {
                cx.waker().wake_by_ref();
            }
        }
    }
}

mod smoltcp_helper {
    use async_timer::{new_timer, Timer};
    use core::time::Duration;
    use smoltcp::time::Instant;

    pub(crate) fn instant_now() -> Instant {
        #[cfg(feature = "std")]
        smoltcp::time::Instant::now()
    }

    pub(crate) fn timer_from(instant: Instant) -> impl Timer {
        let duration = (instant - instant_now()).micros();
        new_timer(Duration::from_micros(duration))
    }
}
