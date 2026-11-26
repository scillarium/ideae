//! Starting programs and ending what the app started when it quits. Closing
//! a terminal ends its session on a background thread, which a quitting app
//! does not wait for.

#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

pub use ideae_core::procs::{command, spawn_tied, which};

/// What ideae-core asks of the app: the job its children go in on Windows.
pub fn tie_children() {
    #[cfg(windows)]
    let _ = ideae_core::procs::TIE.set(win::tie);
}

/// Runs `job` on a thread of its own and gives what it returns. For what
/// waits long on something else (a language server, the network, a test
/// run): the background pool has a thread per core, and a few of those
/// waiting would hold up every save, search and refresh behind them.
pub fn on_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(job());
    });
    async move { rx.recv().await.expect("the job's thread ended without a result") }
}


/// Started from the Dock or a launcher, the app has the session's bare
/// environment: none of what the user's shell profile sets (PATH with go,
/// cargo and node in it, JAVA_HOME, ANDROID_HOME), so language servers and
/// the builds they run are not found or fail. This takes those variables
/// from a login shell. Variables the app already has stay; PATH gets the
/// shell's directories first. Call before any thread starts.
#[cfg(unix)]
pub fn adopt_shell_env() {
    use std::io::Read as _;
    // Started from a terminal: the environment is the shell's already.
    if std::env::var_os("TERM").is_some() || std::env::var_os("IDEAE_NO_SHELL_ENV").is_some() {
        return;
    }
    let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) else { return };
    const MARK: &str = "__ideae_env__";
    let Ok(mut child) = Command::new(shell)
        .args(["-l", "-i", "-c", &format!("printf {MARK}; env -0; printf {MARK}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    // A profile that waits for input or the network must not hold the window.
    let out = rx.recv_timeout(Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    let Ok(out) = out else { return };
    for (name, value) in shell_env_changes(&String::from_utf8_lossy(&out), MARK, |name| std::env::var(name).ok()) {
        // SAFETY: main calls this before it starts any thread; the reader above has ended.
        unsafe { std::env::set_var(name, value) };
    }
}

#[cfg(not(unix))]
pub fn adopt_shell_env() {}

/// The variables to set from `env -0` output between two marks: those
/// missing here, and PATH with the shell's directories ahead of ours.
#[cfg(unix)]
fn shell_env_changes(out: &str, mark: &str, current: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut parts = out.split(mark);
    let (Some(_), Some(env), Some(_)) = (parts.next(), parts.next(), parts.next()) else { return vec![] };
    env.split('\0')
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| !matches!(*name, "_" | "PWD" | "OLDPWD" | "SHLVL" | "TERM"))
        .filter_map(|(name, value)| match current(name) {
            None => Some((name.to_string(), value.to_string())),
            Some(ours) if name == "PATH" => {
                let mut dirs: Vec<&str> = value.split(':').filter(|d| !d.is_empty()).collect();
                for dir in ours.split(':').filter(|d| !d.is_empty()) {
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
                let path = dirs.join(":");
                (path != ours).then(|| (name.to_string(), path))
            }
            Some(_) => None,
        })
        .collect()
}

#[cfg(all(test, unix))]
mod shell_env_tests {
    #[test]
    fn takes_what_is_missing_and_merges_path() {
        let out = "motd\nMARKPATH=/opt/bin:/usr/bin\0HOME=/other\0ANDROID_HOME=/sdk\0MULTI=a\nb\0SHLVL=2\0MARK";
        let current = |name: &str| match name {
            "PATH" => Some("/usr/bin:/bin".to_string()),
            "HOME" => Some("/home/me".to_string()),
            _ => None,
        };
        let changes = super::shell_env_changes(out, "MARK", current);
        let expected = [("PATH", "/opt/bin:/usr/bin:/bin"), ("ANDROID_HOME", "/sdk"), ("MULTI", "a\nb")];
        assert_eq!(changes, expected.map(|(n, v)| (n.to_string(), v.to_string())));
        assert!(super::shell_env_changes("no marks", "MARK", current).is_empty());
    }
}

/// Whether a process with this id runs.
pub fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    return running(pid as i32);
    #[cfg(windows)]
    return win::alive(pid);
}

#[cfg(windows)]
mod win {
    use std::sync::OnceLock;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, STILL_ACTIVE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            },
            Diagnostics::Debug::ReadProcessMemory,
            Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
            },
        },
    };

    struct Job(HANDLE);
    // SAFETY: a job handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    /// One job for the app's whole life; its handle closes only when the
    /// process ends, and then everything in it is killed.
    fn job() -> Option<&'static Job> {
        static JOB: OnceLock<Option<Job>> = OnceLock::new();
        JOB.get_or_init(|| {
            // SAFETY: null attributes and name create an anonymous job; the
            // limits are plain data of the size given.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                );
                Some(Job(handle))
            }
        })
        .as_ref()
    }

    pub fn tie(child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle as _;
        if let Some(job) = job() {
            // SAFETY: both handles are open.
            unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
        }
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut core::ffi::c_void, size: u32, written: *mut u32) -> i32;
    }

    /// The environment as the process was started with it, as UTF-8 with
    /// NULs between the variables. Windows keeps it in the process's own
    /// memory: the PEB points at the process parameters, and those at the
    /// block and its size (the offsets are those of 64-bit Windows).
    #[cfg(target_pointer_width = "64")]
    pub fn environ(pid: u32) -> Option<Vec<u8>> {
        const PARAMETERS_IN_PEB: usize = 0x20;
        const ENVIRONMENT_IN_PARAMETERS: usize = 0x80;
        const ENVIRONMENT_SIZE_IN_PARAMETERS: usize = 0x3f0;
        const MOST: usize = 1 << 20;
        // SAFETY: the handle is checked and closed here; every read is into
        // a buffer of the size asked for, and checked.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
            if handle.is_null() {
                return None;
            }
            let read = |address: usize, into: *mut core::ffi::c_void, size: usize| {
                let mut got = 0usize;
                ReadProcessMemory(handle, address as *const _, into, size, &mut got) != 0 && got == size
            };
            let pointer = |address: usize| {
                let mut value = 0usize;
                read(address, (&raw mut value).cast(), std::mem::size_of::<usize>()).then_some(value)
            };
            let block = (|| {
                // PROCESS_BASIC_INFORMATION: the PEB's address is its second field.
                let mut basic = [0usize; 6];
                if NtQueryInformationProcess(handle, 0, basic.as_mut_ptr().cast(), std::mem::size_of_val(&basic) as u32, std::ptr::null_mut()) != 0 {
                    return None;
                }
                let parameters = pointer(basic[1] + PARAMETERS_IN_PEB)?;
                let environment = pointer(parameters + ENVIRONMENT_IN_PARAMETERS)?;
                let size = pointer(parameters + ENVIRONMENT_SIZE_IN_PARAMETERS)?.min(MOST) & !1;
                let mut wide = vec![0u16; size / 2];
                read(environment, wide.as_mut_ptr().cast(), size).then_some(wide)
            })();
            CloseHandle(handle);
            Some(String::from_utf16_lossy(&block?).into_bytes())
        }
    }

    #[cfg(not(target_pointer_width = "64"))]
    pub fn environ(_: u32) -> Option<Vec<u8>> {
        None
    }

    pub fn alive(pid: u32) -> bool {
        // SAFETY: the handle is checked and closed here.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE as u32
        }
    }
}

#[cfg(target_os = "linux")]
fn pids() -> Vec<i32> {
    std::fs::read_dir("/proc")
        .map(|dir| dir.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn ppid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count; the second call fills one
    // of the size given.
    unsafe {
        let n = libc::proc_listallpids(std::ptr::null_mut(), 0).max(0) as usize;
        let mut pids = vec![0i32; n + 64];
        let size = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
        let got = libc::proc_listallpids(pids.as_mut_ptr().cast(), size).max(0) as usize;
        pids.truncate(got);
        pids
    }
}

/// The short form, which macOS gives for other users' processes too:
/// a terminal there is started through root's `login`.
#[cfg(target_os = "macos")]
fn bsd_info(pid: i32) -> Option<libc::proc_bsdshortinfo> {
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 0, (&raw mut info).cast(), size) };
    (got == size).then_some(info)
}

#[cfg(target_os = "macos")]
fn ppid(pid: i32) -> Option<i32> {
    bsd_info(pid).map(|i| i.pbsi_ppid as i32)
}

/// Every process that runs (no zombies), with its parent and name.
#[cfg(target_os = "macos")]
pub fn running_with_parents() -> Vec<(i32, i32, String)> {
    pids()
        .into_iter()
        .filter_map(|pid| {
            let info = bsd_info(pid).filter(|i| i.pbsi_status != 5)?;
            let short = || info.pbsi_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect();
            Some((pid, info.pbsi_ppid as i32, name(pid as u32).unwrap_or_else(short)))
        })
        .collect()
}

/// Whether a process has a shell of its own that runs: an agent's command
/// in the background (Claude Code runs each in a shell it starts), which
/// would end with it. Its servers (MCP) are no shells. Not known: yes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn runs_a_shell(pid: u32) -> bool {
    const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "nu"];
    let pid = pid as i32;
    pids().into_iter().filter(|&p| ppid(p) == Some(pid) && running(p)).any(|p| name(p as u32).is_some_and(|n| SHELLS.contains(&n.as_str())))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn runs_a_shell(_: u32) -> bool {
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use ideae_core::procs::name;

#[cfg(unix)]
pub fn sid(pid: i32) -> i32 {
    // SAFETY: getsid only reads the process table.
    unsafe { libc::getsid(pid) }
}

#[cfg(target_os = "linux")]
fn running(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(')').map(|(_, rest)| !rest.trim_start().starts_with('Z')))
        .unwrap_or(false)
}

/// Running and no zombie (status SZOMB is 5).
#[cfg(target_os = "macos")]
fn running(pid: i32) -> bool {
    bsd_info(pid).is_some_and(|i| i.pbsi_status != 5)
}

/// Every session led by a child of this process (the terminals' shells):
/// the shell, the agent, and whatever they started, nohup included. On
/// Windows each terminal's tree is in a job that dies with the app.
#[cfg(not(unix))]
pub fn end_child_sessions() {}

/// A process running `ideae --daemon`.
#[cfg(target_os = "linux")]
fn is_daemon(pid: i32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.split(|&b| b == 0).any(|arg| arg == b"--daemon"))
}

/// The window knows its daemon's pid from the socket.
#[cfg(target_os = "macos")]
fn is_daemon(pid: i32) -> bool {
    crate::daemon::client::get().and_then(|d| d.pid()) == Some(pid)
}

#[cfg(unix)]
pub fn end_child_sessions() {
    let me = std::process::id() as i32;
    // The daemon leads a session too, and its terminals outlive the window
    // by design; it ends them itself when told to.
    let daemon = |p: i32| {
        is_daemon(p)
    };
    let leaders: Vec<i32> =
        pids().into_iter().filter(|&p| ppid(p) == Some(me) && sid(p) == p && !daemon(p)).collect();
    if leaders.is_empty() {
        return;
    }
    let members = || -> Vec<i32> {
        pids().into_iter().filter(|&p| leaders.contains(&sid(p)) && running(p)).collect()
    };
    for (signal, grace) in [(libc::SIGHUP, 300), (libc::SIGTERM, 300), (libc::SIGKILL, 0)] {
        let alive = members();
        if alive.is_empty() {
            return;
        }
        for pid in alive {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(pid, signal) };
        }
        let deadline = Instant::now() + Duration::from_millis(grace);
        while Instant::now() < deadline && !members().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A process's environment, NUL-separated.
#[cfg(windows)]
pub fn environ(pid: i32) -> Option<Vec<u8>> {
    win::environ(pid as u32)
}

/// A process's environment, NUL-separated.
#[cfg(target_os = "linux")]
pub fn environ(pid: i32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/environ")).ok()
}

/// From KERN_PROCARGS2: argc, the executable's path, NUL padding, the
/// arguments, then the environment. For the system's own programs
/// (/bin/zsh, /bin/sleep) macOS leaves the environment out: empty then.
#[cfg(target_os = "macos")]
pub fn environ(pid: i32) -> Option<Vec<u8>> {
    let mut size: libc::size_t = 0;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: a null buffer asks for the size; the second call fills one of it.
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
    }
    let mut buf = vec![0u8; size];
    // SAFETY: as above, with the buffer `size` long.
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
    }
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let mut at = 4 + buf[4..].iter().position(|&b| b == 0)?;
    while buf.get(at) == Some(&0) {
        at += 1;
    }
    for _ in 0..argc {
        at += buf.get(at..)?.iter().position(|&b| b == 0)? + 1;
    }
    let env = &buf[at.min(buf.len())..];
    let end = env.windows(2).position(|w| w == [0, 0]).map_or(env.len(), |i| i + 1);
    Some(env[..end].to_vec())
}

/// Every process started for a task: with this app's `socket` and, if
/// given, the task's root in its environment, wherever it went (a session
/// of its own included). Another ideae started from a task's terminal
/// inherits the same, so the trees of ideae processes are left out.
///
/// Not every process shows its environment: macOS keeps back that of its
/// own programs (zsh, sleep), and root's (a terminal's `login`) cannot be
/// read. Such a process goes by its family: what it started, else what
/// started it, nearest first.
#[cfg(unix)]
pub fn task_processes(task: Option<&str>, socket: &str) -> Vec<i32> {
    use std::collections::HashMap;
    let me = std::process::id() as i32;
    let daemon = crate::daemon::client::get().and_then(|d| d.pid()).map(|p| p as i32);
    let sock = format!("{}={socket}", crate::agent::SOCK_ENV);
    let task = task.map(|t| format!("{}={t}", crate::agent::TASK_ENV));

    let running: Vec<i32> = pids().into_iter().filter(|&p| running(p)).collect();
    let parents: HashMap<i32, i32> = running.iter().filter_map(|&p| Some((p, ppid(p)?))).collect();
    // Whether its environment names the task; None when it shows none.
    let mut shown: HashMap<i32, Option<bool>> = HashMap::new();
    let mut own = |pid: i32| {
        *shown.entry(pid).or_insert_with(|| {
            let env = environ(pid).filter(|env| !env.is_empty())?;
            let vars: Vec<&[u8]> = env.split(|&b| b == 0).collect();
            Some(vars.contains(&sock.as_bytes()) && task.as_ref().is_none_or(|t| vars.contains(&t.as_bytes())))
        })
    };
    // Hidden, a process is the task's when its nearest ancestor that shows
    // its environment is: never by what it started, nor by its siblings. The
    // user's systemd hides its environment, and every orphan of the session
    // is its child: judged by them, it was taken for the task's and ended,
    // and the whole session with it.
    let mut tagged = |pid: i32| {
        let mut at = pid;
        for _ in 0..64 {
            match own(at) {
                Some(tagged) => return tagged,
                None => match parents.get(&at) {
                    Some(&parent) if parent > 1 && parent != me && Some(parent) != daemon => at = parent,
                    _ => return false,
                },
            }
        }
        false
    };
    let ideae = |pid: i32| name(pid as u32).is_some_and(|n| n.starts_with("ideae"));
    let in_other_app = |pid: i32| {
        let mut at = Some(pid);
        for _ in 0..64 {
            match at {
                Some(p) if p == me || Some(p) == daemon || p <= 1 => return false,
                Some(p) if ideae(p) => return true,
                Some(p) => at = parents.get(&p).copied(),
                None => return false,
            }
        }
        false
    };
    running.iter().copied().filter(|&p| p != me && Some(p) != daemon && !protected(p) && tagged(p) && !in_other_app(p)).collect()
}

/// What no task's ending may signal, whatever is found: init, the user's
/// service manager and session, and this process with its ancestors.
#[cfg(unix)]
fn protected(pid: i32) -> bool {
    if pid <= 1 {
        return true;
    }
    let mut at = Some(std::process::id() as i32);
    for _ in 0..64 {
        match at {
            Some(p) if p == pid => return true,
            Some(p) if p > 1 => at = ppid(p),
            _ => break,
        }
    }
    name(pid as u32).is_some_and(|n| matches!(n.as_str(), "systemd" | "init" | "gnome-session-binary" | "gnome-shell" | "dbus-daemon" | "dbus-broker" | "Xwayland" | "launchd" | "loginwindow"))
}

#[cfg(not(unix))]
pub fn task_processes(_: Option<&str>, _: &str) -> Vec<i32> {
    Vec::new()
}

/// Ends what a task started that left its terminals' sessions, such as a
/// shell an agent runs in the background with a session of its own.
/// `seen` is what [`task_processes`] found before the task's terminals
/// were ended: once its parent is gone, a process that shows no
/// environment is nobody's any more.
#[cfg(unix)]
pub fn end_task_strays(task: Option<&str>, socket: &str, seen: Vec<i32>) {
    // Not root's `login`, which ends with its terminal.
    let ours = |pid: &i32| !protected(*pid) && running(*pid) && unsafe { libc::kill(*pid, 0) } == 0;
    let alive = || {
        let mut all = task_processes(task, socket);
        all.extend(&seen);
        all.sort();
        all.dedup();
        all.retain(ours);
        all
    };
    for (signal, grace) in [(libc::SIGTERM, 500), (libc::SIGKILL, 0)] {
        let alive_now = alive();
        if alive_now.is_empty() {
            return;
        }
        for pid in alive_now {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(pid, signal) };
        }
        let deadline = Instant::now() + Duration::from_millis(grace);
        while Instant::now() < deadline && !alive().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// On Windows a terminal's tree is in a job that dies with it.
#[cfg(not(unix))]
pub fn end_task_strays(_: Option<&str>, _: &str, _: Vec<i32>) {}

/// Once everything is saved and ended at quit: what is left of the process
/// (the window's teardown, a driver's at exit) has a few seconds, then the
/// process ends anyway. A process that waited for good there kept its link
/// to the agents' daemon and their output, with no window.
pub fn end_if_stuck() {
    const GRACE: std::time::Duration = std::time::Duration::from_secs(5);
    if cfg!(test) {
        return;
    }
    let _ = std::thread::Builder::new().name("end-if-stuck".into()).spawn(|| {
        std::thread::sleep(GRACE);
        eprintln!("ideae: still not ended {}s after quitting, ending it", GRACE.as_secs());
        // No exit handlers: one of them may be what waits.
        #[cfg(unix)]
        // SAFETY: _exit ends the process at once; nothing of it runs after.
        unsafe {
            libc::_exit(0)
        };
        #[cfg(not(unix))]
        std::process::exit(0);
    });
}

#[cfg_attr(not(unix), allow(dead_code))]
static QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_quit_signal(_: libc::c_int) {
    QUIT.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// SIGTERM, SIGHUP and SIGINT ask the app to quit instead of killing it,
/// so it saves and ends its terminals' sessions first.
pub fn catch_quit_signals() {
    #[cfg(unix)]
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, on_quit_signal as *const () as libc::sighandler_t) };
    }
}

pub fn quit_requested() -> bool {
    QUIT.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(all(test, unix))]
mod stray_tests {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command};

    /// Stands in for an agent: started again by the test below with a task
    /// in its environment, it starts a system program in a session of its
    /// own, whose environment macOS keeps back.
    #[test]
    #[ignore]
    fn agent_with_a_background_sleep() {
        let mut command = Command::new("sleep");
        command.arg("30");
        // SAFETY: setsid in the child before exec, as a background shell does.
        unsafe { command.pre_exec(|| { libc::setsid(); Ok(()) }) };
        let _sleep = command.spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    /// Under another name than this program's: what an ideae started is
    /// another app's, and left alone.
    fn agent(task: &str, socket: &str) -> Child {
        let stand_in = std::env::temp_dir().join(format!("stray-agent-{}", std::process::id()));
        // A copy (a clone on APFS: no time, no room), not a link: a symlink
        // keeps the program's name, and a hard link is the build's own file,
        // whose start waits on whatever watches the build's folder (seen at
        // half a minute on a Mac, with every process the tests started after it).
        let this = std::env::current_exe().unwrap();
        if !stand_in.exists() {
            std::fs::copy(&this, &stand_in).unwrap();
        }
        // The copy just written is still open for writing in a child another
        // test forked meanwhile, until that child execs: "Text file busy".
        let started = std::time::Instant::now();
        loop {
            let spawned = Command::new(&stand_in)
                .args(["--exact", "procs::stray_tests::agent_with_a_background_sleep", "--ignored"])
                .env(crate::agent::TASK_ENV, task)
                .env(crate::agent::SOCK_ENV, socket)
                .stdout(std::process::Stdio::null())
                .spawn();
            match spawned {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && started.elapsed() < std::time::Duration::from_secs(5) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                spawned => return spawned.unwrap(),
            }
        }
    }

    /// An agent of `task` and its sleep, once it has started it. One that
    /// ends before (macOS may stop a binary just built, run under a new
    /// name, while it looks it over) is said and started again.
    fn agent_and_sleep(task: &str, socket: &str) -> (Child, i32) {
        for _ in 0..3 {
            let mut agent = agent(task, socket);
            let started = std::time::Instant::now();
            loop {
                if let Some(pid) = super::pids().into_iter().find(|&p| super::ppid(p) == Some(agent.id() as i32) && super::running(p)) {
                    return (agent, pid);
                }
                if let Ok(Some(status)) = agent.try_wait() {
                    eprintln!("the stand-in agent ended before its sleep ({status}): started again");
                    break;
                }
                assert!(started.elapsed() < std::time::Duration::from_secs(30), "the agent starts its sleep");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        panic!("the stand-in agent never started its sleep");
    }

    #[test]
    fn the_session_is_never_a_tasks() {
        assert!(super::protected(1) && super::protected(0) && super::protected(-1));
        assert!(super::protected(std::process::id() as i32), "this process");
        let parent = super::ppid(std::process::id() as i32).unwrap();
        assert!(super::protected(parent), "and what started it");
    }

    fn gone(pid: i32) -> bool {
        let started = std::time::Instant::now();
        while super::running(pid) && started.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        !super::running(pid)
    }

    #[test]
    fn ends_the_tasks_strays_and_only_them() {
        let socket = format!("/tmp/ideae-test-{}.sock", std::process::id());
        /// Whatever the test finds, nothing it started stays.
        struct Started(Vec<i32>);
        impl Drop for Started {
            fn drop(&mut self) {
                for &pid in &self.0 {
                    // SAFETY: kill only sends a signal, to what this test started.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                let _ = std::fs::remove_file(std::env::temp_dir().join(format!("stray-agent-{}", std::process::id())));
            }
        }
        let mut started = Started(Vec::new());
        let mut take = |task: &str, socket: &str| {
            let (agent, sleep) = agent_and_sleep(task, socket);
            started.0.extend([agent.id() as i32, sleep]);
            (agent, sleep)
        };
        let ((mut ours, a), (mut left, b), (mut other_task, c), (mut other_app, d)) = (take("/t/a", &socket), take("/t/a", &socket), take("/t/b", &socket), take("/t/a", "elsewhere"));
        let sleeps = [a, b, c, d];

        // One agent is gone before its sleep is looked for again, as when a
        // task's terminals are ended first.
        let seen = super::task_processes(Some("/t/a"), &socket);
        assert!(seen.contains(&sleeps[0]) && seen.contains(&sleeps[1]), "a sleep goes by the agent that started it");
        let _ = left.kill();
        let _ = left.wait();
        // Its sleep is an orphan now, a child of whatever adopts orphans
        // (the user's systemd): that one is not the task's for it.
        let found = super::task_processes(Some("/t/a"), &socket);
        assert!(found.iter().all(|&p| !super::protected(p)), "nothing of the session in {found:?}");
        assert!(found.iter().all(|&p| super::ppid(p) != Some(1) || p == sleeps[1]), "only the orphan itself");
        super::end_task_strays(Some("/t/a"), &socket, seen);

        assert!(ours.try_wait().unwrap().is_some() || gone(ours.id() as i32), "the task's agent is ended");
        assert!(gone(sleeps[0]), "and the sleep it started");
        assert!(gone(sleeps[1]), "and the sleep whose agent was gone already");
        assert!(other_task.try_wait().unwrap().is_none() && super::running(sleeps[2]), "another task's processes live on");
        assert!(other_app.try_wait().unwrap().is_none() && super::running(sleeps[3]), "another app's processes live on");
        drop(started);
        for child in [&mut ours, &mut other_task, &mut other_app] {
            let _ = child.wait();
        }
    }
}
