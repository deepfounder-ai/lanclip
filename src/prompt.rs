// Native yes/no pop-up. Returns None when no dialog is available (the caller falls back to the web page).
pub fn ask(title: &str, message: &str, yes: &str, no: &str) -> Option<bool> {
    if std::env::var_os("LANCLIP_NO_PROMPT").is_some() {
        return None;
    }
    imp(title, message, yes, no)
}

#[cfg(windows)]
fn imp(title: &str, message: &str, _yes: &str, _no: &str) -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, m) = (wide(title), wide(&format!("{message}\n\nAllow this device to connect?")));
    let r = unsafe { MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND) };
    Some(r == IDYES)
}

#[cfg(target_os = "macos")]
fn imp(title: &str, message: &str, yes: &str, no: &str) -> Option<bool> {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!(
        "display dialog {} with title {} buttons {{{}, {}}} default button {} with icon note giving up after 110",
        q(message), q(title), q(no), q(yes), q(yes));
    let out = std::process::Command::new("osascript").arg("-e").arg(script).output().ok()?;
    // Clicking the "no" button exits with an error ("User canceled"), which also counts as a decline.
    Some(String::from_utf8_lossy(&out.stdout).contains(&format!("button returned:{yes}")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn imp(title: &str, message: &str, yes: &str, no: &str) -> Option<bool> {
    use std::process::Command;
    let zenity = Command::new("zenity")
        .args(["--question", "--title", title, "--text", message, "--ok-label", yes, "--cancel-label", no, "--timeout", "110"])
        .status();
    if let Ok(s) = zenity {
        return Some(s.success());
    }
    let kdialog = Command::new("kdialog").args(["--title", title, "--yes-label", yes, "--no-label", no, "--yesno", message]).status();
    kdialog.ok().map(|s| s.success())
}
