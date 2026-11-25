//! The process table, read without a window.

/// A process's name, such as `bash`.
#[cfg(target_os = "linux")]
pub fn name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|n| n.trim().to_string())
}

#[cfg(target_os = "macos")]
pub fn name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: proc_name writes at most the buffer's size.
    let n = unsafe { libc::proc_name(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn name(_pid: u32) -> Option<String> {
    None
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listening {
    pub port: u16,
    pub pid: i32,
    pub name: String,
}

/// The ports `pids` listen on, lowest first.
#[cfg(target_os = "linux")]
pub fn listening(pids: &[i32]) -> Vec<Listening> {
    // inode → port, from the listening sockets of the whole machine.
    let mut ports: std::collections::HashMap<String, u16> = std::collections::HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() > 9 && f[3] == "0A" {
                if let Some(port) = f[1].rsplit(':').next().and_then(|h| u16::from_str_radix(h, 16).ok()) {
                    ports.insert(format!("socket:[{}]", f[9]), port);
                }
            }
        }
    }
    let mut out: Vec<Listening> = Vec::new();
    for &pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(link) = std::fs::read_link(fd.path()) else { continue };
            if let Some(&port) = ports.get(link.to_string_lossy().as_ref()) {
                if !out.iter().any(|l| l.port == port) {
                    let name = name(pid as u32).unwrap_or_default();
                    out.push(Listening { port, pid, name });
                }
            }
        }
    }
    out.sort_by_key(|l| l.port);
    out
}

/// `lsof` for the ports of `pids`.
#[cfg(target_os = "macos")]
pub fn listening(pids: &[i32]) -> Vec<Listening> {
    if pids.is_empty() {
        return Vec::new();
    }
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    let Ok(out) = std::process::Command::new("lsof")
        .args(["-a", "-p", &list.join(","), "-iTCP", "-sTCP:LISTEN", "-P", "-n", "-Fpcn"])
        .output()
    else {
        return Vec::new();
    };
    let mut found: Vec<Listening> = Vec::new();
    let (mut pid, mut name) = (0, String::new());
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.split_at_checked(1) {
            Some(("p", v)) => pid = v.parse().unwrap_or(0),
            Some(("c", v)) => name = v.to_string(),
            // "n*:3000" or "n127.0.0.1:3000".
            Some(("n", v)) => {
                if let Some(port) = v.rsplit(':').next().and_then(|p| p.parse().ok()) {
                    if !found.iter().any(|l| l.port == port) {
                        found.push(Listening { port, pid, name: name.clone() });
                    }
                }
            }
            _ => {}
        }
    }
    found.sort_by_key(|l| l.port);
    found
}

#[cfg(not(unix))]
pub fn listening(_: &[i32]) -> Vec<Listening> {
    Vec::new()
}


/// The processes of the sessions `leaders` lead (a terminal's shell, a
/// program on pipes, all they started there): theirs, not another session's.
#[cfg(target_os = "linux")]
pub fn in_sessions(leaders: &[i32]) -> Vec<i32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        // After the name in parentheses: state, parent, group, session.
        let session = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(3)?.parse::<i32>().ok());
        if session.is_some_and(|s| leaders.contains(&s)) {
            found.push(pid);
        }
    }
    found
}

#[cfg(target_os = "macos")]
pub fn in_sessions(leaders: &[i32]) -> Vec<i32> {
    let mut pids = vec![0i32; 8192];
    // SAFETY: proc_listallpids writes at most the buffer's size in bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * std::mem::size_of::<i32>()) as i32) };
    pids.truncate(n.max(0) as usize);
    // SAFETY: getsid only reads the process table.
    pids.into_iter().filter(|&pid| pid > 0 && leaders.contains(&unsafe { libc::getsid(pid) })).collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn in_sessions(_: &[i32]) -> Vec<i32> {
    Vec::new()
}

/// The program a process runs, by its path: unlike its name, it can be read
/// of a process of another user, root's `login` among them.
#[cfg(target_os = "macos")]
pub fn path(pid: u32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath writes at most the buffer's size.
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    buf.truncate(n);
    Some(std::path::PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&buf)))
}

/// The name of the shell a terminal's session runs, `pid` being the
/// session's first process. On macOS that process is login(1), root's, whose
/// name cannot be read, and the shell is its child: a session handed over by
/// another daemon has nothing else to tell its shell by.
pub fn shell_of_session(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    if path(pid).is_some_and(|path| path.file_name().is_some_and(|name| name == "login")) {
        let mut children = [0 as libc::pid_t; 8];
        // SAFETY: proc_listchildpids writes at most the buffer's size, and returns how many pids it wrote.
        let n = unsafe { libc::proc_listchildpids(pid as libc::pid_t, children.as_mut_ptr().cast(), std::mem::size_of_val(&children) as libc::c_int) };
        let count = usize::try_from(n).unwrap_or(0).min(children.len());
        return children.get(..count).and_then(|found| found.first()).and_then(|&child| name(child as u32));
    }
    name(pid)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::os::unix::process::CommandExt as _;

    /// A session whose first process is a login (by its path): its shell
    /// is its child.
    #[test]
    fn a_session_under_login_is_told_its_shell() {
        let dir = std::env::temp_dir().join(format!("ideae-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A stand-in for login(1): bash under that name, with sleep as its child.
        let login = dir.join("login");
        std::fs::copy("/bin/bash", &login).unwrap();
        // A copy of a system binary keeps Apple's signature, which no longer
        // fits it: macOS kills it at the start. Signed anew, it runs.
        let signed = std::process::Command::new("codesign").args(["-s", "-", "-f"]).arg(&login).output().unwrap();
        assert!(signed.status.success(), "codesign: {}", String::from_utf8_lossy(&signed.stderr));
        // A group of its own: the sleep goes with it at the end.
        let mut child = std::process::Command::new(&login).args(["-c", "sleep 30 & wait"]).process_group(0).spawn().unwrap();
        let mut told = None;
        for _ in 0..100 {
            told = super::shell_of_session(child.id());
            if told.as_deref() == Some("sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // SAFETY: a signal to the group this test started.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(told.as_deref(), Some("sleep"));
        assert_eq!(super::shell_of_session(std::process::id()), super::name(std::process::id()), "no login: its own name");
    }

    /// The real login is root's, whose name a user's process cannot read:
    /// its path can be. launchd, root's too, stands for it.
    #[test]
    fn a_root_process_is_told_by_its_path() {
        assert_eq!(super::path(1).as_deref(), Some(std::path::Path::new("/sbin/launchd")));
    }
}
