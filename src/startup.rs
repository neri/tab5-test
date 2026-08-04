//! ESP32-P4 ECO2 startup operations which must happen before application code.

/// Stops the RTC watchdog inherited from the ESP-IDF bootloader.
pub fn init() {
    const LP_WDT_BASE: usize = 0x5011_6000;
    const CONFIG0: *mut u32 = LP_WDT_BASE as *mut u32;
    const WPROTECT: *mut u32 = (LP_WDT_BASE + 0x18) as *mut u32;
    const WKEY: u32 = 0x50D8_3AA1;
    const WDT_EN: u32 = 1 << 31;

    // ESP-IDF v5.4: wdt_hal_write_protect_disable(); wdt_hal_disable();
    // wdt_hal_write_protect_enable().
    unsafe {
        WPROTECT.write_volatile(WKEY);
        CONFIG0.write_volatile(CONFIG0.read_volatile() & !WDT_EN);
        WPROTECT.write_volatile(0);
    }
}
