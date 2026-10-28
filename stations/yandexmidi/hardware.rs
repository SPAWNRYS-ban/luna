use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::fs;

const RING_COUNT: usize = 24;
const LOGO_COUNT: usize = 6;

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    Ok(Box::new(SysfsRing))
}

struct SysfsRing;

impl LedBackend for SysfsRing {
    fn show(&self, frame: &[u8]) -> Result<()> {
        self.write_zone(frame, "ring", RING_COUNT, "ring")
    }

    fn show_logo(&self, frame: &[u8]) -> Result<()> {
        self.write_zone(frame, "logo", LOGO_COUNT, "logo")
    }

    fn restore_ring_trigger(&self) -> Result<()> {
        self.restore_trigger("ring", RING_COUNT, "ledfb-ring")
    }

    fn restore_logo_trigger(&self) -> Result<()> {
        self.restore_trigger("logo", LOGO_COUNT, "ledfb-logo")
    }
}

impl SysfsRing {
    fn write_zone(&self, frame: &[u8], node: &str, count: usize, label: &str) -> Result<()> {
        if frame.len() != count * 3 {
            bail!("unexpected Yandex Midi {label} LED frame length");
        }
        for index in 0..count {
            for (channel, value) in ["red", "green", "blue"]
                .iter()
                .zip(&frame[index * 3..index * 3 + 3])
            {
                let path = format!("/sys/class/leds/cucumber:{channel}:{node}{index}/brightness");
                fs::write(&path, value.to_string()).with_context(|| format!("write {path}"))?;
            }
        }
        Ok(())
    }

    fn restore_trigger(&self, node: &str, count: usize, trigger: &str) -> Result<()> {
        for index in 0..count {
            let path = format!("/sys/class/leds/cucumber:red:{node}{index}/trigger");
            fs::write(&path, trigger).with_context(|| format!("restore {path}"))?;
            for channel in ["green", "blue"] {
                let path = format!("/sys/class/leds/cucumber:{channel}:{node}{index}/trigger");
                fs::write(&path, trigger).with_context(|| format!("restore {path}"))?;
            }
        }
        Ok(())
    }
}
