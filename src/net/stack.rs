//! The IPv4 interface: smoltcp's `Interface`, its socket set, and the DHCP
//! client that fills in the address.
//!
//! A `Stack` is deliberately *not* the owner of the C6 link. `wifi::Rpc`
//! keeps the transport, and every call that touches the wire takes a
//! `&mut Rpc` for the duration -- which is what lets an RPC command and a
//! packet-driving command coexist without either holding the other's
//! borrow across a shell command.
//!
//! Time comes from `tick::now_ms`. smoltcp requires a monotonic clock: an
//! `Instant` that goes backwards corrupts every retransmit and lease timer
//! it drives, which rules out `delay`'s wrapping cycle counter.

use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr};

use crate::net::device::StationDevice;
use crate::wifi::Rpc;
use crate::{delay, tick, uart};

/// The IPv4 settings currently in effect.
pub struct Ipv4Config {
    pub address: Ipv4Cidr,
    pub router: Option<Ipv4Address>,
    pub dns_servers: Vec<Ipv4Address>,
    /// The DHCP server that granted this, if it came from DHCP.
    pub server: Option<Ipv4Address>,
    /// `tick::now_ms` when the configuration was applied.
    pub acquired_ms: u64,
}

/// Where the current address came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    /// No address; nothing above ARP will work.
    None,
    /// A DHCP client is running. It may or may not have an address yet.
    Dhcp,
    /// Set by hand from the shell.
    Static,
}

pub struct Stack {
    interface: Interface,
    sockets: SocketSet<'static>,
    /// Present exactly while [`AddressSource::Dhcp`] is selected.
    dhcp: Option<SocketHandle>,
    source: AddressSource,
    config: Option<Ipv4Config>,
    mac: [u8; 6],
    /// How many times a lease has been lost since the stack was built.
    deconfigured: u32,
}

impl Stack {
    /// Builds the interface. The hardware address is the C6's own station
    /// MAC, which the slave reports over RPC -- an interface answering ARP
    /// for an address the radio does not use would never see a reply.
    pub fn new(rpc: &mut Rpc, mac: [u8; 6]) -> Self {
        let mut device = StationDevice::new(rpc);
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        // The seed only has to differ between boots, so two runs do not
        // pick the same TCP ports and initial sequence numbers. The cycle
        // counter read here is tens of millions of cycles after reset and
        // depends on when the operator typed the command.
        config.random_seed =
            ((delay::cycle_count() as u64) << 16) ^ ((mac[4] as u64) << 8) ^ mac[5] as u64;

        let interface = Interface::new(config, &mut device, now());
        Stack {
            interface,
            sockets: SocketSet::new(Vec::new()),
            dhcp: None,
            source: AddressSource::None,
            config: None,
            mac,
            deconfigured: 0,
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn source(&self) -> AddressSource {
        self.source
    }

    pub fn config(&self) -> Option<&Ipv4Config> {
        self.config.as_ref()
    }

    pub fn deconfigured_count(&self) -> u32 {
        self.deconfigured
    }

    /// Whether there is an address to send from. Every command above ARP
    /// checks this first, because smoltcp silently drops egress from an
    /// interface with no address rather than reporting an error.
    pub fn has_address(&self) -> bool {
        self.config.is_some()
    }

    pub fn sockets_mut(&mut self) -> &mut SocketSet<'static> {
        &mut self.sockets
    }

    /// Opens a TCP connection.
    ///
    /// Connecting needs the interface's context *and* the socket, and both
    /// live in this struct, so a caller holding one `&mut Stack` cannot
    /// split them apart -- the call has to be made from in here.
    pub fn connect_tcp(
        &mut self,
        handle: SocketHandle,
        remote: IpEndpoint,
        local_port: u16,
    ) -> Result<(), tcp::ConnectError> {
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        socket.connect(self.interface.context(), remote, local_port)
    }

    /// Starts the DHCP client, discarding any address currently set.
    pub fn start_dhcp(&mut self) {
        self.clear_addresses();
        if self.dhcp.is_none() {
            self.dhcp = Some(self.sockets.add(dhcpv4::Socket::new()));
        }
        self.source = AddressSource::Dhcp;
    }

    /// Stops the DHCP client and installs an address by hand. This is what
    /// the first bring-up on a new network uses, and what stays available
    /// when a DHCP server is not to be trusted with the answer.
    pub fn set_static(&mut self, address: Ipv4Cidr, router: Option<Ipv4Address>) {
        if let Some(handle) = self.dhcp.take() {
            self.sockets.remove(handle);
        }
        self.clear_addresses();
        self.source = AddressSource::Static;
        self.apply_config(Ipv4Config {
            address,
            router,
            dns_servers: Vec::new(),
            server: None,
            acquired_ms: tick::now_ms(),
        });
    }

    /// Drops the address and the default route, leaving the interface up.
    pub fn clear_addresses(&mut self) {
        self.interface
            .update_ip_addrs(|addresses| addresses.clear());
        let _ = self.interface.routes_mut().remove_default_ipv4_route();
        self.config = None;
    }

    /// One turn of the crank: read whatever the C6 has queued, let smoltcp
    /// consume and produce packets, then act on any DHCP event.
    ///
    /// This has to be called often. The C6 keeps received frames until the
    /// host reads them, and once the pending total outgrows the transport's
    /// staging buffer the link cannot resynchronize -- so a command that
    /// blocks for seconds runs its own pump loop rather than leaving the
    /// frame loop to do it.
    pub fn poll(&mut self, rpc: &mut Rpc) -> bool {
        if !rpc.is_alive() {
            return false;
        }

        // The slave can hold more frames than the receive queue does, so
        // fill the queue, let the interface empty it, and go round again.
        // Bounded, because a busy network must not pin the frame loop.
        for _ in 0..POLL_ROUNDS {
            let more = rpc.service();
            let mut device = StationDevice::new(rpc);
            let _ = self.interface.poll(now(), &mut device, &mut self.sockets);
            if !more {
                break;
            }
        }

        self.poll_dhcp();
        true
    }

    /// Applies whatever the DHCP client has decided since the last poll.
    ///
    /// The event borrows the socket set, and acting on it needs the
    /// interface and this stack's own fields, so the parts that matter are
    /// copied out first and the event dropped.
    fn poll_dhcp(&mut self) {
        let Some(handle) = self.dhcp else {
            return;
        };

        let outcome = match self.sockets.get_mut::<dhcpv4::Socket>(handle).poll() {
            None => return,
            Some(dhcpv4::Event::Deconfigured) => None,
            Some(dhcpv4::Event::Configured(configuration)) => Some(Ipv4Config {
                address: configuration.address,
                router: configuration.router,
                dns_servers: configuration.dns_servers.iter().copied().collect(),
                server: Some(configuration.server.address),
                acquired_ms: tick::now_ms(),
            }),
        };

        match outcome {
            None => {
                // Only report a lease that was actually held: the socket
                // also reports `Deconfigured` on its way to the first
                // offer, before anything has been configured.
                if self.config.is_some() {
                    self.deconfigured = self.deconfigured.saturating_add(1);
                    uart::log(b"NET: DHCP lease lost\r\n");
                }
                self.clear_addresses();
            }
            Some(config) => {
                uart::log(b"NET: DHCP configured\r\n");
                self.apply_config(config);
            }
        }
    }

    /// Installs an address and default route on the interface and records
    /// them as the current configuration.
    fn apply_config(&mut self, config: Ipv4Config) {
        let mut installed = false;
        self.interface.update_ip_addrs(|addresses| {
            addresses.clear();
            installed = addresses.push(IpCidr::Ipv4(config.address)).is_ok();
        });
        if !installed {
            // Nothing above ARP can work without this, and the failure is
            // otherwise completely silent: the interface simply ignores
            // every packet aimed at the address the shell just printed.
            uart::log(b"NET: could not install the address on the interface\r\n");
        }
        let _ = self.interface.routes_mut().remove_default_ipv4_route();
        if let Some(router) = config.router
            && self
                .interface
                .routes_mut()
                .add_default_ipv4_route(router)
                .is_err()
        {
            uart::log(b"NET: could not install the default route\r\n");
        }
        self.config = Some(config);
    }

    /// Polls until `predicate` is satisfied or `timeout_ms` of real time
    /// has passed. Returns whether the predicate won.
    ///
    /// This is the shape every blocking network command has: the shell is
    /// single-threaded, so while one runs nothing else services the link.
    pub fn pump_until(
        &mut self,
        rpc: &mut Rpc,
        timeout_ms: u64,
        mut predicate: impl FnMut(&mut Stack) -> bool,
    ) -> bool {
        if !tick::is_running() {
            // Without the tick there is no deadline and no smoltcp clock,
            // so this would spin forever on a stack that cannot retransmit.
            uart::log(b"NET: the millisecond tick is not running\r\n");
            return false;
        }

        let deadline = tick::now_ms() + timeout_ms;
        loop {
            if !self.poll(rpc) {
                return false;
            }
            if predicate(self) {
                return true;
            }
            if tick::now_ms() >= deadline {
                return false;
            }
        }
    }
}

/// How many fill-and-drain rounds one [`Stack::poll`] runs before returning
/// to the caller, whatever the slave still has waiting.
const POLL_ROUNDS: u32 = 4;

/// smoltcp's clock, taken from the 1 kHz tick.
pub fn now() -> Instant {
    Instant::from_millis(tick::now_ms() as i64)
}
