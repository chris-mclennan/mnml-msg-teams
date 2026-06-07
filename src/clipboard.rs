//! Cross-platform clipboard write — shells out to `pbcopy` (macOS),
//! `wl-copy` / `xclip` (Linux), or `clip.exe` (Windows). Zero deps.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy(text: &str) -> Result<(), String> {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if cfg!(target_os = "windows") {
        ("clip", &[])
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {cmd}: {e} — is it on PATH?"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("write to {cmd} stdin: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("wait {cmd}: {e}"))?;
    if !status.success() {
        return Err(format!("{cmd} exited {status}"));
    }
    Ok(())
}
