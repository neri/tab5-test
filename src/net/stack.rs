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

use smoltcp::config::DNS_MAX_SERVER_COUNT;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, dns, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address,
    Ipv4Cidr,
};

use crate::net::device::StationDevice;
use crate::wifi::Rpc;
use crate::{delay, tick, uart};

/// The IPv4 settings currently in effect.
pub struct Ipv4Config {
    pub address: Ipv4Cidr,
    pub router: Option<Ipv4Address>,
    /// The resolvers in effect. Truncated to what the DNS socket can hold,
    /// so what is reported is what is actually asked -- a list longer than
    /// the socket's capacity would otherwise be shown in full while only
    /// its head was ever queried.
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
    /// The DNS client, which unlike every other client here is permanent.
    ///
    /// `Interface::poll` only retransmits and delivers for sockets that are
    /// in the set, so a socket created when a query starts would miss
    /// answers that arrive while nobody is pumping. The other clients are
    /// conversations that exist only while their command runs; a resolver
    /// setting has the same lifetime as the lease it came from.
    dns: SocketHandle,
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
        let mut sockets = SocketSet::new(Vec::new());
        // No resolvers yet: there is no address either, and a query needs a
        // source address to go out from. `alloc` lets the query slots grow
        // on demand, so the socket costs nothing until a name is asked for.
        let dns = sockets.add(dns::Socket::new(&[], Vec::new()));
        Stack {
            interface,
            sockets,
            dhcp: None,
            dns,
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

    /// The resolvers the DNS socket will ask, in order.
    ///
    /// Empty is not the same as "the query will fail": it is the reason a
    /// query must not be started at all. smoltcp reads an empty server list
    /// as "every server has already been tried", so the query fails on the
    /// first dispatch without a packet leaving the board -- which looks
    /// exactly like the name not existing.
    pub fn dns_servers(&self) -> &[Ipv4Address] {
        match &self.config {
            Some(config) => &config.dns_servers,
            None => &[],
        }
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

    /// Starts the DHCP client from scratch, discarding any address
    /// currently set.
    pub fn start_dhcp(&mut self) {
        self.clear_addresses();
        let handle = match self.dhcp {
            Some(handle) => handle,
            None => {
                let handle = self.sockets.add(dhcpv4::Socket::new());
                self.dhcp = Some(handle);
                handle
            }
        };
        // A socket that has already held a lease sits in its renewing
        // state, and taking the address off the interface tells it
        // nothing -- it still believes the lease is good. Without this
        // reset it sends no discover at all and waits for its own renew
        // timer, which is minutes or hours away, while this command times
        // out and blames the access point.
        //
        // Resetting a socket that never held a lease does nothing, so this
        // is unconditional rather than a special case for the reused one.
        self.sockets.get_mut::<dhcpv4::Socket>(handle).reset();
        self.source = AddressSource::Dhcp;
    }

    /// Gives up the address and stops asking for another.
    ///
    /// Stopping the client is the part that is easy to leave out and hard
    /// to notice: a socket left renewing quietly reinstalls the address
    /// some minutes later, and whoever typed `release` has no reason to be
    /// watching for it to come back.
    pub fn release(&mut self) {
        if let Some(handle) = self.dhcp.take() {
            self.sockets.remove(handle);
        }
        self.clear_addresses();
        self.source = AddressSource::None;
    }

    /// Stops the DHCP client and installs an address by hand. This is what
    /// the first bring-up on a new network uses, and what stays available
    /// when a DHCP server is not to be trusted with the answer.
    pub fn set_static(
        &mut self,
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        dns_servers: Vec<Ipv4Address>,
    ) {
        if let Some(handle) = self.dhcp.take() {
            self.sockets.remove(handle);
        }
        self.clear_addresses();
        self.source = AddressSource::Static;
        self.apply_config(Ipv4Config {
            address,
            router,
            dns_servers,
            server: None,
            acquired_ms: tick::now_ms(),
        });
    }

    /// Drops the address, the default route and the resolvers, leaving the
    /// interface up.
    ///
    /// The resolvers go with the address deliberately. They were learned
    /// over a configuration that no longer holds, and keeping them would be
    /// the same kind of lie as keeping an address obtained over a link that
    /// is gone -- with the added trap that a query against a stale resolver
    /// fails by timing out rather than by saying anything useful.
    pub fn clear_addresses(&mut self) {
        self.interface
            .update_ip_addrs(|addresses| addresses.clear());
        let _ = self.interface.routes_mut().remove_default_ipv4_route();
        self.config = None;
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .update_servers(&[]);
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

    /// Installs an address, default route and resolvers on the interface
    /// and records them as the current configuration.
    fn apply_config(&mut self, mut config: Ipv4Config) {
        // Cut the resolver list down here rather than at each caller, so
        // the configuration never claims a server the socket will not ask.
        config.dns_servers.truncate(DNS_MAX_SERVER_COUNT);

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
        self.install_dns_servers();
    }

    /// Replaces the resolvers by hand, leaving the address alone.
    ///
    /// Returns whether there was a configuration to change. This works on a
    /// DHCP lease as well as a static address, which is what makes it
    /// possible to point the stack at a deliberately unreachable resolver
    /// and watch the fallback to the next one happen.
    pub fn set_dns_servers(&mut self, servers: Vec<Ipv4Address>) -> bool {
        let Some(config) = &mut self.config else {
            return false;
        };
        config.dns_servers = servers;
        config.dns_servers.truncate(DNS_MAX_SERVER_COUNT);
        self.install_dns_servers();
        true
    }

    /// Copies the current configuration's resolvers into the DNS socket.
    ///
    /// smoltcp keeps the list privately with no way to read it back, so the
    /// configuration stays the single source of truth and this is the only
    /// place the two are allowed to diverge -- for the length of this
    /// function.
    fn install_dns_servers(&mut self) {
        let mut servers = [IpAddress::Ipv4(Ipv4Address::UNSPECIFIED); DNS_MAX_SERVER_COUNT];
        let mut count = 0;
        if let Some(config) = &self.config {
            // `apply_config` and `set_dns_servers` already truncate, so this
            // never actually drops one. It is here so the bound on the array
            // is enforced where the array is written, not two functions away.
            for server in config.dns_servers.iter().take(DNS_MAX_SERVER_COUNT) {
                servers[count] = IpAddress::Ipv4(*server);
                count += 1;
            }
        }
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .update_servers(&servers[..count]);
    }

    /// Starts a query for `name`'s A records.
    ///
    /// Like [`connect_tcp`](Self::connect_tcp) this cannot live outside the
    /// struct: starting a query needs the interface's context for the
    /// transaction id and source port as well as the socket itself, and a
    /// caller holding one `&mut Stack` cannot borrow both halves at once.
    pub fn start_dns_query(
        &mut self,
        name: &str,
    ) -> Result<dns::QueryHandle, dns::StartQueryError> {
        let context = self.interface.context();
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .start_query(context, name, DnsQueryType::A)
    }

    /// The DNS socket, for collecting or cancelling a started query.
    pub fn dns_socket_mut(&mut self) -> &mut dns::Socket<'static> {
        self.sockets.get_mut::<dns::Socket>(self.dns)
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
