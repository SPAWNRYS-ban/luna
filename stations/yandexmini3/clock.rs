use anyhow::{bail, Context, Result};
use luna::{ClockBackend, ClockCommand};
use std::{collections::HashMap, fs, path::Path};

const ROOT: &str = "/sys/class/leds";

pub fn create_clock_backend() -> Option<Box<dyn ClockBackend>> {
    let clock = Clock::detect()?;
    Some(Box::new(clock))
}

struct Clock {
    paths: Vec<(String, String)>,
}

impl Clock {
    fn names() -> Vec<String> {
        let mut out = Vec::new();
        for position in 0..4 {
            for part in ["A", "B", "C", "D", "E", "F", "G"] {
                out.push(format!("{part}{position}"));
            }
        }
        out.extend(["DEGREE", "DH", "DL", "DM"].into_iter().map(String::from));
        out
    }

    fn detect() -> Option<Self> {
        let paths = Self::names()
            .into_iter()
            .map(|name| {
                let path = Path::new(ROOT).join(format!("bergamot:white:clock{name}/brightness"));
                (name, path.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        paths
            .iter()
            .all(|(_, path)| Path::new(path).is_file())
            .then_some(Self { paths })
    }

    fn blank(&self) -> HashMap<String, u8> {
        self.paths
            .iter()
            .map(|(name, _)| (name.clone(), 0))
            .collect()
    }
    fn glyph(ch: char) -> Option<&'static str> {
        Some(match ch {
            '0' => "ABCDEF",
            '1' => "BC",
            '2' => "ABDEG",
            '3' => "ABCDG",
            '4' => "BCFG",
            '5' => "ACDFG",
            '6' => "ACDEFG",
            '7' => "ABC",
            '8' => "ABCDEFG",
            '9' => "ABCDFG",
            '-' => "G",
            ' ' => "",
            _ => return None,
        })
    }
    fn write(&self, frame: &HashMap<String, u8>) -> Result<()> {
        for (name, path) in &self.paths {
            fs::write(path, frame.get(name).copied().unwrap_or(0).to_string())
                .with_context(|| format!("write clock segment {name}"))?;
        }
        Ok(())
    }
    fn time_frame(&self, text: &str, brightness: u8) -> Result<HashMap<String, u8>> {
        let (left, right) = text
            .split_once(':')
            .context("clock time must look like HH:MM")?;
        if left.len() > 2 || right.len() != 2 {
            bail!("clock time must look like HH:MM")
        }
        let chars = format!("{left:>2}{right}");
        let mut frame = self.blank();
        for (position, ch) in chars.chars().enumerate() {
            for part in Self::glyph(ch)
                .context("clock supports digits, '-' and spaces")?
                .chars()
            {
                frame.insert(format!("{part}{position}"), brightness);
            }
        }
        frame.insert("DH".into(), brightness);
        frame.insert("DL".into(), brightness);
        Ok(frame)
    }
}

impl ClockBackend for Clock {
    fn segments(&self) -> Vec<String> {
        self.paths.iter().map(|(name, _)| name.clone()).collect()
    }
    fn apply(&self, command: ClockCommand) -> Result<()> {
        match command {
            ClockCommand::Off => self.write(&self.blank()),
            ClockCommand::Segments(values) => {
                let mut frame = self.blank();
                for (name, value) in values {
                    if !frame.contains_key(&name) {
                        bail!("unknown clock segment {name}")
                    }
                    frame.insert(name, value);
                }
                self.write(&frame)
            }
            ClockCommand::Time { text, brightness } => {
                self.write(&self.time_frame(&text, brightness)?)
            }
        }
    }
}
