use anyhow::{bail, Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const APLAY_PATHS: &[&str] = &["/system/workdir/bin/aplay", "/usr/bin/aplay"];
const ALSA_LIBS: &str = "/bkupgrade/data/bundle/ap2/libs:/system/workdir/lib";
const ALSA_CONFIG: &str = "/bkupgrade/data/bundle/ap2/asound.conf";
const ALSA_DEVICE: &str = "sps48";
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;
const MAX_PLAYBACK_SECONDS: u64 = 600;

fn find_aplay() -> Option<PathBuf> {
    let from_path = env::var_os("PATH")
        .into_iter()
        .flat_map(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join("aplay"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    from_path
        .into_iter()
        .chain(APLAY_PATHS.iter().map(PathBuf::from))
        .find(|path| path.is_file())
}

fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    if path == Path::new(APLAY_PATHS[0]) {
        command
            .env("LD_LIBRARY_PATH", ALSA_LIBS)
            .env("ALSA_CONFIG_PATH", ALSA_CONFIG)
            .arg("-D")
            .arg(ALSA_DEVICE);
    }
    command
}

pub fn available() -> bool {
    let Some(path) = find_aplay() else {
        return false;
    };
    command(&path)
        .arg("--list-devices")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("PLAYBACK Hardware Devices")
        })
}

fn validate_wav(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_AUDIO_BYTES {
        bail!("audio file exceeds 64 MiB")
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("audio playback accepts PCM WAV files only")
    }
    let riff_end = u32::from_le_bytes(bytes[4..8].try_into()?) as usize + 8;
    if riff_end > bytes.len() {
        bail!("WAV file is truncated")
    }
    let mut offset = 12;
    let mut format = None;
    let mut data_len = None;
    while offset + 8 <= riff_end {
        let chunk_len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(chunk_len)
            .context("WAV chunk size overflow")?;
        if end > riff_end {
            bail!("WAV chunk extends beyond the RIFF container")
        }
        match &bytes[offset..offset + 4] {
            b"fmt " if chunk_len >= 16 => {
                let encoding = u16::from_le_bytes(bytes[start..start + 2].try_into()?);
                let channels = u16::from_le_bytes(bytes[start + 2..start + 4].try_into()?);
                let rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into()?);
                let bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into()?);
                if ![1, 3].contains(&encoding)
                    || !(1..=2).contains(&channels)
                    || !(8_000..=192_000).contains(&rate)
                    || ![8, 16, 24, 32].contains(&bits)
                {
                    bail!("WAV must be mono/stereo PCM or float at a supported sample rate")
                }
                format = Some((u64::from(channels), u64::from(rate), u64::from(bits)));
            }
            b"data" => data_len = Some(chunk_len as u64),
            _ => {}
        }
        offset = end + (chunk_len & 1);
    }
    let (channels, rate, bits) = format.context("WAV format chunk is missing")?;
    let data_len = data_len.context("WAV audio data chunk is missing")?;
    let bytes_per_second = channels * u64::from(rate) * bits / 8;
    if bytes_per_second == 0 || data_len == 0 {
        bail!("WAV audio data is empty")
    }
    if data_len.div_ceil(bytes_per_second) > MAX_PLAYBACK_SECONDS {
        bail!("audio playback is limited to 600 seconds")
    }
    Ok(())
}

pub fn play(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read audio file {}", path.display()))?;
    validate_wav(&bytes)?;
    let aplay = find_aplay().context("aplay is not available on this station")?;
    let mut child = command(&aplay)
        .arg("--quiet")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start {}", aplay.display()))?;
    std::thread::spawn(move || {
        if let Err(error) = child.wait() {
            tracing::warn!(%error, "audio playback process failed");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_wav;

    #[test]
    fn rejects_non_wav_audio() {
        assert!(validate_wav(b"not audio").is_err());
    }
}
