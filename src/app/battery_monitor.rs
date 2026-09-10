//! One INA226 owner shared by the system indicator and details.
use crate::ina226::{BatterySample, Ina226};

pub struct BatteryMonitor {
    device: Option<Ina226>,
    pub sample: Option<BatterySample>,
    pub error: Option<&'static str>,
    next_ms: u64,
}
impl BatteryMonitor {
    pub const fn new() -> Self {
        Self {
            device: None,
            sample: None,
            error: None,
            next_ms: 0,
        }
    }
    /// At most one initialization or measurement group per call. Failed
    /// readings become unknown immediately; probing retries at five seconds.
    pub fn poll(&mut self, now: u64) -> bool {
        if now < self.next_ms {
            return false;
        }
        self.next_ms = now.saturating_add(1000);
        if let Some(device) = &self.device {
            self.sample = device.read_sample();
            self.error = if self.sample.is_none() {
                Some("INA226 read failed")
            } else {
                None
            };
        } else {
            match Ina226::init() {
                Ok(device) => {
                    self.device = Some(device);
                    self.error = None;
                }
                Err(error) => {
                    self.error = Some(error.message());
                    self.next_ms = now.saturating_add(5000);
                }
            }
        }
        true
    }
}
