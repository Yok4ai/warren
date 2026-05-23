//! Self-update: check GitHub Releases for a newer version and replace the running binary in place
//! (the same `warren-<target>.tar.gz` assets that `install.sh` downloads). All network/IO happens
//! on a background thread; results come back as `AppEvent`s.

use std::io::Read;

use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

const REPO: &str = "Yok4ai/warren";

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release-asset target triple for this build's OS/architecture.
fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// Startup check (background): if a newer release exists, apply it when `auto` is set, otherwise
/// just notify. Silent on network failure so offline launches aren't noisy.
pub fn spawn_check(tx: UnboundedSender<AppEvent>, auto: bool) {
    std::thread::spawn(move || {
        let Some(latest) = latest_version() else {
            return;
        };
        if !is_newer(&latest, current()) {
            return;
        }
        if auto {
            let _ = tx.send(AppEvent::UpdateResult(match apply() {
                Ok(()) => format!("updated to v{latest} — restart warren to apply"),
                Err(e) => format!("update failed: {e}"),
            }));
        } else {
            let _ = tx.send(AppEvent::UpdateAvailable(latest));
        }
    });
}

/// On-demand update (background), triggered from the command palette.
pub fn spawn_apply(tx: UnboundedSender<AppEvent>) {
    std::thread::spawn(move || {
        let msg = match latest_version() {
            Some(latest) if is_newer(&latest, current()) => match apply() {
                Ok(()) => format!("updated to v{latest} — restart warren to apply"),
                Err(e) => format!("update failed: {e}"),
            },
            Some(_) => format!("already up to date (v{})", current()),
            None => "could not reach GitHub to check for updates".to_string(),
        };
        let _ = tx.send(AppEvent::UpdateResult(msg));
    });
}

fn latest_version() -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = ureq::get(&url)
        .set("User-Agent", "warren")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

/// Numeric "a.b.c" comparison (ignoring any pre-release suffix).
fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.split('.')
            .map(|p| p.split(['-', '+']).next().unwrap_or("").parse().unwrap_or(0))
            .collect()
    }
    parts(latest) > parts(current)
}

/// Download the latest release asset for this platform and replace the running executable.
fn apply() -> Result<(), String> {
    let target = target().ok_or("unsupported platform")?;
    let asset = format!("warren-{target}.tar.gz");
    let url = format!("https://github.com/{REPO}/releases/latest/download/{asset}");
    let resp = ureq::get(&url)
        .set("User-Agent", "warren")
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(50_000_000)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;

    // Unpack the gzip+tar and pull out the `warren` binary.
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&buf[..]));
    let mut bin = Vec::new();
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let is_warren = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_owned()))
            .is_some_and(|n| n == "warren");
        if is_warren {
            entry.read_to_end(&mut bin).map_err(|e| e.to_string())?;
            break;
        }
    }
    if bin.is_empty() {
        return Err("no warren binary in the release archive".into());
    }

    // Write next to the current binary, then atomically rename over it (works while running).
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("cannot locate the install directory")?;
    let tmp = dir.join(".warren-update.tmp");
    std::fs::write(&tmp, &bin).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &exe).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not replace {} ({e})", exe.display())
    })?;
    Ok(())
}
