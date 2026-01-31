/// Send a desktop notification (macOS and Linux)
pub fn send_desktop_notification(title: &str, message: &str) {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    {
        let safe_title = title.replace('"', "\\\"");
        let safe_msg = message.replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            safe_msg, safe_title
        );
        let _ = Command::new("osascript").arg("-e").arg(&script).output();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send")
            .arg("--app-name=Slack Client")
            .arg("--urgency=normal")
            .arg("--expire-time=5000")
            .arg(title)
            .arg(message)
            .output();
    }
}
