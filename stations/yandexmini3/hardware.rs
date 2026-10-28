use anyhow::{bail, Context, Result};
use luna::LedBackend;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::FileExt,
    path::Path,
};

const COUNT: usize = 6;
// The framebuffer indexes the ring as alternating opposite LEDs:
// physical positions read framebuffer indexes 0, 2, 4, 1, 3, 5.
// Convert the logical ring order back to framebuffer order before writing.
const FRAMEBUFFER_ORDER: [usize; COUNT] = [0, 3, 1, 4, 2, 5];
const FRAMEBUFFER: &str = "/dev/fb0";
const SYSFS: &str = "/sys/class/graphics/fb0";

pub fn create_led_backend() -> Result<Box<dyn LedBackend>> {
    Ok(Box::new(Ring::open()?))
}

struct Ring {
    file: File,
}

impl Ring {
    fn open() -> Result<Self> {
        let geometry = fs::read_to_string(Path::new(SYSFS).join("virtual_size"))?;
        let depth = fs::read_to_string(Path::new(SYSFS).join("bits_per_pixel"))?;
        if geometry.trim() != "6,1" || depth.trim() != "24" {
            bail!("{FRAMEBUFFER} is not the Yandex Mini 3 logo ring")
        }
        Ok(Self {
            file: OpenOptions::new()
                .write(true)
                .open(FRAMEBUFFER)
                .context("open Mini 3 framebuffer")?,
        })
    }
}

impl LedBackend for Ring {
    fn show(&self, frame: &[u8]) -> Result<()> {
        if frame.len() != COUNT * 3 {
            bail!("unexpected Yandex Mini 3 LED frame length")
        }
        let mut ordered = [0u8; COUNT * 3];
        for (framebuffer_index, logical_index) in FRAMEBUFFER_ORDER.into_iter().enumerate() {
            let source = logical_index * 3;
            let destination = framebuffer_index * 3;
            ordered[destination..destination + 3].copy_from_slice(&frame[source..source + 3]);
        }
        // Mini 3's LED framebuffer exposes one frame behind the currently
        // latched hardware state. Writing the same frame twice makes the
        // request visible immediately instead of on the next API call.
        self.file
            .write_all_at(&ordered, 0)
            .and_then(|_| self.file.write_all_at(&ordered, 0))
            .context("write Mini 3 logo ring")
    }
}
