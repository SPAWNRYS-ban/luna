use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::{fs, path::PathBuf};

const CHANNEL_COUNT: usize = 18;

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    let mut paths = Vec::new();
    for index in 1..=6 {
        for (channel_index, channel) in ["red", "green", "blue"].iter().enumerate() {
            let path = PathBuf::from(format!(
                "/sys/class/leds/mini2:{channel}:ring{index}/brightness"
            ));
            if path.exists() {
                paths.push(((index - 1) * 3 + channel_index, path));
            }
        }
    }
    Ok(Box::new(SysfsRing { paths }))
}

struct SysfsRing {
    paths: Vec<(usize, PathBuf)>,
}

impl LedBackend for SysfsRing {
    fn show(&self, frame: &[u8]) -> Result<()> {
        if frame.len() != CHANNEL_COUNT {
            bail!("unexpected Yandex Mini 2 LED frame length");
        }
        if self.paths.is_empty() {
            bail!("Yandex Mini 2 LED sysfs nodes are unavailable");
        }
        for (frame_index, path) in &self.paths {
            let value = frame[*frame_index];
            fs::write(path, value.to_string())
                .with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    }
}
