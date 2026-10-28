use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command, time::Duration};

fn shell_quote(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

pub(crate) fn stage_release_update(
    executable: &Path,
    data_dir: &Path,
    listen: &str,
    model: &str,
    architecture: &str,
) -> Result<bool> {
    const RELEASES: &str = "https://github.com/nyakokitsu/luna/releases/latest/download";
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()?;
    let manifest: serde_json::Value = client
        .get(format!("{RELEASES}/manifest.json"))
        .header("User-Agent", "luna-auto-updater")
        .send()?
        .error_for_status()?
        .json()?;
    let entry = manifest
        .get("devices")
        .and_then(serde_json::Value::as_array)
        .and_then(|devices| {
            devices.iter().find(|entry| {
                entry.get("device").and_then(serde_json::Value::as_str) == Some(model)
                    || entry
                        .get("models")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|models| {
                            models
                                .iter()
                                .any(|item| item.as_str().is_some_and(|name| name == model))
                        })
            })
        })
        .with_context(|| format!("no Luna release asset for model {model}"))?;
    let target = entry
        .get("target")
        .and_then(serde_json::Value::as_str)
        .context("Luna release manifest has no target")?;
    if (target.starts_with("aarch64-") && !matches!(architecture, "aarch64" | "arm64"))
        || (target.starts_with("armv7-") && !matches!(architecture, "arm" | "armv7" | "armv7l"))
    {
        bail!("Luna release target {target} does not match device architecture {architecture}");
    }
    let asset = entry
        .get("asset")
        .and_then(serde_json::Value::as_str)
        .context("Luna release manifest has no asset")?;
    let sums = client
        .get(format!("{RELEASES}/SHA256SUMS"))
        .header("User-Agent", "luna-auto-updater")
        .send()?
        .error_for_status()?
        .text()?;
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next()) {
                (Some(hash), Some(name)) if name == asset => Some(hash),
                _ => None,
            }
        })
        .context("release SHA256SUMS has no Luna checksum")?;
    if format!(
        "{:x}",
        Sha256::digest(fs::read(executable).unwrap_or_default())
    ) == expected
    {
        return Ok(false);
    }
    let binary = client
        .get(format!("{RELEASES}/{asset}"))
        .header("User-Agent", "luna-auto-updater")
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec();
    if format!("{:x}", Sha256::digest(&binary)) != expected {
        bail!("release checksum mismatch")
    }
    let staged = executable.with_extension("luna-update");
    fs::write(&staged, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    }
    let helper = data_dir.join(".luna-restart-update.sh");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nsleep 1\nmv {} {}\nexec {} --listen {} --data-dir {}\n",
            shell_quote(staged.as_os_str()),
            shell_quote(executable.as_os_str()),
            shell_quote(executable.as_os_str()),
            shell_quote(std::ffi::OsStr::new(listen)),
            shell_quote(data_dir.as_os_str()),
        ),
    )?;
    Command::new("sh").arg(helper).spawn()?;
    Ok(true)
}

/// Binary packages reach the daemon on loopback unless it listens on one address only.
pub(crate) fn runtime_api_url(listen: &str) -> Result<String> {
    let mut address: std::net::SocketAddr = listen
        .parse()
        .with_context(|| format!("listen address {listen} must be IP:port"))?;
    if address.ip().is_unspecified() {
        address.set_ip(match address {
            std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    Ok(format!("http://{address}"))
}
