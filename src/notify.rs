// Desktop notifications. macOS uses osascript (works without a signed bundle).
pub fn notify(title: &str, body: &str) {
    crate::util::log(format!("{title}: {body}"));
    if std::env::var_os("LANCLIP_NO_NOTIFY").is_some() {
        return;
    }
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        {
            let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
            let script = format!("display notification {} with title {}", q(&body), q(&title));
            let _ = crate::util::quiet(std::process::Command::new("osascript").arg("-e").arg(script)).status();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = notify_rust::Notification::new().appname("Lanclip").summary(&title).body(&body).show();
        }
    });
}
