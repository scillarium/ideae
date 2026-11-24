//! The app in the desktop: a launcher entry pinned to the GNOME dash, kept
//! pointing at the binary when it moves, notifications, and restarting. On
//! Windows there is no dash to pin to.

use std::path::PathBuf;

use crate::state::APP_ID;

fn desktop_id() -> String {
    format!("{APP_ID}.desktop")
}

const ICON: &str = include_str!("../assets/icon/ideae.svg");

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn desktop_file() -> Option<PathBuf> {
    Some(data_home()?.join("applications").join(desktop_id()))
}

/// The running binary; after it was replaced on disk Linux reports the old
/// path with " (deleted)", and the new file is there under the plain name.
pub fn exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let s = exe.to_string_lossy();
    Ok(match s.strip_suffix(" (deleted)") {
        Some(plain) => PathBuf::from(plain),
        None => exe,
    })
}

fn entry(exe: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nComment=IDE in the agentic era\nExec=\"{}\" %F\nIcon={APP_ID}\nTerminal=false\nCategories=Development;\nStartupWMClass={APP_ID}\n",
        exe.display(),
        name = if cfg!(debug_assertions) { "ideae (dev)" } else { "ideae" },
    )
}

fn favorites() -> Option<Vec<String>> {
    let out = crate::procs::command("gsettings").args(["get", "org.gnome.shell", "favorite-apps"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let list = text.trim().trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']');
    Some(list.split(',').map(|s| s.trim().trim_matches('\'').to_string()).filter(|s| !s.is_empty()).collect())
}

fn set_favorites(list: &[String]) {
    let value = format!("[{}]", list.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "));
    let _ = crate::procs::command("gsettings").args(["set", "org.gnome.shell", "favorite-apps", &value]).status();
}

/// A desktop notification. On Windows a balloon from PowerShell, which needs
/// no app registration as a toast does.
#[cfg_attr(all(unix, not(target_os = "macos")), allow(dead_code))]
pub fn notify(title: &str, body: &str) {
    #[cfg(windows)]
    let child = {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.Icon]::ExtractAssociatedIcon({exe}); $n.Visible = $true; \
             $n.ShowBalloonTip(5000, {title}, {body}, 'None'); Start-Sleep 6; $n.Dispose()",
            exe = quote(&exe().map(|e| e.display().to_string()).unwrap_or_default()),
            title = quote(title),
            body = quote(body),
        );
        crate::procs::command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).spawn()
    };
    // As the app's own where it can be; else by AppleScript, as Script Editor's.
    #[cfg(target_os = "macos")]
    let child = {
        let (again_title, again_body) = (title.to_string(), body.to_string());
        if crate::mac_notify::send(title, body, None, move || {
            let _ = osascript_notification(&again_title, &again_body).spawn().map(|mut child| child.wait());
        }) {
            return;
        }
        osascript_notification(title, body).spawn()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let child = crate::procs::command("notify-send").args(["-a", APP_ID, title, body]).spawn();
    // Waited for off the UI thread, or each one stays a zombie.
    if let Ok(mut child) = child {
        std::thread::spawn(move || child.wait());
    }
}

/// A notification that runs `on_click` when it is clicked. On Linux that is
/// notify-send's default action, waited for on a thread of its own; a
/// Windows balloon reports no clicks, so there it is a plain one.
pub fn notify_with(title: &str, body: &str, on_click: impl FnOnce() + Send + 'static) {
    // A Windows balloon reports no clicks.
    #[cfg(windows)]
    {
        drop(on_click);
        notify(title, body);
    }
    // The app's own notification comes back when clicked; one sent by
    // AppleScript (from a process that is no app: the daemon) does not.
    #[cfg(target_os = "macos")]
    {
        let (again_title, again_body) = (title.to_string(), body.to_string());
        let refused = move || {
            let _ = osascript_notification(&again_title, &again_body).spawn().map(|mut child| child.wait());
        };
        if !crate::mac_notify::send(title, body, Some(Box::new(on_click)), refused) {
            notify(title, body);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let child = crate::procs::command("notify-send")
            .args(["-a", APP_ID, "-A", "default=Open", title, body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(child) = child {
            std::thread::spawn(move || {
                if let Ok(out) = child.wait_with_output() {
                    if String::from_utf8_lossy(&out.stdout).trim() == "default" {
                        on_click();
                    }
                }
            });
        }
    }
}

/// Starts a window on the task at `path` (from a notification the daemon
/// raised while no window was open).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn open_window_on(path: &std::path::Path) {
    if let Ok(exe) = exe() {
        if let Ok(mut child) = crate::procs::command(exe).arg("--task").arg(path).spawn() {
            std::thread::spawn(move || child.wait());
        }
    }
}

/// `display notification` from AppleScript: the strings go in as
/// arguments, so nothing in them is read as script.
#[cfg(target_os = "macos")]
fn osascript_notification(title: &str, body: &str) -> std::process::Command {
    let mut command = crate::procs::command("osascript");
    command
        .args(["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)", "-e", "end run"])
        .args([title, body]);
    command
}

pub fn can_pin() -> bool {
    cfg!(target_os = "linux")
}

/// How the app stands with the desktop: listed with the applications, and,
/// where there is a GNOME dash, pinned to it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Standing {
    pub listed: bool,
    pub dash: bool,
    pub pinned: bool,
}

/// Whether there is anything to do for the app to be listed: on macOS
/// the .app in Applications is all it takes.
pub fn can_list() -> bool {
    cfg!(any(target_os = "linux", windows))
}

/// Asks the desktop; takes a process on GNOME, so not on the UI thread.
pub fn standing() -> Standing {
    #[cfg(target_os = "linux")]
    {
        let favorites = favorites();
        return Standing {
            listed: desktop_file().is_some_and(|file| file.is_file()),
            dash: favorites.is_some(),
            pinned: favorites.is_some_and(|list| list.iter().any(|s| *s == desktop_id())),
        };
    }
    #[cfg(windows)]
    {
        return Standing { listed: start_menu_link().is_some_and(|link| link.is_file()), dash: false, pinned: false };
    }
    #[allow(unreachable_code)]
    Standing { listed: true, dash: false, pinned: false }
}

/// Writes the launcher entry and icon, and adds it to the dash.
pub fn pin() -> Result<(), String> {
    write_entry()?;
    let mut list = favorites().ok_or("the GNOME dash is not available")?;
    if !list.iter().any(|s| *s == desktop_id()) {
        list.push(desktop_id().into());
        set_favorites(&list);
    }
    Ok(())
}

/// Writes the list back only when the app was in it: a list read from
/// elsewhere (another session's settings) never replaces the user's.
pub fn unpin() {
    if let Some(mut list) = favorites().filter(|list| list.contains(&desktop_id())) {
        list.retain(|s| *s != desktop_id());
        set_favorites(&list);
    }
}

fn icon_file() -> Option<PathBuf> {
    Some(data_home()?.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")))
}

fn write_entry() -> Result<(), String> {
    let exe = exe().map_err(|e| e.to_string())?;
    let file = desktop_file().ok_or("no home directory")?;
    crate::state::write_atomic(&file, entry(&exe).as_bytes()).map_err(|e| e.to_string())?;
    if let Some(icon) = icon_file() {
        if crate::state::write_atomic(&icon, ICON.as_bytes()).is_ok() {
            refresh_icon_cache();
        }
    }
    Ok(())
}

/// GTK trusts icon-theme.cache while it is newer than the theme folder
/// itself, and a file added to scalable/apps leaves that folder's time
/// alone: without a fresh cache the dash keeps its generic icon.
fn refresh_icon_cache() {
    let Some(theme) = data_home().map(|d| d.join("icons/hicolor")) else { return };
    if !theme.join("icon-theme.cache").exists() {
        return;
    }
    let updated = crate::procs::command("gtk-update-icon-cache")
        .args(["-q", "-f", "-t"])
        .arg(&theme)
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !updated {
        let _ = std::fs::remove_file(theme.join("icon-theme.cache"));
    }
}

/// `ideae --install`, run by the install scripts: what makes the app one the
/// system lists and lets you pin. On Linux a launcher entry and its icon;
/// on Windows a Start menu shortcut; on macOS the .app in Applications is
/// all it takes.
pub fn install() -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        write_entry()?;
        return Ok(format!("Listed with the applications: {}", desktop_file().map(|f| f.display().to_string()).unwrap_or_default()));
    }
    #[cfg(windows)]
    {
        let (exe, link) = (exe().map_err(|e| e.to_string())?, start_menu_link().ok_or("no Start menu folder")?);
        let script = "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:IDEAE_LINK); $s.TargetPath = $env:IDEAE_EXE; \
                      $s.WorkingDirectory = Split-Path $env:IDEAE_EXE; $s.IconLocation = \"$env:IDEAE_EXE,0\"; $s.Save()";
        let done = crate::procs::command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("IDEAE_LINK", &link)
            .env("IDEAE_EXE", &exe)
            .status()
            .map_err(|e| e.to_string())?;
        return if done.success() { Ok(format!("In the Start menu: {}", link.display())) } else { Err("the shortcut was not made".into()) };
    }
    #[allow(unreachable_code)]
    Ok("Nothing to do: the app is where the system looks for apps.".into())
}

/// `ideae --uninstall`: the entry or shortcut, and the program the install
/// script put in place. Projects, tasks and settings stay where they are.
pub fn uninstall() -> Result<String, String> {
    let exe = exe().map_err(|e| e.to_string())?;
    let kept = format!("Settings and tasks are kept in {}.", crate::state::data_dir().display());
    #[cfg(target_os = "linux")]
    {
        unpin();
        for file in [desktop_file(), icon_file()].into_iter().flatten() {
            let _ = std::fs::remove_file(file);
        }
        refresh_icon_cache();
        std::fs::remove_file(&exe).map_err(|e| e.to_string())?;
        return Ok(format!("Removed {} and its launcher entry. {kept}", exe.display()));
    }
    #[cfg(windows)]
    {
        if let Some(link) = start_menu_link() {
            let _ = std::fs::remove_file(link);
        }
        // A running program cannot be deleted: a moment after it is gone.
        let script = "Start-Sleep 2; Remove-Item -Force $env:IDEAE_EXE; Remove-Item -Force ($env:IDEAE_EXE -replace '\\.exe$', '.old.exe') -ErrorAction SilentlyContinue; \
                      $dir = Split-Path $env:IDEAE_EXE; if (-not (Get-ChildItem $dir)) { Remove-Item $dir }";
        crate::procs::command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
            .env("IDEAE_EXE", &exe)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(format!("Removed {} and its shortcut. {kept}", exe.display()));
    }
    #[cfg(target_os = "macos")]
    {
        let app = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).ok_or("not run from an .app")?.to_path_buf();
        std::fs::remove_dir_all(&app).map_err(|e| e.to_string())?;
        return Ok(format!("Removed {}. {kept}", app.display()));
    }
    #[allow(unreachable_code)]
    Err(format!("nothing to remove for {}", exe.display()))
}

#[cfg(windows)]
fn start_menu_link() -> Option<PathBuf> {
    let name = if cfg!(debug_assertions) { "ideae (dev).lnk" } else { "ideae.lnk" };
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Microsoft/Windows/Start Menu/Programs").join(name))
}

/// An entry written earlier points at where the binary was then, and its
/// icon is the one of that build: follow both.
pub fn refresh_entry() {
    // A run on a scratch state is a test, often of a build in some other
    // worktree: the launcher keeps pointing at the user's own.
    if !can_pin() || std::env::var_os("IDEAE_STATE").is_some() {
        return;
    }
    let (Some(file), Ok(exe)) = (desktop_file(), exe()) else { return };
    if let Ok(current) = std::fs::read_to_string(&file) {
        let stale_icon = icon_file().is_some_and(|icon| std::fs::read_to_string(icon).is_ok_and(|old| old != ICON));
        if (current != entry(&exe) || stale_icon) && current.contains(&format!("StartupWMClass={APP_ID}\n")) {
            let _ = write_entry();
        }
    }
}

/// Starts a new instance with the same arguments; the caller quits. It
/// waits a little: off the UI thread.
pub fn spawn_again() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(app) = linked_bundle() {
        let front = front_app();
        let mut open = crate::procs::command("open");
        open.arg("-n");
        // Launch Services starts the app with its own environment, not ours.
        for (name, value) in std::env::vars().filter(|(name, _)| name.starts_with("IDEAE_")) {
            open.arg("--env").arg(format!("{name}={value}"));
        }
        let status = open.arg(app).arg("--args").args(std::env::args_os().skip(1)).status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("open could not start the app ({status})")));
        }
        // This instance gone before the new one is in front, macOS gives the
        // front to another app, and the new window then may not take it.
        let began = std::time::Instant::now();
        while front.is_some() && front_app() == front && began.elapsed() < std::time::Duration::from_secs(3) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        return Ok(());
    }
    crate::procs::command(exe()?).args(std::env::args_os().skip(1)).spawn().map(|_| ())
}

/// The app in front, as Launch Services names it (`ASN:0x0-0x98d98d:`).
#[cfg(target_os = "macos")]
fn front_app() -> Option<String> {
    let out = crate::procs::command("lsappinfo").arg("front").output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The .app this process was opened from when its binary is a symlink out
/// of the bundle (scripts/macos-app.sh --link): started directly, the
/// binary would be a second, iconless app in the Dock.
#[cfg(target_os = "macos")]
fn linked_bundle() -> Option<PathBuf> {
    let out = crate::procs::command("lsappinfo")
        .args(["info", "-only", "bundlepath", &std::process::id().to_string()])
        .output()
        .ok()?;
    let app = bundle_path(&String::from_utf8_lossy(&out.stdout))?;
    let inside = exe().is_ok_and(|exe| exe.starts_with(&app));
    (!inside && app.join("Contents/Info.plist").is_file()).then_some(app)
}

#[cfg(any(target_os = "macos", test))]
fn bundle_path(lsappinfo: &str) -> Option<PathBuf> {
    let path = lsappinfo.split_once("bundle path=\"")?.1.split_once("\"\n")?.0;
    path.ends_with(".app").then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_path_from_lsappinfo() {
        let out = "[ NULL ]  ASN:0x0-0x1: \n    bundleID=[ NULL ] \n    bundle path=\"/Applications/my \"x\" ideae.app\"\n    executable path=[ NULL ] \n";
        assert_eq!(bundle_path(out), Some(PathBuf::from("/Applications/my \"x\" ideae.app")));
        assert_eq!(bundle_path("    bundle path=[ NULL ] \n"), None);
        assert_eq!(bundle_path("    bundle path=\"/usr/bin/thing\"\n"), None);
    }
}
