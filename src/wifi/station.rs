//! Station-mode operations built on the RPC layer (stage 4 of
//! `docs/WIFI_C6_PLAN.md`): bring the slave's Wi-Fi up, scan, and read the
//! results back.
//!
//! Each function here is one or a few `esp_wifi_*` calls executed on the C6.
//! They return the slave's own `esp_err_t` rather than collapsing it into a
//! boolean, because "the RPC worked and the slave said no" and "the link
//! broke" are different problems: the first is reported as `Some(status)`
//! with a nonzero status, the second as `None`.

use alloc::vec::Vec;

use crate::uart;
use crate::wifi::proto::{Reader, Writer};
use crate::wifi::rpc::{Rpc, Status};

/// Request ids (`RpcId` in `esp_hosted_rpc.proto`).
const REQ_SET_WIFI_MODE: u32 = 260;
const REQ_SET_POWER_SAVE: u32 = 270;
const REQ_WIFI_INIT: u32 = 278;
const REQ_WIFI_START: u32 = 280;
const REQ_WIFI_SCAN_START: u32 = 286;
const REQ_WIFI_SCAN_GET_AP_NUM: u32 = 288;
const REQ_WIFI_SCAN_GET_AP_RECORDS: u32 = 289;

/// `wifi_mode_t`.
const WIFI_MODE_STA: i32 = 1;

/// `wifi_ps_type_t`.
const WIFI_PS_NONE: i32 = 0;

/// SSIDs are at most 32 bytes and are not required to be null-terminated.
pub const SSID_MAX_BYTES: usize = 32;

/// How many access points to ask the slave for. The slave allocates an
/// array of this many records before filling it in, so this is a request
/// for "up to", not a promise.
const MAX_ACCESS_POINTS: i32 = 32;

/// One entry from a scan.
pub struct AccessPoint {
    pub ssid: [u8; SSID_MAX_BYTES],
    pub ssid_length: usize,
    pub bssid: [u8; 6],
    pub channel: u32,
    pub rssi: i32,
    pub auth_mode: i32,
}

impl AccessPoint {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..self.ssid_length]
    }
}

/// `wifi_auth_mode_t` as a name. Unknown values keep their number, since
/// the enum grows with every IDF release and the slave may be newer than
/// this table.
pub fn auth_mode_name(mode: i32) -> Option<&'static str> {
    Some(match mode {
        0 => "OPEN",
        1 => "WEP",
        2 => "WPA_PSK",
        3 => "WPA2_PSK",
        4 => "WPA_WPA2_PSK",
        5 => "ENTERPRISE",
        6 => "WPA3_PSK",
        7 => "WPA2_WPA3_PSK",
        8 => "WAPI_PSK",
        9 => "OWE",
        10 => "WPA3_ENT_192",
        _ => return None,
    })
}

/// Initializes Wi-Fi on the slave and puts it in station mode.
///
/// The slave starts from its own `WIFI_INIT_CONFIG_DEFAULT()` and then
/// overwrites nearly every field with what arrives here -- including
/// `magic`, which `esp_wifi_init` validates -- so this has to send a
/// complete and self-consistent configuration rather than zeros.
pub fn start(rpc: &mut Rpc) -> Option<Status> {
    let status = init(rpc)?;
    if status != 0 {
        return Some(status);
    }

    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, WIFI_MODE_STA);
    let length = writer.finish()?;
    let status = simple_status(rpc, REQ_SET_WIFI_MODE, &body[..length])?;
    if status != 0 {
        return Some(status);
    }

    let status = simple_status(rpc, REQ_WIFI_START, &[])?;
    if status != 0 {
        return Some(status);
    }

    // A station defaults to `WIFI_PS_MIN_MODEM`, which lets the radio doze
    // between beacons once it is associated. Nothing here manages power, and
    // a co-processor that sleeps is a co-processor that stops answering on
    // the bus, so ask for no power saving at all.
    uart::log(b"WIFI: disabling power save\r\n");
    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, WIFI_PS_NONE);
    let length = writer.finish()?;
    simple_status(rpc, REQ_SET_POWER_SAVE, &body[..length])
}

/// Sends `esp_wifi_init` with ESP-IDF's default configuration.
fn init(rpc: &mut Rpc) -> Option<Status> {
    // `WIFI_INIT_CONFIG_DEFAULT()` from ESP-IDF v5.5.3's `esp_wifi.h`, with
    // the Kconfig-derived numbers at their defaults. `feature_caps` is left
    // at zero on purpose: when it differs from the slave's own build the
    // slave keeps its value, which is the safe direction.
    let mut config = [0u8; 96];
    let mut writer = Writer::new(&mut config);
    writer.int32_field(1, 10); // static_rx_buf_num
    writer.int32_field(2, 32); // dynamic_rx_buf_num
    writer.int32_field(3, 1); // tx_buf_type: dynamic
    writer.int32_field(4, 0); // static_tx_buf_num (unused when dynamic)
    writer.int32_field(5, 32); // dynamic_tx_buf_num
    writer.int32_field(6, 0); // cache_tx_buf_num
    writer.int32_field(7, 0); // csi_enable
    writer.int32_field(8, 1); // ampdu_rx_enable
    writer.int32_field(9, 1); // ampdu_tx_enable
    writer.int32_field(10, 0); // amsdu_tx_enable
    writer.int32_field(11, 1); // nvs_enable
    writer.int32_field(12, 0); // nano_enable
    writer.int32_field(13, 6); // rx_ba_win
    writer.int32_field(14, 0); // wifi_task_core_id
    writer.int32_field(15, 752); // beacon_max_len
    writer.int32_field(16, 32); // mgmt_sbuf_num
    writer.uint32_field(17, 0); // feature_caps
    writer.bool_field(18, true); // sta_disconnected_pm
    writer.int32_field(19, 7); // espnow_max_encrypt_num
    writer.int32_field(20, 0x1F2F3F4F); // magic
    writer.int32_field(21, 0); // rx_mgmt_buf_type
    writer.int32_field(22, 5); // rx_mgmt_buf_num
    let config_length = writer.finish()?;

    let mut body = [0u8; 128];
    let mut writer = Writer::new(&mut body);
    writer.bytes_field(1, &config[..config_length]);
    let length = writer.finish()?;

    simple_status(rpc, REQ_WIFI_INIT, &body[..length])
}

/// Runs one scan and returns what the slave found.
///
/// The scan is asked for in blocking mode, so the response does not come
/// back until the radio has been round every channel -- a few seconds. That
/// keeps the caller a straight line instead of a state machine waiting for
/// the scan-done event.
pub fn scan(rpc: &mut Rpc) -> Option<(Status, Vec<AccessPoint>)> {
    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.bool_field(2, true); // block
    writer.int32_field(3, 0); // config_set: no scan config, use the defaults
    let length = writer.finish()?;

    let status = simple_status(rpc, REQ_WIFI_SCAN_START, &body[..length])?;
    if status != 0 {
        return Some((status, Vec::new()));
    }

    let (status, found) = access_point_count(rpc)?;
    if status != 0 || found == 0 {
        return Some((status, Vec::new()));
    }

    access_point_records(rpc, found.min(MAX_ACCESS_POINTS))
}

/// `esp_wifi_scan_get_ap_num`.
fn access_point_count(rpc: &mut Rpc) -> Option<(Status, i32)> {
    let payload = rpc.call(REQ_WIFI_SCAN_GET_AP_NUM, &[])?;

    let mut status = 0;
    let mut number = 0;
    let mut reader = Reader::new(&payload);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => status = value.as_i32(),
            2 => number = value.as_i32(),
            _ => {}
        }
    }
    Some((status, number))
}

/// `esp_wifi_scan_get_ap_records`. The records come back as a repeated
/// field, i.e. the same field number once per access point.
fn access_point_records(rpc: &mut Rpc, wanted: i32) -> Option<(Status, Vec<AccessPoint>)> {
    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, wanted);
    let length = writer.finish()?;

    let payload = rpc.call(REQ_WIFI_SCAN_GET_AP_RECORDS, &body[..length])?;

    let mut status = 0;
    let mut records = Vec::new();
    let mut reader = Reader::new(&payload);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => status = value.as_i32(),
            // Field 2 repeats the count, which the records themselves give.
            3 => {
                if let Some(record) = parse_access_point(value.as_bytes()) {
                    records.push(record);
                }
            }
            _ => {}
        }
    }

    Some((status, records))
}

/// One `wifi_ap_record`. Only the fields this firmware displays are read;
/// country, HE and VHT details are skipped like any other unknown field.
fn parse_access_point(bytes: &[u8]) -> Option<AccessPoint> {
    let mut record = AccessPoint {
        ssid: [0; SSID_MAX_BYTES],
        ssid_length: 0,
        bssid: [0; 6],
        channel: 0,
        rssi: 0,
        auth_mode: 0,
    };

    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => {
                if let Some(address) = value.as_bytes().get(..6) {
                    record.bssid.copy_from_slice(address);
                }
            }
            2 => {
                let ssid = value.as_bytes();
                // The slave sends the whole 33-byte field including its
                // terminator; keep the bytes up to the first zero.
                let end = ssid
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(ssid.len())
                    .min(SSID_MAX_BYTES);
                record.ssid[..end].copy_from_slice(&ssid[..end]);
                record.ssid_length = end;
            }
            3 => record.channel = value.as_u32(),
            5 => record.rssi = value.as_i32(),
            6 => record.auth_mode = value.as_i32(),
            _ => {}
        }
    }

    Some(record)
}

/// Runs a request whose response is just a status code.
fn simple_status(rpc: &mut Rpc, request_id: u32, body: &[u8]) -> Option<Status> {
    let payload = rpc.call(request_id, body)?;

    let mut status = 0;
    let mut reader = Reader::new(&payload);
    while let Some((field, value)) = reader.next_field() {
        if field == 1 {
            status = value.as_i32();
        }
    }
    if status != 0 {
        uart::log_hex(b"WIFI: request id=", request_id);
        uart::log_hex(b"WIFI: slave returned status=", status as u32);
    }
    Some(status)
}

// ---------------------------------------------------------------------------
// Connecting (stage 5 of `docs/WIFI_C6_PLAN.md`)
// ---------------------------------------------------------------------------

const REQ_WIFI_CONNECT: u32 = 282;
const REQ_WIFI_DISCONNECT: u32 = 283;
const REQ_WIFI_SET_CONFIG: u32 = 284;
const REQ_WIFI_STA_GET_AP_INFO: u32 = 294;

/// Event ids the connect path waits on (`RpcId`, event range).
pub const EVENT_STA_CONNECTED: u32 = 775;
pub const EVENT_STA_DISCONNECTED: u32 = 776;

/// `wifi_interface_t`. Note this is not `wifi_mode_t`: the slave checks
/// `iface` against `WIFI_IF_STA`, which is zero.
const WIFI_IF_STA: i32 = 0;

/// SSIDs are 32 bytes and passwords 64, both without a terminator.
pub const PASSWORD_MAX_BYTES: usize = 64;

/// How the slave answered a connection attempt.
pub enum Outcome {
    Connected {
        ssid: [u8; SSID_MAX_BYTES],
        ssid_length: usize,
        bssid: [u8; 6],
        channel: u32,
        auth_mode: i32,
    },
    /// The slave gave up. `reason` is a `wifi_err_reason_t`.
    Disconnected { reason: u32 },
    /// Neither event arrived in time.
    TimedOut,
}

/// `wifi_err_reason_t` as a name, for the handful of reasons a connection
/// attempt from a shell command actually produces.
pub fn disconnect_reason_name(reason: u32) -> Option<&'static str> {
    Some(match reason {
        1 => "UNSPECIFIED",
        2 => "AUTH_EXPIRE",
        3 => "AUTH_LEAVE",
        4 => "DISASSOC_DUE_TO_INACTIVITY",
        5 => "ASSOC_TOOMANY",
        6 => "CLASS2_FRAME_FROM_NONAUTH_STA",
        7 => "CLASS3_FRAME_FROM_NONASSOC_STA",
        8 => "ASSOC_LEAVE",
        15 => "4WAY_HANDSHAKE_TIMEOUT (wrong password?)",
        200 => "BEACON_TIMEOUT",
        201 => "NO_AP_FOUND",
        202 => "AUTH_FAIL",
        203 => "ASSOC_FAIL",
        204 => "HANDSHAKE_TIMEOUT",
        205 => "CONNECTION_FAIL",
        210 => "NO_AP_FOUND_W_COMPATIBLE_SECURITY",
        211 => "NO_AP_FOUND_IN_AUTHMODE_THRESHOLD",
        212 => "NO_AP_FOUND_IN_RSSI_THRESHOLD",
        _ => return None,
    })
}

/// Sets the station configuration and asks the slave to connect.
///
/// A zero status only means the slave accepted the request: association
/// happens afterwards and is reported by an event, so callers follow this
/// with [`wait_for_connection`].
pub fn connect(rpc: &mut Rpc, ssid: &[u8], password: &[u8]) -> Option<Status> {
    // `wifi_pmf_config { capable = 1 }`. The field is deprecated in recent
    // IDF (a station always uses PMF when the AP offers it), but an older
    // slave may still read it, and refusing PMF would rule out WPA3 APs.
    let mut pmf = [0u8; 4];
    let mut writer = Writer::new(&mut pmf);
    writer.bool_field(1, true);
    let pmf_length = writer.finish()?;

    // `wifi_scan_threshold { rssi = 0, authmode = 0 }`: accept any signal
    // and let the password decide the minimum security.
    //
    // This has to be sent even though every field is zero. A nested message
    // that is absent decodes to a null pointer on the slave, and the
    // firmware shipped on this C6 reads `threshold->rssi` without checking
    // -- which reboots the co-processor mid-request. The reference host
    // allocates `threshold` and `pmf_cfg` unconditionally
    // (`rpc_req.c`, `RPC_ALLOC_ELEMENT`), so anything it always sends is
    // effectively required.
    let mut threshold = [0u8; 8];
    let mut writer = Writer::new(&mut threshold);
    writer.int32_field(1, 0);
    writer.int32_field(2, 0);
    let threshold_length = writer.finish()?;

    // `wifi_sta_config`. Everything not set here stays at the slave's zero
    // value, which is what ESP-IDF's own defaults amount to: fast scan,
    // sort by signal, and an auth threshold of "whatever the password
    // implies".
    let mut sta = [0u8; 192];
    let mut writer = Writer::new(&mut sta);
    writer.bytes_field(1, ssid);
    writer.bytes_field(2, password);
    writer.bytes_field(9, &threshold[..threshold_length]);
    writer.bytes_field(10, &pmf[..pmf_length]);
    let sta_length = writer.finish()?;

    // `wifi_config { sta = 2 }`.
    let mut config = [0u8; 224];
    let mut writer = Writer::new(&mut config);
    writer.bytes_field(2, &sta[..sta_length]);
    let config_length = writer.finish()?;

    // `Rpc_Req_WifiSetConfig { iface = 1, cfg = 2 }`.
    let mut body = [0u8; 256];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, WIFI_IF_STA);
    writer.bytes_field(2, &config[..config_length]);
    let length = writer.finish()?;

    // These two are logged because a link that dies mid-connect needs to be
    // pinned to one of them.
    uart::log(b"WIFI: sending the station configuration\r\n");
    let status = simple_status(rpc, REQ_WIFI_SET_CONFIG, &body[..length])?;
    if status != 0 {
        return Some(status);
    }

    uart::log(b"WIFI: sending connect\r\n");
    simple_status(rpc, REQ_WIFI_CONNECT, &[])
}

/// Waits for the slave to report the outcome of a connection attempt.
pub fn wait_for_connection(rpc: &mut Rpc, timeout_ms: u32) -> Outcome {
    let wanted = [EVENT_STA_CONNECTED, EVENT_STA_DISCONNECTED];
    let Some(event) = rpc.wait_for_event(timeout_ms, &wanted) else {
        return Outcome::TimedOut;
    };

    // Both events wrap the interesting part in field 2.
    let mut details: &[u8] = &[];
    let mut reader = Reader::new(&event.payload);
    while let Some((field, value)) = reader.next_field() {
        if field == 2 {
            details = value.as_bytes();
        }
    }

    if event.msg_id == EVENT_STA_DISCONNECTED {
        let mut reason = 0;
        let mut reader = Reader::new(details);
        while let Some((field, value)) = reader.next_field() {
            if field == 4 {
                reason = value.as_u32();
            }
        }
        return Outcome::Disconnected { reason };
    }

    let mut ssid = [0u8; SSID_MAX_BYTES];
    let mut ssid_length = 0;
    let mut bssid = [0u8; 6];
    let mut channel = 0;
    let mut auth_mode = 0;
    let mut reader = Reader::new(details);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => {
                let bytes = value.as_bytes();
                let end = bytes.len().min(SSID_MAX_BYTES);
                ssid[..end].copy_from_slice(&bytes[..end]);
                ssid_length = end;
            }
            2 => ssid_length = (value.as_u32() as usize).min(ssid_length),
            3 => {
                if let Some(address) = value.as_bytes().get(..6) {
                    bssid.copy_from_slice(address);
                }
            }
            4 => channel = value.as_u32(),
            5 => auth_mode = value.as_i32(),
            _ => {}
        }
    }

    Outcome::Connected {
        ssid,
        ssid_length,
        bssid,
        channel,
        auth_mode,
    }
}

/// `esp_wifi_sta_get_ap_info`: what the station is currently associated
/// with. A nonzero status means "not connected" as much as it means an
/// error.
pub fn connected_access_point(rpc: &mut Rpc) -> Option<(Status, Option<AccessPoint>)> {
    let payload = rpc.call(REQ_WIFI_STA_GET_AP_INFO, &[])?;

    let mut status = 0;
    let mut record = None;
    let mut reader = Reader::new(&payload);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => status = value.as_i32(),
            2 => record = parse_access_point(value.as_bytes()),
            _ => {}
        }
    }
    Some((status, record))
}

/// `esp_wifi_disconnect`.
pub fn disconnect(rpc: &mut Rpc) -> Option<Status> {
    simple_status(rpc, REQ_WIFI_DISCONNECT, &[])
}
