use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::{fs, path::PathBuf};

const CHANNEL_COUNT: usize = 18;

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    let mut paths = Vec::new();
    for index in 1..=6 {
        for channel in ["red", "green", "blue"] {
            let path = PathBuf::from(format!(
                "/sys/class/leds/mini2:{channel}:ring{index}/brightness"
            ));
            if path.exists() {
                paths.push(path);
            }
        }
    }
    Ok(Box::new(SysfsRing { paths }))
}

struct SysfsRing {
    paths: Vec<PathBuf>,
}

impl LedBackend for SysfsRing {
    fn show(&self, frame: &[u8]) -> Result<()> {
        if frame.len() != CHANNEL_COUNT {
            bail!("unexpected Yandex Mini 2 LED frame length");
        }
        if self.paths.is_empty() {
            bail!("Yandex Mini 2 LED sysfs nodes are unavailable");
        }
        for (path, value) in self.paths.iter().zip(frame) {
            fs::write(path, value.to_string())
                .with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    }
}
