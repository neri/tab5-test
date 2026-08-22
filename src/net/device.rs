//! smoltcp's `phy::Device` over the ESP-Hosted station interface.
//!
//! ESP-Hosted's `IF_STA` payload is an 802.3 ethernet frame: the host side
//! of ESP-Hosted transmits through `esp_netif`'s transmit callback, which a
//! Wi-Fi station netif feeds with 14-byte-header ethernet frames, and the
//! C6's Wi-Fi driver does the 802.11 conversion. So the medium here is
//! `Medium::Ethernet` and the MTU is the usual 1500 -- a 1,514-byte frame
//! fits an ESP-Hosted payload (1,524 bytes) whole.
//!
//! The borrow is arranged so that RPC calls and packet I/O never need the
//! `Rpc` at the same time: `Rpc` owns the transport permanently, and a
//! `StationDevice` is built around a `&mut Rpc` for the duration of one
//! `Interface::poll` and dropped again. The receive token owns its frame
//! rather than borrowing, which is what lets `receive` hand out a transmit
//! token in the same breath, as the trait requires.

use alloc::vec::Vec;

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

use crate::uart;
use crate::wifi::Rpc;

/// Largest frame this link carries, **ethernet header included** -- which
/// is what smoltcp's `max_transmission_unit` means for `Medium::Ethernet`
/// (it subtracts the 14-byte header itself to get the IP MTU). Writing the
/// familiar 1500 here would quietly give every connection an IP MTU of
/// 1486. ESP-Hosted's payload limit is 1,524 bytes, so a full 1,514-byte
/// frame fits whole.
pub const MAX_FRAME_BYTES: usize = 1514;

/// The station interface as smoltcp sees it.
pub struct StationDevice<'a> {
    rpc: &'a mut Rpc,
}

impl<'a> StationDevice<'a> {
    pub fn new(rpc: &'a mut Rpc) -> Self {
        StationDevice { rpc }
    }
}

impl Device for StationDevice<'_> {
    type RxToken<'token>
        = StationRxToken
    where
        Self: 'token;
    type TxToken<'token>
        = StationTxToken<'token>
    where
        Self: 'token;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.rpc.take_station_frame()?;
        Some((StationRxToken { frame }, StationTxToken { rpc: self.rpc }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(StationTxToken { rpc: self.rpc })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = MAX_FRAME_BYTES;
        // Left unset on purpose. smoltcp turns `max_burst_size` into a TCP
        // window of that many segments, so declaring the one-frame-at-a-time
        // shape of the SDIO transport here would cap every connection at a
        // single MSS in flight.
        capabilities
    }
}

/// One received frame, already copied out of the transport's staging
/// buffer by `Rpc`.
pub struct StationRxToken {
    frame: Vec<u8>,
}

impl RxToken for StationRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame)
    }
}

/// Permission to send one frame. smoltcp builds the frame in place, so the
/// buffer has to exist before its contents are known.
pub struct StationTxToken<'a> {
    rpc: &'a mut Rpc,
}

impl TxToken for StationTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = alloc::vec![0u8; len];
        let result = f(&mut frame);
        if !self.rpc.send_station_frame(&frame) {
            // Dropping is the honest outcome: everything above this is a
            // protocol that retransmits, and blocking here would stall the
            // frame loop on a co-processor that has asked for quiet.
            uart::log(b"NET: dropped an outgoing frame\r\n");
        }
        result
    }
}
