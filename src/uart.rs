//! USB/UART serial output.

//! USB Serial/JTAG output for ESP32-P4 ECO2.

const USB_SERIAL_JTAG_BASE: usize = 0x500D_2000;
const EP1_FIFO: *mut u32 = USB_SERIAL_JTAG_BASE as *mut u32;
const EP1_CONF: *mut u32 = (USB_SERIAL_JTAG_BASE + 0x04) as *mut u32;
const TX_FIFO_FREE: u32 = 1 << 1;
const WR_DONE: u32 = 1;

/// Sends the hello-world line over the Tab5's native USB serial port.
pub fn hello_world() {
    write(b"Hello, world! (UART)\r\n");
}

/// Writes a diagnostic line from another hardware module.
pub fn log(bytes: &[u8]) {
    write(bytes);
}

/// Writes a memory-mapped register value without requiring formatting support.
pub fn log_hex(label: &[u8], value: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = [0u8; 11];
    output[0] = b'0';
    output[1] = b'x';
    for index in 0..8 {
        output[index + 2] = HEX[((value >> (28 - index * 4)) & 0xF) as usize];
    }
    output[10] = b'\n';
    write(label);
    write(&output);
}

fn write(bytes: &[u8]) {
    for &byte in bytes {
        // Do not let an unplugged host stall the firmware forever.
        let mut timeout = 1_000_000;
        while unsafe { EP1_CONF.read_volatile() } & TX_FIFO_FREE == 0 {
            if timeout == 0 {
                return;
            }
            timeout -= 1;
        }

        unsafe { EP1_FIFO.write_volatile(byte as u32) };
    }

    // Commit the short USB CDC packet to the host.
    unsafe { EP1_CONF.write_volatile(WR_DONE) };
}
