// OS integration: context-menu entries, start at login, self-install and uninstall.
//   Windows: Explorer verbs + Run key + Start Menu shortcut + "Apps" uninstall entry (all per-user, HKCU)
//   macOS:   Finder Quick Actions in ~/Library/Services + LaunchAgent
//   Linux:   Nautilus/Caja scripts, Nemo actions, Dolphin service menus, Thunar actions, autostart + launcher
#![allow(dead_code)]
use crate::util::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SEND: &str = "Copy to LAN";
pub const PASTE: &str = "Paste from LAN";
pub const CLIP: &str = "Copy clipboard to LAN";
pub const TEXT: &str = "Copy text to LAN";

fn home() -> PathBuf { dirs::home_dir().unwrap_or_default() }

fn write_exec(path: &Path, content: &str) {
    if let Some(p) = path.parent() { let _ = fs::create_dir_all(p); }
    let _ = fs::write(path, content);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
}

fn sh_quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }
fn xml(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;") }

/// The 64px app icon as RGBA (drawn in code so the repo has no binary assets).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let r = s * 0.22;
    for y in 0..size {
        for x in 0..size {
            let (xf, yf) = (x as f32, y as f32);
            let dx = (r - xf).max(0.0).max(xf - (s - 1.0 - r));
            let dy = (r - yf).max(0.0).max(yf - (s - 1.0 - r));
            if dx * dx + dy * dy > r * r { continue; }
            let (u, v) = (xf / s, yf / s);
            let board = u > 0.27 && u < 0.73 && v > 0.24 && v < 0.82;
            let clip = u > 0.39 && u < 0.61 && v > 0.16 && v < 0.3;
            let line = board && u > 0.35 && u < 0.65 && ((v > 0.42 && v < 0.47) || (v > 0.55 && v < 0.6) || (v > 0.68 && v < 0.73));
            let c = if (board || clip) && !line { [255, 255, 255] } else { [79, 70, 229] };
            let i = ((y * size + x) * 4) as usize;
            buf[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    buf
}

fn icon_png() -> Vec<u8> { crate::clip::encode_png(64, 64, &icon_rgba(64)).unwrap_or_default() }

// ================================================================= public API

/// Where the binary should live. On Windows/Linux the downloaded binary installs itself there;
/// on macOS the .app bundle stays where the user put it.
pub fn install_target() -> PathBuf {
    #[cfg(windows)]
    { return dirs::data_local_dir().unwrap_or_default().join("Programs").join("Lanclip").join("lanclip.exe"); }
    #[cfg(target_os = "linux")]
    { return home().join(".local").join("bin").join("lanclip"); }
    #[allow(unreachable_code)]
    std::env::current_exe().unwrap_or_default()
}

/// Copy the running binary to its install location (if it isn't already there). Returns the installed path.
pub fn install_binary() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let target = install_target();
    if same_file(&exe, &target) { return Ok(target); }
    #[cfg(windows)]
    windows::remove_electron_version(&target);
    if let Some(p) = target.parent() { fs::create_dir_all(p)?; }
    // An older running copy may lock the file; retry briefly after asking it to quit.
    for attempt in 0..20 {
        match fs::copy(&exe, &target) {
            Ok(_) => break,
            Err(e) if attempt == 19 => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    }
    Ok(target)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) { (Ok(x), Ok(y)) => x == y, _ => false }
}

/// Install/refresh context-menu entries pointing at `exe`.
pub fn install_menus(exe: &Path) {
    #[cfg(windows)] windows::install(exe);
    #[cfg(target_os = "macos")] mac::install(exe);
    #[cfg(target_os = "linux")] linux::install(exe);
}

pub fn uninstall() {
    set_autostart(Path::new(""), false);
    #[cfg(windows)] windows::uninstall();
    #[cfg(target_os = "macos")] mac::uninstall();
    #[cfg(target_os = "linux")] linux::uninstall();
}

pub fn set_autostart(exe: &Path, enabled: bool) {
    #[cfg(windows)] windows::set_autostart(exe, enabled);
    #[cfg(target_os = "macos")] mac::set_autostart(exe, enabled);
    #[cfg(target_os = "linux")] linux::set_autostart(exe, enabled);
    let _ = (exe, enabled);
}

pub fn autostart_enabled() -> bool {
    #[cfg(windows)] return windows::autostart_enabled();
    #[cfg(target_os = "macos")] return mac::launch_agent().exists();
    #[cfg(target_os = "linux")] return linux::autostart_file().exists();
    #[allow(unreachable_code)]
    false
}

/// Show a file in the OS file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    { let _ = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn(); }
    #[cfg(target_os = "macos")]
    { let _ = Command::new("open").arg("-R").arg(path).spawn(); }
    #[cfg(target_os = "linux")]
    { if let Some(p) = path.parent() { let _ = open::that_detached(p); } }
}

// ================================================================= Windows
#[cfg(windows)]
mod windows {
    use super::*;

    const CLASSES: &str = r"HKCU\Software\Classes";
    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const UNINSTALL: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lanclip";

    fn verbs() -> Vec<(String, &'static str, Vec<&'static str>)> {
        vec![
            (format!(r"{CLASSES}\*\shell\Lanclip.Send"), SEND, vec!["send", "%1"]),
            (format!(r"{CLASSES}\Directory\shell\Lanclip.Send"), SEND, vec!["send", "%1"]),
            (format!(r"{CLASSES}\Directory\shell\Lanclip.Paste"), PASTE, vec!["paste", "%1"]),
            (format!(r"{CLASSES}\Directory\Background\shell\Lanclip.Paste"), PASTE, vec!["paste", "%V"]),
            (format!(r"{CLASSES}\Directory\Background\shell\Lanclip.Clip"), CLIP, vec!["send-clipboard"]),
        ]
    }

    fn reg(args: &[&str]) -> bool {
        quiet(Command::new("reg").args(args)).status().map(|s| s.success()).unwrap_or(false)
    }

    fn icon_path(exe: &Path) -> PathBuf {
        // ICO file that simply wraps a PNG.
        let png = icon_png();
        let mut ico = vec![0, 0, 1, 0, 1, 0, 64, 64, 0, 0, 1, 0, 32, 0];
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(&png);
        let path = exe.with_file_name("lanclip.ico");
        let _ = fs::write(&path, ico);
        path
    }

    pub fn install(exe: &Path) {
        let exe_s = exe.display().to_string();
        let icon = icon_path(exe).display().to_string();
        for (key, label, args) in verbs() {
            let command = std::iter::once(exe_s.as_str()).chain(args).map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" ");
            reg(&["add", &key, "/ve", "/d", label, "/f"]);
            reg(&["add", &key, "/v", "Icon", "/d", &icon, "/f"]);
            reg(&["add", &key, "/v", "MultiSelectModel", "/d", "Player", "/f"]);
            reg(&["add", &format!(r"{key}\command"), "/ve", "/d", &command, "/f"]);
        }
        // Settings > Apps entry
        let un = format!("\"{exe_s}\" uninstall");
        reg(&["add", UNINSTALL, "/v", "DisplayName", "/d", "Lanclip", "/f"]);
        reg(&["add", UNINSTALL, "/v", "DisplayVersion", "/d", env!("CARGO_PKG_VERSION"), "/f"]);
        reg(&["add", UNINSTALL, "/v", "Publisher", "/d", "Lanclip", "/f"]);
        reg(&["add", UNINSTALL, "/v", "DisplayIcon", "/d", &icon, "/f"]);
        reg(&["add", UNINSTALL, "/v", "UninstallString", "/d", &un, "/f"]);
        reg(&["add", UNINSTALL, "/v", "NoModify", "/t", "REG_DWORD", "/d", "1", "/f"]);
        reg(&["add", UNINSTALL, "/v", "NoRepair", "/t", "REG_DWORD", "/d", "1", "/f"]);
        // Start Menu shortcut (opens settings / starts the service)
        let lnk = start_menu_lnk();
        let ps = format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.IconLocation='{}';$s.Description='Lanclip settings';$s.Save()",
            lnk.display().to_string().replace('\'', "''"), exe_s.replace('\'', "''"), icon.replace('\'', "''"));
        let _ = quiet(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps])).status();
    }

    fn start_menu_lnk() -> PathBuf {
        dirs::data_dir().unwrap_or_default().join(r"Microsoft\Windows\Start Menu\Programs\Lanclip.lnk")
    }

    pub fn uninstall() {
        for (key, _, _) in verbs() { reg(&["delete", &key, "/f"]); }
        reg(&["delete", UNINSTALL, "/f"]);
        let _ = fs::remove_file(start_menu_lnk());
        // Remove the installed binary after this process exits.
        let target = install_target();
        if let Some(dir) = target.parent() {
            if std::env::current_exe().map(|e| e.starts_with(dir)).unwrap_or(false) {
                let cmd = format!("ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
                let _ = quiet(Command::new("cmd").args(["/c", &cmd])).spawn();
            }
        }
    }

    /// Lanclip 0.1 was an Electron app installed by NSIS into the same folder. Stop it and run its
    /// uninstaller so it can't fight over the context menu entries.
    pub fn remove_electron_version(target: &Path) {
        let Some(dir) = target.parent() else { return };
        let uninstaller = dir.join("Uninstall Lanclip.exe");
        if !uninstaller.exists() { return; }
        let ps = format!(
            "Get-Process | Where-Object {{ $_.Path -like '{}\\*' }} | Stop-Process -Force",
            dir.display().to_string().replace('\'', "''"));
        let _ = quiet(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps])).status();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = quiet(Command::new(&uninstaller).arg("/S")).status();
        // The NSIS uninstaller copies itself to %TEMP% and returns early; wait for the old files to go.
        for _ in 0..40 {
            if !dir.join("Lanclip.exe").exists() || !uninstaller.exists() { break; }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        reg(&["delete", RUN, "/v", "electron.app.Lanclip", "/f"]);
    }

    pub fn set_autostart(exe: &Path, enabled: bool) {
        if enabled {
            reg(&["add", RUN, "/v", "Lanclip", "/d", &format!("\"{}\" daemon", exe.display()), "/f"]);
        } else {
            reg(&["delete", RUN, "/v", "Lanclip", "/f"]);
        }
    }

    pub fn autostart_enabled() -> bool {
        reg(&["query", RUN, "/v", "Lanclip"])
    }
}

// ================================================================= macOS
#[cfg(target_os = "macos")]
mod mac {
    use super::*;

    fn services() -> PathBuf { home().join("Library/Services") }
    pub fn launch_agent() -> PathBuf { home().join("Library/LaunchAgents/app.lanclip.plist") }

    pub fn install(exe: &Path) {
        let q = sh_quote(&exe.display().to_string());
        let finder_dir = r#"osascript -e 'tell application "Finder" to POSIX path of (insertion location as alias)' 2>/dev/null"#;
        workflow(SEND, true, &format!("{q} send \"$@\""));
        workflow(PASTE, true, &format!("dir=\"$1\"\n[ -z \"$dir\" ] && dir=$({finder_dir})\n[ -f \"$dir\" ] && dir=$(dirname \"$dir\")\n{q} paste \"$dir\""));
        workflow(CLIP, true, &format!("{q} send-clipboard"));
        workflow(TEXT, false, &format!("{q} send-text"));
        let _ = quiet(&mut Command::new("/System/Library/CoreServices/pbs")).arg("-update").status();
    }

    pub fn uninstall() {
        for name in [SEND, PASTE, CLIP, TEXT] {
            let _ = fs::remove_dir_all(services().join(format!("{name}.workflow")));
        }
        let _ = quiet(&mut Command::new("/System/Library/CoreServices/pbs")).arg("-update").status();
    }

    pub fn set_autostart(exe: &Path, enabled: bool) {
        let path = launch_agent();
        if !enabled { let _ = fs::remove_file(path); return; }
        let _ = fs::create_dir_all(path.parent().unwrap());
        let _ = fs::write(&path, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>app.lanclip</string>
  <key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#, xml(&exe.display().to_string())));
    }

    fn uuid() -> String { new_id().to_uppercase() }

    fn workflow(name: &str, files: bool, script: &str) {
        let dir = services().join(format!("{name}.workflow")).join("Contents");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
        let _ = fs::create_dir_all(&dir);
        let (send_key, send_type) = if files { ("NSSendFileTypes", "public.item") } else { ("NSSendTypes", "public.utf8-plain-text") };
        let context = if files { "<key>NSRequiredContext</key><dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict>" } else { "<key>NSRequiredContext</key><dict/>" };
        let _ = fs::write(dir.join("Info.plist"), format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>{n}</string>
  <key>NSServices</key>
  <array><dict>
    <key>NSMenuItem</key><dict><key>default</key><string>{n}</string></dict>
    <key>NSMessage</key><string>runWorkflowAsService</string>
    {context}
    <key>{send_key}</key><array><string>{send_type}</string></array>
  </dict></array>
</dict>
</plist>
"#, n = xml(name)));
        let input_type = if files { "com.apple.Automator.fileSystemObject" } else { "com.apple.Automator.text" };
        let finder = if files { "<key>serviceApplicationBundleID</key><string>com.apple.finder</string><key>serviceApplicationPath</key><string>/System/Library/CoreServices/Finder.app</string>" } else { "" };
        let _ = fs::write(dir.join("document.wflow"), format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>AMApplicationBuild</key><string>523</string>
  <key>AMApplicationVersion</key><string>2.10</string>
  <key>AMDocumentVersion</key><string>2</string>
  <key>actions</key>
  <array><dict>
    <key>action</key>
    <dict>
      <key>AMAccepts</key><dict><key>Container</key><string>List</string><key>Optional</key><true/><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
      <key>AMActionVersion</key><string>2.0.3</string>
      <key>AMApplication</key><array><string>Automator</string></array>
      <key>AMParameterProperties</key><dict><key>COMMAND_STRING</key><dict/><key>CheckedForUserDefaultShell</key><dict/><key>inputMethod</key><dict/><key>shell</key><dict/><key>source</key><dict/></dict>
      <key>AMProvides</key><dict><key>Container</key><string>List</string><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
      <key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string>
      <key>ActionName</key><string>Run Shell Script</string>
      <key>ActionParameters</key>
      <dict>
        <key>COMMAND_STRING</key><string>{script}</string>
        <key>CheckedForUserDefaultShell</key><true/>
        <key>inputMethod</key><integer>{input_method}</integer>
        <key>shell</key><string>/bin/bash</string>
        <key>source</key><string></string>
      </dict>
      <key>BundleIdentifier</key><string>com.apple.RunShellScript</string>
      <key>CFBundleVersion</key><string>2.0.3</string>
      <key>CanShowSelectedItemsWhenRun</key><false/>
      <key>CanShowWhenRun</key><true/>
      <key>Category</key><array><string>AMCategoryUtilities</string></array>
      <key>Class Name</key><string>RunShellScriptAction</string>
      <key>InputUUID</key><string>{u1}</string>
      <key>Keywords</key><array><string>Shell</string><string>Script</string></array>
      <key>OutputUUID</key><string>{u2}</string>
      <key>UUID</key><string>{u3}</string>
      <key>UnlocalizedApplications</key><array><string>Automator</string></array>
      <key>arguments</key><dict/>
      <key>isViewVisible</key><integer>1</integer>
      <key>location</key><string>309.000000:253.000000</string>
    </dict>
    <key>isViewVisible</key><integer>1</integer>
  </dict></array>
  <key>connectors</key><dict/>
  <key>workflowMetaData</key>
  <dict>
    <key>applicationBundleIDsByPath</key><dict/>
    <key>applicationPaths</key><array/>
    <key>inputTypeIdentifier</key><string>{input_type}</string>
    <key>outputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
    <key>presentationMode</key><integer>15</integer>
    <key>processesInput</key><integer>0</integer>
    {finder}
    <key>serviceInputTypeIdentifier</key><string>{input_type}</string>
    <key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
    <key>serviceProcessesInput</key><integer>0</integer>
    <key>systemImageName</key><string>NSActionTemplate</string>
    <key>useAutomaticInputType</key><integer>0</integer>
    <key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string>
  </dict>
</dict>
</plist>
"#, script = xml(script), input_method = if files { 1 } else { 0 }, u1 = uuid(), u2 = uuid(), u3 = uuid()));
    }
}

// ================================================================= Linux
#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    fn data() -> PathBuf { std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")) }
    fn config() -> PathBuf { std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")) }
    pub fn autostart_file() -> PathBuf { config().join("autostart/lanclip.desktop") }
    fn launcher_file() -> PathBuf { data().join("applications/lanclip.desktop") }
    fn icon_file() -> PathBuf { data().join("icons/hicolor/64x64/apps/lanclip.png") }
    fn thunar() -> PathBuf { config().join("Thunar/uca.xml") }
    const T_BEGIN: &str = "<!-- lanclip:begin -->";
    const T_END: &str = "<!-- lanclip:end -->";

    pub fn install(exe: &Path) {
        let e = exe.display().to_string();
        let q = sh_quote(&e);

        let _ = fs::create_dir_all(icon_file().parent().unwrap());
        let _ = fs::write(icon_file(), icon_png());
        let _ = fs::create_dir_all(launcher_file().parent().unwrap());
        let _ = fs::write(launcher_file(), format!("[Desktop Entry]\nType=Application\nName=Lanclip\nComment=Copy to LAN / Paste from LAN settings\nExec={q}\nIcon=lanclip\nCategories=Utility;Network;\nTerminal=false\n"));

        // Nautilus (GNOME Files) and Caja: Scripts submenu; selection arrives as arguments, cwd = folder.
        for dir in [data().join("nautilus/scripts"), config().join("caja/scripts")] {
            write_exec(&dir.join(SEND), &format!("#!/bin/sh\n[ $# -gt 0 ] && exec {q} send \"$@\"\n"));
            write_exec(&dir.join(PASTE), &format!("#!/bin/sh\nd=\"$PWD\"\n[ $# -eq 1 ] && [ -d \"$1\" ] && d=\"$1\"\nexec {q} paste \"$d\"\n"));
            write_exec(&dir.join(CLIP), &format!("#!/bin/sh\nexec {q} send-clipboard\n"));
        }

        // Nemo (Cinnamon)
        let nemo = data().join("nemo/actions");
        let _ = fs::create_dir_all(&nemo);
        let action = |file: &str, name: &str, args: &str, sel: &str, ext: &str| {
            let _ = fs::write(nemo.join(file), format!("[Nemo Action]\nName={name}\nComment=Lanclip\nExec=\"{e}\" {args}\nIcon-Name=edit-copy\nSelection={sel}\nExtensions={ext};\n"));
        };
        action("lanclip-send.nemo_action", SEND, "send %F", "notnone", "any");
        action("lanclip-paste.nemo_action", PASTE, "paste %P", "none", "any");
        action("lanclip-paste-into.nemo_action", PASTE, "paste %F", "s", "dir");
        action("lanclip-clip.nemo_action", CLIP, "send-clipboard", "none", "any");

        // Dolphin (KDE); Plasma 6 requires executable service menus.
        let kio = data().join("kio/servicemenus");
        write_exec(&kio.join("lanclip-send.desktop"), &format!("[Desktop Entry]\nType=Service\nMimeType=all/allfiles;inode/directory;\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nActions=send;\nX-KDE-Priority=TopLevel\n\n[Desktop Action send]\nName={SEND}\nIcon=edit-copy\nExec={q} send %F\n"));
        write_exec(&kio.join("lanclip-paste.desktop"), &format!("[Desktop Entry]\nType=Service\nMimeType=inode/directory;\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nActions=paste;clip;\nX-KDE-Priority=TopLevel\n\n[Desktop Action paste]\nName={PASTE}\nIcon=edit-paste\nExec={q} paste %f\n\n[Desktop Action clip]\nName={CLIP}\nIcon=edit-copy\nExec={q} send-clipboard\n"));

        // Thunar (XFCE): merge our actions into uca.xml between markers.
        let act = |name: &str, icon: &str, cmd: &str, kinds: &str| format!(
            "<action>\n  <icon>{icon}</icon><name>{}</name><submenu></submenu>\n  <unique-id>lanclip-{}</unique-id>\n  <command>{}</command><description>Lanclip</description>\n  <range></range><patterns>*</patterns>{kinds}\n</action>",
            xml(name), name.replace(' ', "-").to_lowercase(), xml(cmd));
        let block = [
            T_BEGIN.to_string(),
            act(SEND, "edit-copy", &format!("{q} send %F"), "<directories/><audio-files/><image-files/><other-files/><text-files/><video-files/>"),
            act(PASTE, "edit-paste", &format!("{q} paste %f"), "<directories/>"),
            T_END.to_string(),
        ].join("\n");
        let uca = fs::read_to_string(thunar()).unwrap_or_else(|_| "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n".into());
        let stripped = strip_thunar(&uca);
        let merged = match stripped.rfind("</actions>") {
            Some(i) => format!("{}{block}\n</actions>\n", &stripped[..i]),
            None => stripped,
        };
        let _ = fs::create_dir_all(thunar().parent().unwrap());
        let _ = fs::write(thunar(), merged);
    }

    fn strip_thunar(text: &str) -> String {
        match (text.find(T_BEGIN), text.find(T_END)) {
            (Some(a), Some(b)) if b > a => format!("{}{}", &text[..a], text[b + T_END.len()..].trim_start_matches('\n')),
            _ => text.to_string(),
        }
    }

    pub fn uninstall() {
        for dir in [data().join("nautilus/scripts"), config().join("caja/scripts")] {
            for n in [SEND, PASTE, CLIP] { let _ = fs::remove_file(dir.join(n)); }
        }
        for f in ["lanclip-send", "lanclip-paste", "lanclip-paste-into", "lanclip-clip"] {
            let _ = fs::remove_file(data().join("nemo/actions").join(format!("{f}.nemo_action")));
        }
        for f in ["lanclip-send", "lanclip-paste"] {
            let _ = fs::remove_file(data().join("kio/servicemenus").join(format!("{f}.desktop")));
        }
        if let Ok(t) = fs::read_to_string(thunar()) { let _ = fs::write(thunar(), strip_thunar(&t)); }
        let _ = fs::remove_file(launcher_file());
        let _ = fs::remove_file(icon_file());
        let target = install_target();
        if std::env::current_exe().map(|e| same_file(&e, &target)).unwrap_or(false) {
            let _ = fs::remove_file(target);
        }
    }

    pub fn set_autostart(exe: &Path, enabled: bool) {
        if !enabled { let _ = fs::remove_file(autostart_file()); return; }
        let _ = fs::create_dir_all(autostart_file().parent().unwrap());
        let _ = fs::write(autostart_file(), format!("[Desktop Entry]\nType=Application\nName=Lanclip\nComment=Share clipboard and files on your LAN\nExec={} daemon\nIcon=lanclip\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n", sh_quote(&exe.display().to_string())));
    }
}
