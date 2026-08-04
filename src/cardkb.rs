//! CardKB v1.1 reader for the Tab5 PORT.A connector.
//!
//! The connector exposes GPIO53 as SDA and GPIO54 as SCL.  CardKB returns one
//! key byte directly when addressed for reading at I2C address 0x5F.

const GPIO: usize = 0x500E_0000;
const IO_MUX: usize = 0x500E_1000;

const SDA: u32 = 53;
const SCL: u32 = 54;
const CARDKB_ADDRESS: u8 = 0x5F;

pub struct CardKb;

impl CardKb {
    /// Configures PORT.A for open-drain, software-I2C operation.
    ///
    /// Initialization succeeds when both bus lines can be released high.  A
    /// disconnected keyboard is therefore harmless: `poll` simply returns no
    /// byte until a device acknowledges its address.
    pub fn init() -> Option<Self> {
        configure_i2c_gpio(SDA);
        configure_i2c_gpio(SCL);
        gpio_release(SDA);
        gpio_release(SCL);
        delay_us(20);

        // Recover a bus left in the middle of a transaction before starting
        // normal polling.  This also makes hot-plugging the unit predictable.
        for _ in 0..9 {
            gpio_low(SCL);
            delay_us(5);
            gpio_release(SCL);
            if !wait_scl_high() {
                return None;
            }
            delay_us(5);
        }
        i2c_stop();
        if gpio_level(SDA) && gpio_level(SCL) {
            Some(Self)
        } else {
            None
        }
    }

    /// Reads the current key value.  `None` means no key, no attached unit,
    /// or a transient bus failure; CardKB itself returns zero when idle.
    pub fn poll(&mut self) -> Option<u8> {
        if !i2c_start() || !i2c_write_byte((CARDKB_ADDRESS << 1) | 1) {
            i2c_stop();
            return None;
        }
        let Some(byte) = i2c_read_byte(false) else {
            i2c_stop();
            return None;
        };
        i2c_stop();
        (byte != 0).then_some(byte)
    }
}

fn configure_i2c_gpio(pin: u32) {
    // The GPIO matrix output register layout is GPIO_FUNC0_OUT_SEL_CFG plus
    // four bytes per GPIO.  Select the GPIO output source and control output
    // enable separately, which lets a released line behave as open drain.
    let matrix_out = GPIO + 0x558 + pin as usize * 4;
    let iomux = IO_MUX + 0x04 + pin as usize * 4;
    write(matrix_out, 256 | (1 << 10));
    modify(
        iomux,
        (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
        (1 << 8) | (1 << 9) | (2 << 10) | (1 << 12),
    );
    gpio_low(pin);
    gpio_release(pin);
}

fn i2c_start() -> bool {
    gpio_release(SDA);
    gpio_release(SCL);
    delay_us(5);
    if !gpio_level(SDA) || !wait_scl_high() {
        return false;
    }
    gpio_low(SDA);
    delay_us(5);
    gpio_low(SCL);
    true
}

fn i2c_stop() {
    gpio_low(SDA);
    delay_us(5);
    let _ = wait_scl_high();
    delay_us(5);
    gpio_release(SDA);
    delay_us(5);
}

fn i2c_write_byte(byte: u8) -> bool {
    for bit in (0..8).rev() {
        gpio_low(SCL);
        if byte & (1 << bit) == 0 {
            gpio_low(SDA);
        } else {
            gpio_release(SDA);
        }
        delay_us(5);
        if !wait_scl_high() {
            return false;
        }
        delay_us(5);
    }

    gpio_low(SCL);
    gpio_release(SDA);
    delay_us(5);
    if !wait_scl_high() {
        return false;
    }
    let acknowledged = !gpio_level(SDA);
    delay_us(5);
    gpio_low(SCL);
    acknowledged
}

fn i2c_read_byte(acknowledge: bool) -> Option<u8> {
    let mut byte = 0;
    gpio_release(SDA);
    for _ in 0..8 {
        gpio_low(SCL);
        delay_us(5);
        if !wait_scl_high() {
            return None;
        }
        byte = (byte << 1) | gpio_level(SDA) as u8;
        delay_us(5);
    }

    gpio_low(SCL);
    if acknowledge {
        gpio_low(SDA);
    } else {
        gpio_release(SDA); // NACK the final byte of this one-byte read.
    }
    delay_us(5);
    if !wait_scl_high() {
        return None;
    }
    delay_us(5);
    gpio_low(SCL);
    gpio_release(SDA);
    Some(byte)
}

fn wait_scl_high() -> bool {
    gpio_release(SCL);
    for _ in 0..40_000 {
        if gpio_level(SCL) {
            return true;
        }
    }
    false
}

fn gpio_low(pin: u32) {
    let bit = 1u32 << (pin - 32);
    write(GPIO + 0x18, bit); // GPIO_OUT1_W1TC
    write(GPIO + 0x30, bit); // GPIO_ENABLE1_W1TS
}

fn gpio_release(pin: u32) {
    write(GPIO + 0x34, 1u32 << (pin - 32)); // GPIO_ENABLE1_W1TC
}

fn gpio_level(pin: u32) -> bool {
    read(GPIO + 0x40) & (1u32 << (pin - 32)) != 0 // GPIO_IN1
}

fn delay_us(microseconds: u32) {
    const CPU_CYCLES_PER_US: u32 = 400;
    let start = cycle_count();
    while cycle_count().wrapping_sub(start) < microseconds.saturating_mul(CPU_CYCLES_PER_US) {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn cycle_count() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdcycle {value}", value = out(reg) value, options(nomem, nostack));
    }
    value
}

fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

fn modify(address: usize, mask: u32, value: u32) {
    write(address, (read(address) & !mask) | (value & mask));
}
