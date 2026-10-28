use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::{fs, path::Path};

const LED_PATHS: [&str; 9] = [
    "/sys/class/leds/micro:red:ring1/brightness",
    "/sys/class/leds/micro:green:ring1/brightness",
    "/sys/class/leds/micro:blue:ring1/brightness",
    "/sys/class/leds/micro:red:ring2/brightness",
    "/sys/class/leds/micro:green:ring2/brightness",
    "/sys/class/leds/micro:blue:ring2/brightness",
    "/sys/class/leds/micro:red:ring3/brightness",
    "/sys/class/leds/micro:green:ring3/brightness",
    "/sys/class/leds/micro:blue:ring3/brightness",
];

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    if let Some(path) = LED_PATHS.iter().find(|path| !Path::new(path).exists()) {
        bail!("Yandex Micro LED sysfs node is missing: {path}");
    }
    Ok(Box::new(Driver))
}

struct Driver;

impl LedBackend for Driver {
    fn show(&self, frame: &[u8]) -> Result<()> {
        if frame.len() != LED_PATHS.len() {
            bail!("unexpected Yandex Micro LED frame length");
        }
        for (path, value) in LED_PATHS.iter().zip(frame) {
            fs::write(path, value.to_string()).with_context(|| format!("write {path}"))?;
        }
        Ok(())
    }
}
