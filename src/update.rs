//! Updates for release builds: the latest GitHub release of the repository
//! this binary came from, downloaded next to the running binary and put in
//! its place. A build of a checkout: new commits of its remote's main branch,
//! pulled and built over the running binary, which the new-build notice then
//! offers to restart into.

use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub asset: String,
    pub url: String,
    pub size: u64,
}

/// The release this binary is: its tag, a UTC time `yyyymmddhhmmss`.
/// IDEAE_UPDATE_FROM pretends to be an older one, for tests.
fn current() -> Option<u64> {
    std::env::var("IDEAE_UPDATE_FROM").ok().or(option_env!("IDEAE_VERSION").map(str::to_string))?.parse().ok()
}

pub fn enabled() -> bool {
    current().is_some()
}

fn repo() -> Option<String> {
    let url = env!("CARGO_PKG_REPOSITORY");
    url.strip_prefix("https://github.com/").map(|r| r.trim_end_matches('/').to_string())
}

/// This system's binary in a release.
fn suffix() -> &'static str {
    if cfg!(windows) {
        "-windows-x86_64.exe"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "-macos-arm64"
    } else if cfg!(target_os = "macos") {
        "-macos-x86_64"
    } else {
        "-linux-x86_64"
    }
}

/// A newer release than this one, off the UI thread.
pub fn newer() -> Option<Release> {
    let current = current()?;
    let out = crate::procs::command("curl")
        .args(["-fsSL", "--max-time", "20", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{}/releases/latest", repo()?))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let release = parse(&serde_json::from_slice(&out.stdout).ok()?)?;
    (release.tag.parse::<u64>().ok()? > current).then_some(release)
}

fn parse(v: &Value) -> Option<Release> {
    let tag = v["tag_name"].as_str()?.to_string();
    let asset = v["assets"].as_array()?.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix())))?;
    Some(Release {
        tag,
        asset: asset["name"].as_str()?.to_string(),
        url: asset["browser_download_url"].as_str()?.to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
    })
}

/// "15.11 11:32" from a tag.
pub fn when(tag: &str) -> String {
    if tag.len() == 14 && tag.is_ascii() {
        format!("{}.{} {}:{}", &tag[6..8], &tag[4..6], &tag[8..10], &tag[10..12])
    } else {
        tag.to_string()
    }
}

/// Downloads `release` and puts it in place of the running binary.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let exe = crate::desktop::exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no directory")?;
    let download = dir.join(format!(".{}.download", release.asset));
    let status = crate::procs::command("curl")
        .args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&download)
        .arg(&release.url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    let size = std::fs::metadata(&download).map(|m| m.len()).unwrap_or(0);
    if !status.success() || (release.size > 0 && size != release.size) {
        let _ = std::fs::remove_file(&download);
        return Err(format!("the download failed ({size} of {} bytes)", release.size));
    }
    replace(&exe, &download)?;
    Ok(exe)
}

#[cfg(unix)]
fn replace(exe: &Path, new: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    // The running process keeps its copy; the path gets the new one.
    std::fs::rename(new, exe).map_err(|e| format!("cannot replace {}: {e}", exe.display()))
}

/// A running exe cannot be overwritten on Windows, but it can be renamed.
#[cfg(windows)]
fn replace(exe: &Path, new: &Path) -> Result<(), String> {
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("cannot move {} aside: {e}", exe.display()))?;
    std::fs::rename(new, exe).map_err(|e| {
        let _ = std::fs::rename(&old, exe);
        format!("cannot replace {}: {e}", exe.display())
    })
}

/// A build of a checkout: new commits on its remote's main branch, which a
/// click pulls and builds over the running binary.
#[derive(Clone, Debug, PartialEq)]
pub struct Behind {
    pub commits: usize,
    /// `origin/main`.
    pub base: String,
}

/// The checkout this binary was built from, while it is there. Release and
/// debug builds have none.
pub fn source() -> Option<PathBuf> {
    if enabled() || cfg!(debug_assertions) {
        return None;
    }
    let dir = PathBuf::from(std::env::var("IDEAE_UPDATE_SOURCE").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()));
    dir.join(".git").exists().then_some(dir)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::procs::command("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// `origin/main`: what the remote's HEAD names, else its `main`.
fn base(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).unwrap_or_else(|_| "origin/main".into())
}

/// Fetches and counts the base's commits the checkout lacks, off the UI thread.
pub fn behind(dir: &Path) -> Result<Option<Behind>, String> {
    git(dir, &["fetch", "--quiet", "origin"])?;
    let base = base(dir);
    let commits = git(dir, &["rev-list", "--count", &format!("HEAD..{base}")])?.parse().unwrap_or(0);
    Ok((commits > 0).then_some(Behind { commits, base }))
}

/// Why the checkout cannot be pulled as it is: not on the base's branch, or changed.
fn blocked(dir: &Path, base: &str) -> Option<String> {
    let branch = base.split_once('/').map_or(base, |(_, b)| b);
    let on = git(dir, &["branch", "--show-current"]).unwrap_or_default();
    if on != branch {
        return Some(format!("{} is on {}, not {branch}: switch it to {branch} to update.", dir.display(), if on.is_empty() { "no branch" } else { &on }));
    }
    let changed = git(dir, &["status", "--porcelain", "--untracked-files=no"]).unwrap_or_default();
    (!changed.is_empty()).then(|| format!("{} has uncommitted changes: commit or stash them to update.", dir.display()))
}

/// Where cargo puts the running binary: `…/target` of `…/target/release/ideae`.
fn target_dir(exe: &Path) -> Option<&Path> {
    let release = exe.parent()?;
    (release.file_name()? == "release").then(|| release.parent()).flatten()
}

fn run(cmd: &mut std::process::Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
    Err(format!("{what} failed:\n{}", lines[lines.len().saturating_sub(12)..].join("\n")))
}

/// Pulls the base into the checkout and builds it over the running binary;
/// the title bar's new-build notice then offers the restart.
pub fn rebuild(dir: &Path) -> Result<(), String> {
    let base = base(dir);
    if let Some(why) = blocked(dir, &base) {
        return Err(why);
    }
    git(dir, &["pull", "--ff-only", "--quiet"])?;
    let exe = crate::desktop::exe().map_err(|e| e.to_string())?;
    let mut cargo = crate::procs::command("cargo");
    cargo.current_dir(dir).args(["build", "--release"]);
    if let Some(target) = target_dir(&exe) {
        cargo.env("CARGO_TARGET_DIR", target);
    }
    run(&mut cargo, "cargo build --release")?;
    let sign = dir.join("scripts/macos-sign.sh");
    if cfg!(target_os = "macos") && sign.exists() {
        run(crate::procs::command(&sign).arg(&exe), "macos-sign.sh")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse, suffix, target_dir, when};
    use std::path::Path;

    #[test]
    fn picks_this_systems_asset() {
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"tag_name":"20261115113213","assets":[
                {{"name":"ideae-20261115113213-other","browser_download_url":"u0","size":1}},
                {{"name":"ideae-20261115113213{}","browser_download_url":"u1","size":7}}]}}"#,
            suffix()
        ))
        .unwrap();
        let r = parse(&v).unwrap();
        assert_eq!((r.url.as_str(), r.size), ("u1", 7));
        assert_eq!(when("20261115113213"), "15.11 11:32");
    }

    #[test]
    fn builds_where_the_running_binary_is() {
        assert_eq!(target_dir(Path::new("/w/ideae/target/release/ideae")), Some(Path::new("/w/ideae/target")));
        assert_eq!(target_dir(Path::new("/usr/local/bin/ideae")), None);
    }

    #[test]
    fn a_checkout_off_its_main_branch_or_changed_is_not_pulled() {
        let dir = std::env::temp_dir().join(format!("ideae-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.as_path();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(d).args(args).output().unwrap().status.success(), "{args:?}");
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "one"]);
        std::fs::write(d.join("a"), "1").unwrap();
        git(&["add", "a"]);
        assert!(super::blocked(d, "origin/main").unwrap().contains("uncommitted"));
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "two"]);
        assert_eq!(super::blocked(d, "origin/main"), None);
        git(&["switch", "-q", "-c", "feature"]);
        assert!(super::blocked(d, "origin/main").unwrap().contains("is on feature, not main"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
