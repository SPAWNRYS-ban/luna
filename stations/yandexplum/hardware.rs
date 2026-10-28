use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::fs;

const COUNT: usize = 3;

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    Ok(Box::new(SysfsRing))
}

struct SysfsRing;

impl LedBackend for SysfsRing {
    fn show(&self, frame: &[u8]) -> Result<()> {
        if frame.len() != COUNT * 3 {
            bail!("unexpected Yandex Plum LED frame length");
        }
        for index in 0..COUNT {
            for (channel, value) in ["red", "green", "blue"]
                .iter()
                .zip(&frame[index * 3..index * 3 + 3])
            {
                let path = format!(
                    "/sys/class/leds/plum:{channel}:ring{}/brightness",
                    index + 1
                );
                fs::write(&path, value.to_string()).with_context(|| format!("write {path}"))?;
            }
        }
        Ok(())
    }
}
