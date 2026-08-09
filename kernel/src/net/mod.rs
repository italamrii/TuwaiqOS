//! Phase 7 bounded IPv4 networking.
//!
//! The hardware path uses a polling VirtIO NIC and smoltcp protocol state.
//! No IRQ handler accesses this module.  A non-spinning atomic lease protects
//! the single network stack: competing callers receive `network busy` rather
//! than spinning while a preempted owner performs device I/O.

pub mod driver;
pub mod loopback;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, dns, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr,
};
use spin::Mutex;

use crate::drivers::virtio;
use crate::drivers::virtio::net::{VirtioNet, VirtioNetDevice};

use driver::NetDriver;
use loopback::LoopbackDriver;

const SLOT_UNAVAILABLE: u8 = 0;
const SLOT_READY: u8 = 1;
const SLOT_BUSY: u8 = 2;
const DHCP_TIMEOUT_TICKS: u64 = 1_500;
const DNS_TIMEOUT_TICKS: u64 = 800;
pub const MAX_UDP_PAYLOAD: usize = 1200;
const MAX_UDP_SOCKETS: usize = 8;
const UDP_PACKET_SLOTS: usize = 4;

static LOOPBACK: Mutex<LoopbackDriver> = Mutex::new(LoopbackDriver::new());
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static HARDWARE_PRESENT: AtomicBool = AtomicBool::new(false);
static STACK_STATE: AtomicU8 = AtomicU8::new(SLOT_UNAVAILABLE);

struct StackSlot(UnsafeCell<MaybeUninit<NetworkStack>>);

// Safety: STACK_STATE grants at most one mutable accessor.  No interrupt
// handler accesses the stack and initialization publishes with Release only
// after the complete value has been written.
unsafe impl Sync for StackSlot {}

static STACK: StackSlot = StackSlot(UnsafeCell::new(MaybeUninit::uninit()));

struct NetworkStack {
    device: VirtioNetDevice,
    interface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    dns: SocketHandle,
    udp_handles: Vec<SocketHandle>,
    udp_owners: [UdpOwner; MAX_UDP_SOCKETS],
    next_udp_generation: u32,
    driver_claim: crate::hal::driver::DriverClaim,
    address: Option<Ipv4Cidr>,
    router: Option<Ipv4Address>,
    dns_server: Option<Ipv4Address>,
}

#[derive(Clone, Copy)]
struct UdpOwner {
    pid: u32,
    generation: u32,
    port: u16,
}

impl UdpOwner {
    const FREE: Self = Self {
        pid: 0,
        generation: 0,
        port: 0,
    };
}

struct StackLease;

impl Drop for StackLease {
    fn drop(&mut self) {
        STACK_STATE.store(SLOT_READY, Ordering::Release);
    }
}

fn with_stack<R>(operation: impl FnOnce(&mut NetworkStack) -> R) -> Result<R, &'static str> {
    STACK_STATE
        .compare_exchange(SLOT_READY, SLOT_BUSY, Ordering::Acquire, Ordering::Relaxed)
        .map_err(|state| {
            if state == SLOT_BUSY {
                "network busy"
            } else {
                "no supported network device"
            }
        })?;
    let _lease = StackLease;
    // Safety: the successful state transition above gives this call unique
    // access until `_lease` publishes READY again.
    let stack = unsafe { (&mut *STACK.0.get()).assume_init_mut() };
    Ok(operation(stack))
}

fn now() -> Instant {
    Instant::from_millis(crate::interrupts::ticks().saturating_mul(10) as i64)
}

/// Initialize loopback and bind the first supported VirtIO network function.
/// All allocation happens before the stack is published; no lock is held.
pub fn init() {
    {
        let mut loopback = LOOPBACK.lock();
        loopback.init();
        let _ = (loopback.name(), loopback.is_up());
    }
    INITIALIZED.store(true, Ordering::Release);

    let Some(pci) = crate::hal::pci::discover().find(virtio::PCI_VENDOR, virtio::LEGACY_NET_DEVICE)
    else {
        crate::serial_println!(
            "virtio-net: no supported legacy PCI device; hardware network offline"
        );
        return;
    };
    let driver = match VirtioNet::bind(pci) {
        Ok(driver) => driver,
        Err(reason) => {
            crate::serial_println!("virtio-net: bind failed cleanly: {}", reason);
            return;
        }
    };
    let ownership = driver.ownership();
    let driver_claim = match crate::hal::driver::claim(ownership) {
        Ok(claim) => claim,
        Err(reason) => {
            let mut driver = driver;
            driver.shutdown();
            crate::serial_println!("virtio-net: ownership claim failed cleanly: {}", reason);
            return;
        }
    };
    let mut device = VirtioNetDevice::new(driver);
    let mac = device.mac();
    let mut config = Config::new(EthernetAddress(mac).into());
    config.random_seed = u64::from_le_bytes([
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        pci.address.device,
        pci.address.function,
    ]);
    let interface = Interface::new(config, &mut device, now());

    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());
    // One owned query slot is recycled after every result, bounding DNS
    // state independently of hostile or repeated requests.
    let mut query_slots = Vec::with_capacity(1);
    query_slots.push(None);
    let dns = sockets.add(dns::Socket::new(&[], query_slots));
    let mut udp_handles = Vec::with_capacity(MAX_UDP_SOCKETS);
    for _ in 0..MAX_UDP_SOCKETS {
        let receive = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_PACKET_SLOTS],
            vec![0; MAX_UDP_PAYLOAD * 2],
        );
        let transmit = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_PACKET_SLOTS],
            vec![0; MAX_UDP_PAYLOAD * 2],
        );
        udp_handles.push(sockets.add(udp::Socket::new(receive, transmit)));
    }
    let stack = NetworkStack {
        device,
        interface,
        sockets,
        dhcp,
        dns,
        udp_handles,
        udp_owners: [UdpOwner::FREE; MAX_UDP_SOCKETS],
        next_udp_generation: 1,
        driver_claim,
        address: None,
        router: None,
        dns_server: None,
    };
    // Safety: init is called once during single-threaded boot and the slot is
    // still UNAVAILABLE.  Publication follows the complete write.
    unsafe { (*STACK.0.get()).write(stack) };
    HARDWARE_PRESENT.store(true, Ordering::Release);
    STACK_STATE.store(SLOT_READY, Ordering::Release);
    crate::serial_println!(
        "net: VirtIO Ethernet link={} IPv4=unconfigured DHCP=ready DNS=ready",
        if with_stack(|stack| stack.device.link_up()).unwrap_or(false) {
            "up"
        } else {
            "down"
        }
    );
}

fn udp_slot(stack: &NetworkStack, pid: u32, public_handle: u32) -> Result<usize, &'static str> {
    let encoded_slot = (public_handle & 0xFF) as usize;
    if encoded_slot == 0 || encoded_slot > MAX_UDP_SOCKETS {
        return Err("invalid UDP handle");
    }
    let slot = encoded_slot - 1;
    let generation = public_handle >> 8;
    let owner = stack.udp_owners[slot];
    if owner.pid != pid || owner.generation != generation || owner.port == 0 {
        return Err("UDP handle is stale or belongs to another process");
    }
    Ok(slot)
}

/// Open one owner-bound UDP handle. Ports below 1024 are reserved for future
/// capability-controlled services and cannot be claimed by arbitrary Ring 3
/// processes.
pub fn udp_open(pid: u32, port: u16) -> Result<u32, &'static str> {
    if !(1024..49152).contains(&port) {
        return Err("UDP port is reserved");
    }
    with_stack(|stack| {
        if stack.udp_owners.iter().any(|owner| owner.port == port) {
            return Err("UDP port is already bound");
        }
        let slot = stack
            .udp_owners
            .iter()
            .position(|owner| owner.pid == 0)
            .ok_or("UDP socket table is full")?;
        let generation = (stack.next_udp_generation & 0x00FF_FFFF).max(1);
        stack.next_udp_generation = ((generation + 1) & 0x00FF_FFFF).max(1);
        stack
            .sockets
            .get_mut::<udp::Socket>(stack.udp_handles[slot])
            .bind(port)
            .map_err(|_| "UDP bind failed")?;
        stack.udp_owners[slot] = UdpOwner {
            pid,
            generation,
            port,
        };
        Ok((generation << 8) | (slot as u32 + 1))
    })?
}

pub fn udp_send(
    pid: u32,
    public_handle: u32,
    destination: Ipv4Address,
    port: u16,
    payload: &[u8],
) -> Result<usize, &'static str> {
    if payload.is_empty() || payload.len() > MAX_UDP_PAYLOAD || port == 0 {
        return Err("invalid UDP destination or payload length");
    }
    with_stack(|stack| {
        if stack.address.is_none() {
            return Err("IPv4 is not configured");
        }
        let slot = udp_slot(stack, pid, public_handle)?;
        stack
            .sockets
            .get_mut::<udp::Socket>(stack.udp_handles[slot])
            .send_slice(payload, IpEndpoint::new(IpAddress::Ipv4(destination), port))
            .map_err(|_| "UDP transmit buffer is full or destination is invalid")?;
        poll_stack(stack);
        Ok(payload.len())
    })?
}

pub struct UdpDatagram {
    pub source: Ipv4Address,
    pub source_port: u16,
    pub length: usize,
    pub payload: [u8; MAX_UDP_PAYLOAD],
}

pub fn udp_receive(pid: u32, public_handle: u32) -> Result<Option<UdpDatagram>, &'static str> {
    with_stack(|stack| {
        let slot = udp_slot(stack, pid, public_handle)?;
        poll_stack(stack);
        let socket = stack
            .sockets
            .get_mut::<udp::Socket>(stack.udp_handles[slot]);
        if !socket.can_recv() {
            return Ok(None);
        }
        let mut datagram = UdpDatagram {
            source: Ipv4Address::UNSPECIFIED,
            source_port: 0,
            length: 0,
            payload: [0; MAX_UDP_PAYLOAD],
        };
        let (length, metadata) = socket
            .recv_slice(&mut datagram.payload)
            .map_err(|_| "UDP receive buffer was truncated")?;
        let IpAddress::Ipv4(source) = metadata.endpoint.addr;
        datagram.source = source;
        datagram.source_port = metadata.endpoint.port;
        datagram.length = length;
        Ok(Some(datagram))
    })?
}

pub fn udp_close(pid: u32, public_handle: u32) -> Result<(), &'static str> {
    with_stack(|stack| {
        let slot = udp_slot(stack, pid, public_handle)?;
        stack
            .sockets
            .get_mut::<udp::Socket>(stack.udp_handles[slot])
            .close();
        stack.udp_owners[slot] = UdpOwner::FREE;
        Ok(())
    })?
}

/// Release every socket owned by a process before its TCB/address space is
/// reclaimed. This is idempotent and does no allocation or device I/O.
pub fn close_process_sockets(pid: u32) {
    let _ = with_stack(|stack| {
        for slot in 0..MAX_UDP_SOCKETS {
            if stack.udp_owners[slot].pid == pid {
                stack
                    .sockets
                    .get_mut::<udp::Socket>(stack.udp_handles[slot])
                    .close();
                stack.udp_owners[slot] = UdpOwner::FREE;
            }
        }
    });
}

fn poll_stack(stack: &mut NetworkStack) {
    let timestamp = now();
    let NetworkStack {
        device,
        interface,
        sockets,
        ..
    } = stack;
    let _ = interface.poll(timestamp, device, sockets);
}

fn poll_dhcp_step() -> Result<Option<NetworkSnapshot>, &'static str> {
    with_stack(|stack| {
        poll_stack(stack);
        let event = stack.sockets.get_mut::<dhcpv4::Socket>(stack.dhcp).poll();
        match event {
            None => Ok(None),
            Some(dhcpv4::Event::Deconfigured) => {
                stack.address = None;
                stack.router = None;
                stack.dns_server = None;
                stack
                    .interface
                    .update_ip_addrs(|addresses| addresses.clear());
                stack.interface.routes_mut().remove_default_ipv4_route();
                stack
                    .sockets
                    .get_mut::<dns::Socket>(stack.dns)
                    .update_servers(&[]);
                Ok(None)
            }
            Some(dhcpv4::Event::Configured(config)) => {
                let address = config.address;
                let router = config.router;
                let dns_server = config.dns_servers.first().copied();
                stack.interface.update_ip_addrs(|addresses| {
                    addresses.clear();
                    let _ = addresses.push(IpCidr::Ipv4(address));
                });
                stack.interface.routes_mut().remove_default_ipv4_route();
                if let Some(router) = router {
                    stack
                        .interface
                        .routes_mut()
                        .add_default_ipv4_route(router)
                        .map_err(|_| "route table is full")?;
                }
                if let Some(server) = dns_server {
                    stack
                        .sockets
                        .get_mut::<dns::Socket>(stack.dns)
                        .update_servers(&[IpAddress::Ipv4(server)]);
                }
                stack.address = Some(address);
                stack.router = router;
                stack.dns_server = dns_server;
                Ok(Some(stack.snapshot()))
            }
        }
    })?
}

/// Acquire DHCP configuration with a finite 15-second budget.  The lease is
/// released before every scheduler sleep, so no task sleeps while owning the
/// network stack.
pub fn configure_dhcp() -> Result<NetworkSnapshot, &'static str> {
    let deadline = crate::interrupts::ticks().saturating_add(DHCP_TIMEOUT_TICKS);
    loop {
        if let Some(snapshot) = poll_dhcp_step()? {
            if let Some(address) = snapshot.address {
                crate::serial_println!("net: DHCP lease address={}", address);
            }
            if let Some(router) = snapshot.router {
                crate::serial_println!("net: DHCP router={}", router);
            }
            if let Some(server) = snapshot.dns_server {
                crate::serial_println!("net: DHCP DNS={}", server);
            } else {
                crate::serial_println!("net: DHCP supplied no DNS server");
            }
            return Ok(snapshot);
        }
        if crate::interrupts::ticks() >= deadline {
            return Err("DHCP timed out");
        }
        crate::task::sleep_ticks(1);
    }
}

/// Resolve one A record through the DHCP-provided DNS server.  Only one query
/// may be outstanding and both name length and wait time are bounded by the
/// protocol library and this wrapper.
pub fn resolve_ipv4(name: &str) -> Result<Ipv4Address, &'static str> {
    if name.is_empty() || name.len() > 253 {
        return Err("invalid DNS name length");
    }
    let query = with_stack(|stack| {
        if stack.address.is_none() || stack.dns_server.is_none() {
            return Err("DHCP configuration with DNS is required");
        }
        let NetworkStack {
            interface,
            sockets,
            dns,
            ..
        } = stack;
        sockets
            .get_mut::<dns::Socket>(*dns)
            .start_query(interface.context(), name, DnsQueryType::A)
            .map_err(|_| "DNS query could not be started")
    })??;
    let deadline = crate::interrupts::ticks().saturating_add(DNS_TIMEOUT_TICKS);
    loop {
        let result = with_stack(|stack| {
            poll_stack(stack);
            stack
                .sockets
                .get_mut::<dns::Socket>(stack.dns)
                .get_query_result(query)
        })?;
        match result {
            Ok(addresses) => {
                return addresses
                    .iter()
                    .find_map(|address| match address {
                        IpAddress::Ipv4(address) => Some(*address),
                    })
                    .ok_or("DNS response contained no IPv4 address");
            }
            Err(dns::GetQueryResultError::Pending) => {}
            Err(dns::GetQueryResultError::Failed) => return Err("DNS query failed"),
        }
        if crate::interrupts::ticks() >= deadline {
            with_stack(|stack| {
                stack
                    .sockets
                    .get_mut::<dns::Socket>(stack.dns)
                    .cancel_query(query)
            })?;
            return Err("DNS query timed out");
        }
        crate::task::sleep_ticks(1);
    }
}

#[derive(Clone, Copy)]
pub struct NetworkSnapshot {
    pub link_up: bool,
    pub mac: [u8; 6],
    pub address: Option<Ipv4Cidr>,
    pub router: Option<Ipv4Address>,
    pub dns_server: Option<Ipv4Address>,
}

impl NetworkStack {
    fn snapshot(&self) -> NetworkSnapshot {
        NetworkSnapshot {
            link_up: self.device.link_up(),
            mac: self.device.mac(),
            address: self.address,
            router: self.router,
            dns_server: self.dns_server,
        }
    }
}

pub fn snapshot() -> Result<NetworkSnapshot, &'static str> {
    with_stack(|stack| stack.snapshot())
}

/// High-level status for the shell and System Monitor.
pub fn status_lines() -> Vec<String> {
    let mut lines = Vec::new();
    if !INITIALIZED.load(Ordering::Acquire) {
        lines.push(String::from("Network: not initialized"));
        return lines;
    }
    lines.push(String::from("Loopback: active"));
    if !HARDWARE_PRESENT.load(Ordering::Acquire) {
        lines.push(String::from("Hardware: no supported VirtIO network device"));
        return lines;
    }
    match snapshot() {
        Ok(status) => {
            lines.push(format!(
                "VirtIO Ethernet: {} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                if status.link_up {
                    "link up"
                } else {
                    "link down"
                },
                status.mac[0],
                status.mac[1],
                status.mac[2],
                status.mac[3],
                status.mac[4],
                status.mac[5]
            ));
            lines.push(match status.address {
                Some(address) => format!("IPv4: {}", address),
                None => String::from("IPv4: unconfigured (run: net dhcp)"),
            });
            if let Some(router) = status.router {
                lines.push(format!("Gateway: {}", router));
            }
            if let Some(server) = status.dns_server {
                lines.push(format!("DNS: {}", server));
            }
        }
        Err(reason) => lines.push(format!("Hardware: {}", reason)),
    }
    lines
}

/// Send a loopback diagnostic packet without touching hardware I/O.
pub fn ping(host: &str) -> Result<String, &'static str> {
    let host = host.trim();
    if host != "localhost" && host != "127.0.0.1" {
        return Err("ICMP echo is not exposed yet; use net dns for real traffic");
    }
    let mut driver = LOOPBACK.lock();
    let payload = b"tuwaiqos-ping";
    match driver.send(payload) {
        driver::SendResult::Delivered => {}
        driver::SendResult::Dropped => return Err("loopback dropped packet"),
    }
    let mut buffer = [0u8; 64];
    let received = driver.receive(&mut buffer).ok_or("no loopback reply")?;
    if received != payload.len() || &buffer[..received] != payload {
        return Err("unexpected loopback reply");
    }
    Ok(String::from("Reply from localhost: ok"))
}

pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

/// Reset hardware and scrub DMA storage.  Used by failure-path verification;
/// normal operation keeps the boot-owned device bound for kernel lifetime.
pub fn shutdown_hardware() -> Result<(), &'static str> {
    STACK_STATE
        .compare_exchange(SLOT_READY, SLOT_BUSY, Ordering::Acquire, Ordering::Relaxed)
        .map_err(|_| "network busy or unavailable")?;
    // Safety: the terminal READY->BUSY transition grants unique access and
    // this function never republishes READY.
    let stack = unsafe { (&mut *STACK.0.get()).assume_init_mut() };
    for slot in 0..MAX_UDP_SOCKETS {
        stack
            .sockets
            .get_mut::<udp::Socket>(stack.udp_handles[slot])
            .close();
        stack.udp_owners[slot] = UdpOwner::FREE;
    }
    stack.device.shutdown();
    stack.driver_claim.release();
    STACK_STATE.store(SLOT_UNAVAILABLE, Ordering::Release);
    HARDWARE_PRESENT.store(false, Ordering::Release);
    if crate::hal::driver::active_claims() == 0 {
        Ok(())
    } else {
        Err("driver ownership claim survived teardown")
    }
}
