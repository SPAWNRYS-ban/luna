use anyhow::{bail, Context, Result};
use luna::{MicrophoneBackend, MicrophoneDevice};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

const CHANNELS: usize = 8;
const SAMPLE_BYTES: usize = 4;
const PHYSICAL_MICROPHONES: usize = 4;

pub fn create_microphone_backend() -> Option<Box<dyn MicrophoneBackend>> {
    Some(Box::new(AlsaMicrophone))
}

struct AlsaMicrophone;

impl AlsaMicrophone {
    const ARECORD: &'static str = "/system/workdir/bin/arecord";
    const DEVICE: &'static str = "/dev/snd/pcmC0D2c";

    fn available(&self) -> bool {
        Path::new(Self::ARECORD).is_file() && Path::new(Self::DEVICE).exists()
    }
}

impl MicrophoneBackend for AlsaMicrophone {
    fn is_available(&self) -> Result<bool> {
        Ok(self.available())
    }

    fn devices(&self) -> Result<Vec<MicrophoneDevice>> {
        if !self.available() {
            bail!("Yandex Mini microphone capture is unavailable")
        }
        Ok((0..PHYSICAL_MICROPHONES)
            .map(|index| MicrophoneDevice {
                id: format!("raw_mic_{index}"),
                name: format!("Microphone {}", index + 1),
            })
            .collect())
    }

    fn record(&self, device: Option<&str>, seconds: u8, cancelled: &AtomicBool) -> Result<Vec<u8>> {
        if !self.available() {
            bail!("Yandex Mini microphone capture is unavailable")
        }
        let channel = device
            .map(|value| {
                value
                    .strip_prefix("raw_mic_")
                    .context("invalid microphone device ID")?
                    .parse::<usize>()
                    .context("invalid microphone device ID")
            })
            .transpose()?
            .unwrap_or(0);
        if channel >= PHYSICAL_MICROPHONES {
            bail!("unknown microphone device")
        }
        if cancelled.load(Ordering::Relaxed) {
            bail!("plugin stopped")
        }
        let mut child = Command::new(Self::ARECORD)
            .args([
                "-D",
                "dmicarray",
                "-f",
                "S32_LE",
                "-r",
                "48000",
                "-c",
                "8",
                "-t",
                "raw",
                "-d",
                &seconds.to_string(),
                "-q",
                "-",
            ])
            .env("LD_LIBRARY_PATH", "/system/vendor/quasar/libs")
            .env("ALSA_CONFIG_PATH", "/system/workdir/lib/alsa/alsa.conf")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start Yandex Mini microphone capture")?;
        let mut raw = Vec::new();
        child
            .stdout
            .take()
            .context("ALSA capture has no stdout")?
            .read_to_end(&mut raw)?;
        let status = child.wait()?;
        if cancelled.load(Ordering::Relaxed) {
            bail!("plugin stopped")
        }
        if !status.success() {
            bail!("ALSA microphone capture exited with {status}")
        }
        let mut pcm = Vec::with_capacity(raw.len() / CHANNELS / 3 * 2);
        for (index, frame) in raw.chunks_exact(CHANNELS * SAMPLE_BYTES).enumerate() {
            if index % 3 != 0 {
                continue;
            }
            let offset = channel * SAMPLE_BYTES;
            let sample = i32::from_le_bytes(frame[offset..offset + 4].try_into()?);
            let sample = (sample >> 16).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        if pcm.is_empty() {
            bail!("ALSA microphone capture returned no audio")
        }
        Ok(wav_from_pcm(&pcm))
    }
}

fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}
