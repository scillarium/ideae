use std::{
    path::{Path, PathBuf},
};
use std::sync::Arc;
#[cfg(test)]
use std::process::Command;

pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// `git worktree lock`, with its reason when given.
    pub locked: Option<String>,
}

impl Worktree {
    /// Locked by a process that still runs, such as a Claude Code subagent
    /// working in it ("claude agent … (pid N start T)"): someone else's
    /// working copy for now.
    pub fn in_use(&self) -> bool {
        self.locked.as_deref().and_then(lock_owner).is_some_and(|(pid, start)| process_alive(pid, start))
    }
}

/// The pid and start time named in a lock reason.
fn lock_owner(reason: &str) -> Option<(u32, Option<u64>)> {
    let rest = &reason[reason.find("pid ")? + 4..];
    let pid = rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    let start = rest
        .find("start ")
        .and_then(|i| rest[i + 6..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
    Some((pid, start))
}

/// The process exists and, when its start time is known, is the same one
/// and not a later process given the same pid.
fn process_alive(pid: u32, start: Option<u64>) -> bool {
    if cfg!(not(target_os = "linux")) {
        return crate::procs::alive(pid);
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    let Some(start) = start else { return true };
    // Field 22, counted after the parenthesized command name.
    let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    after.split_whitespace().nth(19).and_then(|s| s.parse::<u64>().ok()) == Some(start)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    // No optional locks: status and diff must not rewrite the index, or the
    // file watcher would see our own refresh and refresh again forever.
    let output = crate::procs::command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Names as they are: quoted ("b/\303\251.rs") they are not the
        // names the rest of the app knows the files by.
        .args(["-c", "core.quotePath=false"])
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    // `diff --no-index` exits 1 when the files differ.
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    // One file in Latin-1 is no reason to show nothing of all the others:
    // what is not UTF-8 shows as U+FFFD.
    Some(String::from_utf8(output.stdout).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Every worktree of the repository containing `path`, main one first.
pub fn worktrees(path: &Path) -> Vec<Worktree> {
    let Some(out) = git(path, &["worktree", "list", "--porcelain"]) else {
        return vec![Worktree { path: path.to_path_buf(), branch: "(no git)".into(), locked: None }];
    };
    let mut result = Vec::new();
    for block in out.split("\n\n") {
        let mut wt_path = None;
        let mut branch = String::from("(detached)");
        let mut locked = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                wt_path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = b.trim_start_matches("refs/heads/").to_string();
            } else if line == "locked" {
                locked = Some(String::new());
            } else if let Some(reason) = line.strip_prefix("locked ") {
                locked = Some(reason.trim_matches('"').to_string());
            } else if line == "bare" {
                wt_path = None;
            }
        }
        if let Some(path) = wt_path {
            result.push(Worktree { path, branch, locked });
        }
    }
    result
}

const DIFF_ARGS: &[&str] = &[
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--find-renames",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    // Whatever the user's own git is set to show: the patch is parsed, and
    // its changes are applied back one at a time.
    "--unified=3",
    "--submodule=short",
];

/// Uncommitted changes against HEAD, untracked files included, as one patch.
pub fn working_patch(root: &Path) -> String {
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    let untracked: Vec<&str> = untracked.split('\0').filter(|f| !f.is_empty()).collect();
    tracked_patch(root, "HEAD", false) + &new_files_patch(root, &untracked)
}

/// The tracked files of the worktree against `rev`.
fn tracked_patch(root: &Path, rev: &str, light: bool) -> String {
    let mut args = light_args(light, &["diff", rev]);
    args.extend_from_slice(DIFF_ARGS);
    without_huge_files(git(root, &args).unwrap_or_default())
}

/// The most untracked files a patch shows, the largest of them shown in
/// full, and the most of their text in all: an agent leaves logs, bundles
/// and data files behind, and the patch is read again on every change.
const NEW_FILES: usize = 200;
const NEW_FILE_BYTES: u64 = 512 * 1024;
const NEW_FILES_BYTES: usize = 4 * 1024 * 1024;

/// Untracked files as the patch `git diff` would give were they added,
/// made here: asking git costs a process a file, on every refresh.
fn new_files_patch(root: &Path, untracked: &[&str]) -> String {
    let mut patch = String::new();
    for file in untracked.iter().take(NEW_FILES) {
        new_file_patch(root, file, patch.len() < NEW_FILES_BYTES, &mut patch);
    }
    patch
}

fn new_file_patch(root: &Path, file: &str, in_full: bool, patch: &mut String) {
    use std::fmt::Write as _;
    let path = root.join(file);
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return };
    #[cfg(unix)]
    let executable = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
    #[cfg(not(unix))]
    let executable = false;
    let (mode, text) = if meta.file_type().is_symlink() {
        ("120000", std::fs::read_link(&path).ok().map(|to| to.to_string_lossy().into_owned()))
    } else if !meta.is_file() {
        return;
    } else {
        // Too large, or not text (as git tells: a NUL early on): named, not shown.
        let text = (in_full && meta.len() <= NEW_FILE_BYTES)
            .then(|| std::fs::read(&path).ok())
            .flatten()
            .filter(|bytes| !bytes[..bytes.len().min(8000)].contains(&0))
            .and_then(|bytes| String::from_utf8(bytes).ok());
        (if executable { "100755" } else { "100644" }, text)
    };
    let _ = write!(patch, "diff --git a/{file} b/{file}\nnew file mode {mode}\n");
    match text {
        None => {
            let _ = writeln!(patch, "Binary files /dev/null and b/{file} differ");
        }
        Some(text) if text.is_empty() => {}
        Some(text) => {
            let lines = text.split_inclusive('\n').count();
            let range = if lines == 1 { "+1".to_string() } else { format!("+1,{lines}") };
            let _ = write!(patch, "--- /dev/null\n+++ b/{file}\n@@ -0,0 {range} @@\n");
            for line in text.split_inclusive('\n') {
                patch.push('+');
                patch.push_str(line);
            }
            if !text.ends_with('\n') {
                patch.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
}

/// One change of a patch, on its own: a run of lines added and removed,
/// enough to take back or to stage without the rest of its file, or of
/// its hunk, where git puts changes a few lines apart together.
#[derive(Clone, Debug, PartialEq)]
pub struct Hunk {
    pub path: String,
    /// The last line it changes: on the new side, or on the old one
    /// (`modified` false) when it only removes.
    pub line: u32,
    pub modified: bool,
    /// Its lines, added and removed: what it is, wherever in the file the
    /// other changes have moved it.
    pub changes: String,
    /// The hunk it is in, kept once for all its changes, and which of the
    /// hunk's lines are this one: its patches are made when asked for. Made
    /// for each change ahead, they were the hunk again for every change in it.
    source: std::sync::Arc<HunkSource>,
    lines: std::ops::Range<usize>,
}

#[derive(Debug, PartialEq)]
struct HunkSource {
    /// The file's header.
    header: String,
    /// The first line on the old side and on the new.
    old: u32,
    new: u32,
    lines: Vec<String>,
}

impl Hunk {
    /// The file's header and this change as `git apply -R` takes it from
    /// the file as it is now: the other changes of its hunk are there.
    pub fn patch(&self) -> String {
        self.alone(true)
    }

    /// The same for `git apply` to the file as it was: they are not.
    pub fn stage(&self) -> String {
        self.alone(false)
    }

    /// With `as_it_is` the lines the other changes added are around this
    /// one, and those they removed are not; without, the reverse.
    fn alone(&self, as_it_is: bool) -> String {
        let HunkSource { header, old, new, lines } = &*self.source;
        let (mut body, mut olds, mut news, mut kept) = (String::new(), 0, 0, true);
        for (i, line) in lines.iter().enumerate() {
            let (sign, rest) = line.split_at(1.min(line.len()));
            let here = self.lines.contains(&i);
            let stays = if as_it_is { "+" } else { "-" };
            kept = match sign {
                "\\" => kept,
                "+" | "-" if !here => sign == stays,
                _ => true,
            };
            if !kept {
                continue;
            }
            let sign = if here || !line.starts_with(['+', '-']) { sign } else { " " };
            olds += u32::from(sign == " " || sign == "-");
            news += u32::from(sign == " " || sign == "+");
            body.push_str(sign);
            body.push_str(rest);
        }
        format!("{header}@@ -{old},{olds} +{new},{news} @@\n{body}")
    }
}

/// The changes of a patch `git diff` made. Not those of files that are new
/// or gone: there the file is the change.
pub fn hunks(patch: &str) -> Vec<Hunk> {
    let mut found = Vec::new();
    let (mut header, mut path, mut whole_file) = (String::new(), String::new(), false);
    // (first old line, first new line, lines)
    let mut open: Option<(u32, u32, Vec<&str>)> = None;
    let mut close = |open: &mut Option<(u32, u32, Vec<&str>)>, header: &str, path: &str, whole_file: bool| {
        let Some((old, new, lines)) = open.take() else { return };
        // Nor what was not UTF-8 and is shown with U+FFFD: it would not apply.
        if whole_file || path.is_empty() || lines.iter().any(|line| line.contains('\u{FFFD}')) {
            return;
        }
        let changed = |line: &str| line.starts_with(['+', '-']);
        let mut source = None;
        let mut at = 0;
        while at < lines.len() {
            if !changed(lines[at]) {
                at += 1;
                continue;
            }
            let end = (at..lines.len()).find(|&i| !changed(lines[i]) && !lines[i].starts_with('\\')).unwrap_or(lines.len());
            let (mut old_line, mut new_line, mut last) = (old, new, None);
            for (i, line) in lines.iter().enumerate().take(end) {
                let here = i >= at;
                match line.as_bytes().first() {
                    Some(b'+') => {
                        if here {
                            last = Some((new_line, true));
                        }
                        new_line += 1;
                    }
                    Some(b'-') => {
                        // A line added after it speaks for the change.
                        if here && !last.is_some_and(|(_, modified)| modified) {
                            last = Some((old_line, false));
                        }
                        old_line += 1;
                    }
                    Some(b' ') => {
                        old_line += 1;
                        new_line += 1;
                    }
                    _ => {}
                }
            }
            if let Some((line, modified)) = last {
                let source = source.get_or_insert_with(|| std::sync::Arc::new(HunkSource { header: header.to_string(), old, new, lines: lines.iter().map(|line| line.to_string()).collect() }));
                found.push(Hunk { path: path.to_string(), line, modified, changes: lines[at..end].concat(), source: source.clone(), lines: at..end });
            }
            at = end;
        }
    };
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            close(&mut open, &header, &path, whole_file);
            (header, path, whole_file) = (line.to_string(), String::new(), false);
        } else if let Some(at) = line.strip_prefix("@@ -") {
            close(&mut open, &header, &path, whole_file);
            // "12,3 +14,5 @@ fn name"
            let start = |side: &str| side.split(',').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
            let mut sides = at.split(' ');
            let (old, new) = (start(sides.next().unwrap_or_default()), start(sides.next().unwrap_or_default().trim_start_matches('+')));
            open = Some((old, new, Vec::new()));
        } else if let Some((_, _, lines)) = open.as_mut() {
            lines.push(line);
        } else {
            // A submodule's commit is no line to take back either.
            let submodule = line.strip_prefix("index ").is_some_and(|index| index.trim_end().ends_with(" 160000"));
            if line.starts_with("--- /dev/null") || line.starts_with("+++ /dev/null") || line.starts_with("Binary files") || submodule {
                whole_file = true;
            }
            if let Some(name) = line.strip_prefix("+++ b/") {
                path = name.trim_end().to_string();
                // The header a change is applied with is of the file as it is
                // named now, and of nothing else: with the one git gave, taking
                // one line back would also take back a rename or a mode.
                header = format!("diff --git a/{path} b/{path}\n--- a/{name}+++ b/{name}");
            } else {
                header.push_str(line);
            }
        }
    }
    close(&mut open, &header, &path, whole_file);
    found
}

/// Stages one change of a file, or takes it out of the index again. The
/// change is looked up in what the index has now, so that others of its
/// file staged before it are no obstacle.
pub fn stage_change(root: &Path, change: &Hunk, stage: bool) -> Result<String, String> {
    // The whole of what is staged, not the file's alone: a renamed file
    // would be a new one there, with no change to take out.
    let mut args = if stage { vec!["diff"] } else { vec!["diff", "--cached"] };
    args.extend_from_slice(DIFF_ARGS);
    if stage {
        args.extend(["--", change.path.as_str()]);
    }
    let now = hunks(&git(root, &args).unwrap_or_default())
        .into_iter()
        .find(|hunk| hunk.path == change.path && hunk.changes == change.changes)
        .ok_or("the change is no longer there")?;
    if stage { apply_hunk(root, &now.stage(), false, true) } else { apply_hunk(root, &now.patch(), true, true) }
}

/// Applies one hunk: to the worktree (`reverse` takes the change back),
/// or with `cached` to the index (stages it).
pub fn apply_hunk(root: &Path, patch: &str, reverse: bool, cached: bool) -> Result<String, String> {
    use std::io::Write as _;
    let mut command = crate::procs::command("git");
    command.current_dir(root).args(["apply", "--whitespace=nowarn"]);
    if reverse {
        command.arg("-R");
    }
    if cached {
        command.arg("--cached");
    }
    let mut child = command
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(patch.as_bytes());
    }
    let done = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if done.status.success() {
        Ok(String::new())
    } else {
        Err(String::from_utf8_lossy(&done.stderr).lines().next().unwrap_or("the change no longer applies").to_string())
    }
}

/// First line of each file's first hunk on the new side, keyed by path.
pub fn first_changed_lines(patch: &str) -> std::collections::HashMap<String, u32> {
    let mut lines = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            // Git ends a name that has a space in it with a tab.
            current = path.strip_prefix("b/").map(|path| path.trim_end_matches('\t').to_string());
        } else if line.starts_with("@@ ") {
            if let Some(path) = current.take() {
                let start = line
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix('+'))
                    .and_then(|range| range.split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1);
                lines.insert(path, start);
            }
        }
    }
    lines
}

/// Tracked and untracked-but-not-ignored files, relative to `root`.
#[cfg(test)]
pub fn files(root: &Path) -> Vec<String> {
    let out = git(root, &["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    let mut files: Vec<String> = out.split('\0').filter(|f| !f.is_empty()).map(str::to_string).collect();
    files.sort();
    files.dedup();
    files
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchInfo {
    pub head: String,
    pub upstream: Option<String>,
    /// Push and PR go to `origin`.
    pub has_origin: bool,
    pub ahead: u32,
    pub behind: u32,
    /// The branch work starts from (origin/main), and the commits there
    /// this branch lacks.
    pub base: Option<String>,
    pub behind_base: u32,
    pub last_hash: String,
    pub last_subject: String,
    /// The files a pull would leave in conflict, with what is committed
    /// here or only written; empty when none, or when it cannot be told.
    pub pull_conflicts: Vec<String>,
}

/// `git status` read once for all it tells: the branch, how far it is from
/// its upstream, each changed file's state, the untracked files.
#[derive(Default)]
struct Status {
    info: BranchInfo,
    /// The commit HEAD is at.
    oid: String,
    states: std::collections::HashMap<String, FileState>,
    untracked: Vec<String>,
    /// The tracked files that changed, with where a renamed one was.
    changed: Vec<String>,
    /// What `.gitignore` keeps out: files, and folders as `target/` with
    /// nothing of what is in them.
    ignored: Vec<String>,
}

/// With `light` git reads the worktree on one thread, not ten: the same
/// answer for a sixth of the processor's time and a little more of the
/// clock's, which is the better trade for a task nobody looks at.
fn light_args<'a>(light: bool, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = if light { vec!["-c", "core.preloadIndex=false"] } else { Vec::new() };
    all.extend_from_slice(args);
    all
}

fn parse_status(out: &str) -> Status {
    let mut status = Status::default();
    let mut entries = out.split('\0');
    while let Some(entry) = entries.next() {
        // What comes before the path: the kind, then fields by the kind.
        let changed = |fields: usize| {
            let mut parts = entry.splitn(fields + 1, ' ');
            let xy = parts.nth(1)?.as_bytes();
            Some((*xy.first()?, *xy.get(1)?, parts.nth(fields - 2)?.to_string()))
        };
        if let Some(head) = entry.strip_prefix("# branch.head ") {
            status.info.head = head.to_string();
        } else if let Some(oid) = entry.strip_prefix("# branch.oid ") {
            status.oid = oid.to_string();
        } else if let Some(upstream) = entry.strip_prefix("# branch.upstream ") {
            status.info.upstream = Some(upstream.to_string());
        } else if let Some(ab) = entry.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    status.info.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.info.behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(path) = entry.strip_prefix("? ") {
            // A directory, when it holds a repository of its own: no file of this one.
            if path.ends_with('/') {
                continue;
            }
            status.states.insert(path.to_string(), FileState { staged: false, unstaged: true, change: Change::Untracked });
            status.untracked.push(path.to_string());
        } else if let Some(path) = entry.strip_prefix("! ") {
            status.ignored.push(path.to_string());
        } else {
            let fields = match entry.as_bytes().first() {
                Some(b'1') => 8,
                Some(b'2') => {
                    // The original path of a rename or copy.
                    status.changed.extend(entries.next().map(str::to_string));
                    9
                }
                Some(b'u') => 10,
                _ => continue,
            };
            let kind = entry.as_bytes()[0];
            if let Some((x, y, path)) = changed(fields) {
                let change = match (kind, x, y) {
                    (b'u', ..) => Change::Conflict,
                    (_, b'A', _) | (_, _, b'A') => Change::Added,
                    _ => Change::Modified,
                };
                status.changed.push(path.clone());
                status.states.insert(path, FileState { staged: x != b'.', unstaged: y != b'.', change });
            }
        }
    }
    status
}

/// A file's time and size: whether it is the one seen before.
type Stamp = Option<(std::time::SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// What a refresh need not ask git every time: where the repository keeps
/// itself, whether it has an origin, the branch work starts from (asked
/// again now and then, since a remote can be added and a default changed),
/// and the answers of the last refresh that still hold, each with what it
/// was read from.
#[derive(Default)]
pub struct Known {
    /// What a pull was last tried on in the mind, and how it came out.
    pull: (String, Vec<String>),
    age: u32,
    git_dir: Option<PathBuf>,
    refs: Option<PathBuf>,
    has_origin: bool,
    base: Option<String>,
    /// The tracked files, by the index they were listed from.
    tracked: Option<(Stamp, Arc<Vec<String>>)>,
    /// Every file and the directories to watch, as made from the tracked
    /// files and the new ones last: made again only when one of the two is
    /// another, and one list for all who read it.
    listed: Option<Listed>,
    /// What only a new commit or a moved base changes.
    history: Option<(HistoryKey, History)>,
    /// What the patches last given were made of.
    patches: Option<u64>,
    /// The stashes, by the log they were listed from.
    stashes: (Stamp, Vec<Stash>),
}

struct Listed {
    tracked: Arc<Vec<String>>,
    untracked: Vec<String>,
    files: Arc<Vec<String>>,
    flat: Arc<std::collections::BTreeSet<PathBuf>>,
}

/// HEAD, the branch compared with, and the files its ref can be in.
#[derive(PartialEq)]
struct HistoryKey {
    oid: String,
    base: Option<String>,
    compared: Option<String>,
    refs: Vec<Stamp>,
}

#[derive(Clone, Default)]
struct History {
    behind_base: u32,
    ahead_of_base: u32,
    last_hash: String,
    last_subject: String,
    /// Where the branch left the one it is compared with.
    fork: Option<String>,
}

impl Known {
    fn fresh(&mut self, root: &Path) {
        if self.age % 30 == 0 {
            self.git_dir = git(root, &["rev-parse", "--absolute-git-dir"]).map(|dir| PathBuf::from(dir.trim()));
            self.refs = git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .map(|common| PathBuf::from(common.trim()).join("refs"))
                .filter(|refs| refs.is_dir());
            self.has_origin = git(root, &["remote", "get-url", "origin"]).is_some();
            self.base = base_ref(root);
        }
        self.age += 1;
    }

    /// The tracked files: listed again only when the index is another one.
    fn tracked(&mut self, root: &Path) -> Arc<Vec<String>> {
        // Stamped before it is read: an index written meanwhile is read again next time.
        let now = self.git_dir.as_ref().and_then(|dir| stamp(&dir.join("index")));
        if now.is_none() || self.tracked.as_ref().is_none_or(|(seen, _)| *seen != now) {
            let out = git(root, &["ls-files", "--cached", "-z"]).unwrap_or_default();
            self.tracked = Some((now, Arc::new(out.split('\0').filter(|f| !f.is_empty()).map(str::to_string).collect())));
        }
        self.tracked.as_ref().map(|(_, files)| files.clone()).unwrap_or_default()
    }

    /// The files a ref named `name` can be read from, as they are now. None
    /// where refs are not kept in files that tell (reftable): ask every time.
    fn ref_stamps(&self, names: &[Option<&str>]) -> Option<Vec<Stamp>> {
        let common = self.refs.as_ref()?.parent()?;
        if common.join("reftable").exists() {
            return None;
        }
        let mut stamps = vec![stamp(&common.join("packed-refs"))];
        for name in names.iter().flatten() {
            stamps.extend([stamp(&common.join("refs/remotes").join(name)), stamp(&common.join("refs/heads").join(name)), stamp(&common.join("refs").join(name))]);
        }
        Some(stamps)
    }
}

/// All a task shows of its worktree, read in one go.
pub struct Refreshed {
    pub files: Arc<Vec<String>>,
    /// What `.gitignore` keeps out, as the status names it.
    pub ignored: Vec<String>,
    pub watch: (Arc<std::collections::BTreeSet<PathBuf>>, Vec<PathBuf>),
    pub info: BranchInfo,
    pub states: std::collections::HashMap<String, FileState>,
    /// The branch compared with.
    pub base: Option<String>,
    /// None when they are the ones of the refresh before.
    pub patches: Option<Patches>,
    /// Git did not answer (no repository any more, no git): nothing here
    /// is to be believed, and what was read before stays.
    pub failed: bool,
    /// Work put away, the latest first.
    pub stashes: Vec<Stash>,
}

pub struct Patches {
    /// The uncommitted changes, untracked files included.
    pub working: String,
    /// The changes since the branch forked from the one compared with.
    pub branch: String,
    /// What is staged; asked for only when something is.
    pub staged: String,
}

/// Work put away with `git stash`. The repository has one list of it,
/// whichever worktree it was put away in.
#[derive(Clone, Debug, PartialEq)]
pub struct Stash {
    /// `stash@{0}`.
    pub name: String,
    /// "On main: before the pull".
    pub what: String,
    /// "2 hours ago".
    pub when: String,
    /// Its commit: the name is a place in the list, which every worktree
    /// of the repository adds to and takes from.
    pub id: String,
}

/// Where a stash is in the list now: an action is for the one that was
/// shown, not for whatever has its place since.
fn stash_now(root: &Path, stash: &Stash) -> Result<String, String> {
    stashes(root).into_iter().find(|now| now.id == stash.id).map(|now| now.name).ok_or_else(|| format!("{} ({}) is no longer in the list", stash.name, stash.what))
}

pub fn stashes(root: &Path) -> Vec<Stash> {
    let list = git(root, &["stash", "list", "--format=%gd%x1f%gs%x1f%cr%x1f%H"]).unwrap_or_default();
    list.lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            Some(Stash { name: parts.next()?.to_string(), what: parts.next()?.to_string(), when: parts.next()?.to_string(), id: parts.next()?.to_string() })
        })
        .collect()
}

/// Puts away everything not committed, files git does not know of too.
pub fn stash(root: &Path, message: &str) -> Result<String, String> {
    let mut args = vec!["stash", "push", "--include-untracked"];
    if !message.is_empty() {
        args.extend(["-m", message]);
    }
    run(root, "git", &args).map(|_| "Put away: it is under Stashes".to_string())
}

/// Brings a stash back into the worktree; with `drop` it leaves the list
/// when that went without a conflict. On a conflict it stays.
pub fn stash_apply(root: &Path, stash: &Stash, drop: bool) -> Result<String, String> {
    let name = &stash_now(root, stash)?;
    // Not tried when it would not go in: a stash applied halfway leaves its conflict in the files.
    if let Some(files) = merge_conflicts(root, Some(&format!("{name}^1")), name).filter(|files| !files.is_empty()) {
        return Err(format!("would conflict in {} (the stash is kept)", files.join(", ")));
    }
    run(root, "git", &["stash", "apply", name]).map_err(|error| {
        let first = error.lines().find(|line| line.starts_with("error") || line.contains("CONFLICT")).unwrap_or(error.lines().next().unwrap_or_default());
        format!("{first} (the stash is kept)")
    })?;
    if drop {
        run(root, "git", &["stash", "drop", name])?;
    }
    Ok(format!("Brought back {name}"))
}

pub fn stash_drop(root: &Path, stash: &Stash) -> Result<String, String> {
    let name = &stash_now(root, stash)?;
    run(root, "git", &["stash", "drop", name]).map(|_| format!("Dropped {name}"))
}

/// What a stash holds, as a patch against the commit it was made on.
pub fn stash_patch(root: &Path, name: &str) -> String {
    git(root, &["stash", "show", "-p", "--include-untracked", name]).unwrap_or_default()
}

/// `picked`: the branch to compare with when not the one work starts from.
/// `light`: for a task out of view, see `light_args`.
pub fn refresh(root: &Path, picked: Option<String>, light: bool, known: &mut Known) -> Refreshed {
    use std::hash::{Hash as _, Hasher as _};
    known.fresh(root);
    let raw = git(root, &light_args(light, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--ignored=matching"])).unwrap_or_default();
    // A status has its branch lines at the least.
    let failed = raw.is_empty();
    let Status { mut info, oid, states, untracked, changed, ignored } = parse_status(&raw);

    // Every file: the tracked ones and, from the status, those that are new.
    let tracked = known.tracked(root);
    if !known.listed.as_ref().is_some_and(|listed| Arc::ptr_eq(&listed.tracked, &tracked) && listed.untracked == untracked) {
        // With nothing new the tracked files are all of them, as git lists them: in order.
        let files = if untracked.is_empty() && tracked.is_sorted() {
            tracked.clone()
        } else {
            let mut files: Vec<String> = tracked.iter().chain(&untracked).cloned().collect();
            files.sort();
            files.dedup();
            Arc::new(files)
        };
        let mut flat: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        let mut last = "";
        for dir in files.iter().filter_map(|file| file.rsplit_once('/')).map(|(dir, _)| dir) {
            // Sorted: a directory's files come together.
            if dir != last {
                flat.insert(root.join(dir));
                last = dir;
            }
        }
        flat.insert(root.to_path_buf());
        flat.extend(known.git_dir.clone());
        known.listed = Some(Listed { tracked, untracked: untracked.clone(), files, flat: Arc::new(flat) });
    }
    let (files, flat) = known.listed.as_ref().map(|listed| (listed.files.clone(), listed.flat.clone())).unwrap_or_default();
    let watch = (flat, known.refs.clone().into_iter().collect());

    info.has_origin = known.has_origin;
    // Not for the base branch itself (main, tracking origin/main).
    let base = known.base.clone().filter(|b| b.rsplit('/').next() != Some(info.head.as_str()));
    // On the base branch itself the comparison is the uncommitted diff again.
    let compared = picked.or_else(|| known.base.clone().filter(|base| *base != info.head));
    let key = known.ref_stamps(&[base.as_deref(), compared.as_deref()]).map(|refs| HistoryKey { oid: oid.clone(), base: base.clone(), compared: compared.clone(), refs });
    let history = match (&key, &known.history) {
        (Some(key), Some((seen, history))) if key == seen => history.clone(),
        _ => {
            let count = |range: String| git(root, &["rev-list", "--count", &range]).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
            let mut history = History::default();
            if let Some(base) = &base {
                history.behind_base = count(format!("HEAD..{base}"));
                history.ahead_of_base = count(format!("{base}..HEAD"));
            }
            if let Some(last) = git(root, &["log", "-1", "--format=%h%x1f%s"]) {
                if let Some((hash, subject)) = last.trim_end().split_once('\x1f') {
                    (history.last_hash, history.last_subject) = (hash.to_string(), subject.to_string());
                }
            }
            history.fork = compared.as_ref().and_then(|base| git(root, &["merge-base", "HEAD", base])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            known.history = key.map(|key| (key, history.clone()));
            history
        }
    };
    if base.is_some() {
        info.behind_base = history.behind_base;
        // Not published yet: its own commits are the ones ahead.
        if info.upstream.is_none() {
            info.ahead = history.ahead_of_base;
        }
        info.base = base;
    }
    (info.last_hash, info.last_subject) = (history.last_hash, history.last_subject);

    // The patches are of what the status tells, the files it names as they
    // are on disk, and where the branch forked: the same of all three, the
    // same patches, and git is not asked.
    let mut made_of = std::collections::hash_map::DefaultHasher::new();
    // Not what is ignored: a build writing its output changes no patch.
    raw.split('\0').filter(|entry| !entry.starts_with("! ")).for_each(|entry| entry.hash(&mut made_of));
    (&history.fork, &compared).hash(&mut made_of);
    for path in changed.iter().chain(&untracked) {
        stamp(&root.join(path)).hash(&mut made_of);
    }
    let made_of = made_of.finish();
    let base = compared.filter(|_| history.fork.is_some());
    // The log of the stash is there as long as something is stashed, and
    // is written to whenever the list changes.
    let log = stamp(&known.refs.as_ref().and_then(|refs| refs.parent()).map(|common| common.join("logs/refs/stash")).unwrap_or_default());
    if known.stashes.0 != log {
        known.stashes = (log, if log.is_some() { stashes(root) } else { Vec::new() });
    }
    let stashes = known.stashes.1.clone();
    if known.patches == Some(made_of) {
        // As found when the patches were made: nothing it depends on has moved.
        if info.upstream.is_some() && info.behind > 0 {
            info.pull_conflicts = known.pull.1.clone();
        }
        return Refreshed { files, ignored, watch, info, states, base, patches: None, stashes, failed };
    }
    known.patches = Some(made_of);

    let untracked: Vec<&str> = untracked.iter().map(String::as_str).collect();
    let new_files = new_files_patch(root, &untracked);
    let working = changed_patch(root, &oid, &changed, light) + &new_files;
    let branch = match &history.fork {
        // No commit of its own yet: the same patch, not asked for twice.
        Some(fork) if *fork == oid => working.clone(),
        Some(fork) => tracked_patch(root, fork, light) + &new_files,
        None => String::new(),
    };
    if info.upstream.is_some() && info.behind > 0 {
        // Asked again only when a side of it has moved.
        let theirs = git(root, &["rev-parse", "@{u}"]).unwrap_or_default();
        let key = format!("{oid} {} {}", theirs.trim(), working.len() ^ working.bytes().fold(0usize, |sum, byte| sum.wrapping_mul(31).wrapping_add(byte as usize)));
        if known.pull.0 != key {
            known.pull = (key, pull_conflicts(root).unwrap_or_default());
        }
        info.pull_conflicts = known.pull.1.clone();
    }
    let staged = if states.values().any(|state| state.staged) { git(root, &[&["diff", "--cached"][..], DIFF_ARGS].concat()).unwrap_or_default() } else { String::new() };
    Refreshed { files, ignored, watch, info, states, base, patches: Some(Patches { working, branch, staged }), stashes, failed }
}

/// The most changed files `git diff` is told by name. More, and naming
/// them costs more than it saves (and a command line has an end).
const NAMED_PATHS: usize = 50;

/// The tree of no files, which every repository has.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The tracked files against HEAD, when the status has told which changed:
/// none, and git is not asked; a few, and it looks at those and not at
/// every file of the worktree.
fn changed_patch(root: &Path, head: &str, changed: &[String], light: bool) -> String {
    if changed.is_empty() {
        return String::new();
    }
    // Before the first commit there is no HEAD to compare with: what is
    // added is new against a tree with nothing in it.
    let head = if head == "(initial)" { EMPTY_TREE } else { "HEAD" };
    if changed.len() > NAMED_PATHS {
        return tracked_patch(root, head, light);
    }
    let mut args = vec!["--literal-pathspecs", "diff", head];
    args.extend_from_slice(DIFF_ARGS);
    args.push("--");
    args.extend(changed.iter().map(String::as_str));
    without_huge_files(git(root, &args).unwrap_or_default())
}

/// The most of one tracked file's changes a patch shows. A lock file or a
/// bundle written anew is megabytes that would be parsed, laid out and kept
/// for every task at every change: such a file is named, as a binary one is.
const FILE_PATCH_BYTES: usize = 1024 * 1024;

fn without_huge_files(patch: String) -> String {
    const START: &str = "diff --git ";
    let starts: Vec<usize> = patch.match_indices(START).map(|(at, _)| at).filter(|&at| at == 0 || patch.as_bytes()[at - 1] == b'\n').collect();
    let ends = starts.iter().skip(1).copied().chain([patch.len()]);
    if !starts.iter().zip(ends.clone()).any(|(start, end)| end - start > FILE_PATCH_BYTES) {
        return patch;
    }
    let mut out = String::with_capacity(patch.len().min(4 * FILE_PATCH_BYTES));
    for (&start, end) in starts.iter().zip(ends) {
        let section = &patch[start..end];
        // Named by the file as it is now, or as it was when it is gone.
        let name = section.lines().take_while(|line| !line.starts_with("@@ ")).find_map(|line| line.strip_prefix("+++ b/").or(line.strip_prefix("--- a/")));
        match name.filter(|_| section.len() > FILE_PATCH_BYTES) {
            Some(name) => {
                let name = name.trim_end_matches('\t');
                out.push_str(section.lines().next().unwrap_or_default());
                out.push_str(&format!("\nBinary files a/{name} and b/{name} differ\n"));
            }
            None => out.push_str(section),
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    /// The branches and tags that point at it: "main, origin/main, tag: v1".
    pub refs: String,
}

/// Whose history is read and which commits of it: all fields empty, the
/// task's own branch, all of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryQuery {
    /// A branch, tag or commit other than HEAD.
    pub rev: String,
    /// In the message, whatever the case.
    pub text: String,
    pub author: String,
    /// Commits that touched this file or folder.
    pub path: String,
}

impl HistoryQuery {
    /// As typed into one field: words are looked for in the message,
    /// `@ann` is the author, `file:src/a.rs` what the commit touched.
    pub fn typed(rev: &str, typed: &str) -> Self {
        let mut query = Self { rev: rev.to_string(), ..Self::default() };
        let mut words = Vec::new();
        for word in typed.split_whitespace() {
            if let Some(author) = word.strip_prefix('@') {
                query.author = author.to_string();
            } else if let Some(path) = word.strip_prefix("file:") {
                query.path = path.to_string();
            } else {
                words.push(word);
            }
        }
        query.text = words.join(" ");
        query
    }
}

/// The newest `limit` commits reachable from HEAD.
pub fn history(root: &Path, limit: usize, query: &HistoryQuery) -> Vec<Commit> {
    let limit = format!("-n{limit}");
    let (text, author) = (format!("--grep={}", query.text), format!("--author={}", query.author));
    let mut args = vec!["log", limit.as_str(), "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%s%x1f%D%x1e"];
    if !query.text.is_empty() {
        args.extend([text.as_str(), "-i", "--fixed-strings"]);
    }
    if !query.author.is_empty() {
        args.push(author.as_str());
    }
    if !query.rev.is_empty() {
        args.push(query.rev.as_str());
    }
    args.push("--");
    if !query.path.is_empty() {
        args.push(query.path.as_str());
    }
    let out = git(root, &args).unwrap_or_default();
    out.split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\x1f');
            Some(Commit {
                hash: fields.next().filter(|h| !h.is_empty())?.to_string(),
                short: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                date: fields.next()?.to_string(),
                subject: fields.next().unwrap_or_default().to_string(),
                // HEAD's own arrow says nothing the list does not.
                refs: fields.next().unwrap_or_default().split(", ").map(|name| name.trim_start_matches("HEAD -> ")).filter(|name| !name.is_empty() && *name != "HEAD" && !name.ends_with("/HEAD")).collect::<Vec<_>>().join(", "),
            })
        })
        .collect()
}

/// The patch a commit introduced, against its first parent.
/// A commit's message past its subject, trailing blank lines left out.
pub fn commit_body(root: &Path, hash: &str) -> String {
    git(root, &["show", "-s", "--format=%b", hash, "--"]).map(|b| b.trim_end().to_string()).unwrap_or_default()
}

pub fn commit_patch(root: &Path, hash: &str) -> String {
    let mut args = vec!["show", "--format=", "--patch", "--first-parent", "-m"];
    args.extend_from_slice(DIFF_ARGS);
    args.extend_from_slice(&[hash, "--"]);
    git(root, &args).unwrap_or_default()
}

/// Index (staged) and worktree (unstaged) state per path.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FileState {
    pub staged: bool,
    pub unstaged: bool,
    pub change: Change,
}

/// What became of a file since the last commit, as the tree colors it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Change {
    #[default]
    Modified,
    /// New, and added to the index.
    Added,
    /// New, and not added.
    Untracked,
    Conflict,
}

/// Runs a command in `root` for a user action: stdout on success, stderr (or
/// stdout) as the error.
pub fn run(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = crate::procs::command(program)
        .current_dir(root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// A commit message for what would be committed (the staged changes, or
/// all of them), written by `claude -p` in the style of the latest commits.
pub fn suggest_message(root: &Path, staged_only: bool) -> Result<String, String> {
    use std::io::Write as _;
    const MAX_DIFF: usize = 60_000;
    let mut diff = if staged_only { git(root, &["diff", "--cached"]).unwrap_or_default() } else { working_patch(root) };
    if diff.trim().is_empty() {
        return Err("Nothing to describe".into());
    }
    if diff.len() > MAX_DIFF {
        let mut cut = MAX_DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n[… the rest of the diff is left out]\n");
    }
    let log = git(root, &["log", "-12", "--format=%B%x1e"]).unwrap_or_default();
    let examples: Vec<&str> = log.split('\x1e').map(str::trim).filter(|m| !m.is_empty()).collect();
    let prompt = format!(
        "Write the git commit message for the diff below. Match the recent commits of this \
         repository: language, length and form of the subject, whether there is a body and how it \
         reads. Leave out trailers such as Co-Authored-By. Answer with the message only, no quotes, \
         no code fences.\n\nRecent commits, separated by ---:\n{}\n\nThe diff:\n{diff}",
        examples.join("\n---\n")
    );
    // Quick and on its own: no tools, no MCP servers, no session kept.
    let mut child = crate::procs::command("claude")
        .current_dir(root)
        .args(["-p", "--model", "haiku", "--tools", "", "--strict-mcp-config", "--no-session-persistence"])
        .env_remove(crate::agent::SOCK_ENV)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("claude: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("claude: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().trim_matches('`').trim().to_string();
    if out.status.success() && !text.is_empty() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "claude gave no message".into() } else { err })
    }
}

pub fn stage(root: &Path, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    run(root, "git", &args)
}

pub fn unstage(root: &Path, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(String::as_str));
    run(root, "git", &args)
}

/// Commits what is staged; with nothing staged, everything (`git add -A`
/// first). Asked of git here: what the window last read may be from before
/// a change was staged.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if git(root, &["diff", "--cached", "--name-only"]).is_some_and(|staged| staged.trim().is_empty()) {
        run(root, "git", &["add", "-A"])?;
    }
    run(root, "git", &["commit", "-m", message])
}

/// Pushes, setting the upstream on the first push of a branch.
pub fn push(root: &Path, has_upstream: bool) -> Result<String, String> {
    if has_upstream {
        run(root, "git", &["push"])
    } else {
        run(root, "git", &["push", "-u", "origin", "HEAD"])
    }
}

/// The branch's pull request URL, creating the PR (from the commits) if needed.
pub fn pull_request(root: &Path, has_upstream: bool) -> Result<String, String> {
    if let Ok(url) = run(root, "gh", &["pr", "view", "--json", "url", "-q", ".url"]) {
        return Ok(url);
    }
    push(root, has_upstream)?;
    let out = run(root, "gh", &["pr", "create", "--fill"])?;
    Ok(out.lines().rev().find(|l| l.starts_with("http")).unwrap_or(&out).to_string())
}

/// The directories to watch so that any change to the worktree or its git
/// state is seen: every directory holding a listed file, the worktree root,
/// the git dir (HEAD, index) and the shared refs.
pub fn base_ref(root: &Path) -> Option<String> {
    let nonempty = |out: Option<String>| out.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(r) = nonempty(git(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"])) {
        if r != "origin/HEAD" {
            return Some(r);
        }
    }
    ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|c| nonempty(git(root, &["rev-parse", "--verify", "-q", c])).is_some())
        .map(str::to_string)
}

/// Everything the task changed: from where it forked off `base` to the
/// worktree as it is now, uncommitted work included.
pub fn touched_files(root: &Path, base: &str) -> Option<std::collections::BTreeSet<String>> {
    let fork = git(root, &["merge-base", "HEAD", base]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())?;
    let changed = git(root, &["diff", "--name-only", "--no-renames", "-z", &fork])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    Some(
        changed
            .split('\0')
            .chain(untracked.split('\0'))
            .filter(|f| !f.is_empty() && !is_lockfile(f))
            .map(str::to_string)
            .collect(),
    )
}

pub fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".lock") || matches!(name, "package-lock.json" | "pnpm-lock.yaml" | "go.sum")
}

#[derive(Clone, Debug)]
pub struct BlameLine {
    pub hash: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub summary: String,
}

impl BlameLine {
    pub fn uncommitted(&self) -> bool {
        self.hash.bytes().all(|b| b == b'0')
    }
}

/// Who last changed each line (index 0 is line 1), by `git blame --porcelain`.
pub fn blame(root: &Path, file: &Path) -> Vec<Option<std::sync::Arc<BlameLine>>> {
    let Some(out) = git(root, &["blame", "--porcelain", "--", &file.to_string_lossy()]) else {
        return Vec::new();
    };
    // A commit is told once and shared by its lines: a file has thousands
    // of lines and a handful of commits.
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut commits: Vec<BlameLine> = Vec::new();
    let mut lines: Vec<Option<usize>> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for line in out.lines() {
        if line.starts_with('\t') {
            if let Some((commit, final_line)) = current.take() {
                if final_line > 0 {
                    if lines.len() < final_line {
                        lines.resize(final_line, None);
                    }
                    lines[final_line - 1] = Some(commit);
                }
            }
            continue;
        }
        let mut parts = line.split(' ');
        let first = parts.next().unwrap_or_default();
        if first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) {
            let final_line = parts.nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
            let commit = *index.entry(first).or_insert_with(|| {
                commits.push(BlameLine { hash: first.to_string(), author: String::new(), time: 0, summary: String::new() });
                commits.len() - 1
            });
            current = Some((commit, final_line));
            continue;
        }
        let Some(entry) = current.and_then(|(commit, _)| commits.get_mut(commit)) else { continue };
        let rest = line.split_once(' ').map(|(_, r)| r).unwrap_or_default();
        match first {
            "author" => entry.author = rest.to_string(),
            "author-time" => entry.time = rest.parse().unwrap_or(0),
            "summary" => entry.summary = rest.to_string(),
            _ => {}
        }
    }
    let commits: Vec<std::sync::Arc<BlameLine>> = commits.into_iter().map(std::sync::Arc::new).collect();
    lines.into_iter().map(|commit| commit.map(|commit| commits[commit].clone())).collect()
}

/// One commit's list entry, for a commit outside the loaded history.
pub fn commit_info(root: &Path, hash: &str) -> Option<Commit> {
    let out = git(root, &["log", "-1", "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%s", hash])?;
    let mut fields = out.trim_end().split('\x1f');
    Some(Commit {
        hash: fields.next().filter(|h| !h.is_empty())?.to_string(),
        short: fields.next()?.to_string(),
        author: fields.next()?.to_string(),
        date: fields.next()?.to_string(),
        subject: fields.next().unwrap_or_default().to_string(),
        refs: String::new(),
    })
}

/// "3 days ago" for a unix time.
pub fn ago(time: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let secs = (now - time).max(0);
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86400 => (s / 3600, "hour"),
        s if s < 86400 * 30 => (s / 86400, "day"),
        s if s < 86400 * 365 => (s / (86400 * 30), "month"),
        s => (s / (86400 * 365), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// Throws away every change to `path`: back to HEAD, staged or not; a file
/// HEAD does not have is deleted.
pub fn discard(root: &Path, path: &str) -> Result<String, String> {
    let in_head = run(root, "git", &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
    if in_head {
        run(root, "git", &["restore", "--source=HEAD", "--staged", "--worktree", "--", path])
    } else {
        let _ = run(root, "git", &["rm", "--cached", "-q", "--ignore-unmatch", "--", path]);
        std::fs::remove_file(root.join(path)).map_err(|e| e.to_string())?;
        Ok(format!("deleted {path}"))
    }
}

/// A file as it was in the worktree and the index, to put back after a
/// discard. The index holds a blob that is in the object store already.
#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub path: String,
    worktree: Option<Vec<u8>>,
    /// "mode,sha" of its index entry.
    index: Option<String>,
}

pub fn snapshot(root: &Path, path: &str) -> FileSnapshot {
    let index = git(root, &["ls-files", "-s", "--", path]).and_then(|out| {
        let mut f = out.lines().next()?.split_whitespace();
        Some(format!("{},{}", f.next()?, f.next()?))
    });
    FileSnapshot { path: path.to_string(), worktree: std::fs::read(root.join(path)).ok(), index }
}

pub fn restore(root: &Path, snap: &FileSnapshot) -> Result<String, String> {
    let path = &snap.path;
    match &snap.index {
        Some(entry) => run(root, "git", &["update-index", "--add", "--cacheinfo", &format!("{entry},{path}")])?,
        None => run(root, "git", &["rm", "--cached", "-q", "--ignore-unmatch", "--", path])?,
    };
    let file = root.join(path);
    match &snap.worktree {
        Some(bytes) => {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
        }
        None if file.exists() => std::fs::remove_file(&file).map_err(|e| e.to_string())?,
        None => {}
    }
    Ok(format!("Restored {path}"))
}

pub fn fetch(root: &Path) -> Result<String, String> {
    run(root, "git", &["fetch", "--prune"]).map(|out| if out.is_empty() { "Fetched".into() } else { out })
}

/// Brings in the upstream without ever leaving a mess: a fast-forward when
/// there is nothing local, else a rebase that is aborted on any conflict.
/// Uncommitted work is stashed around it either way.
/// Why a rebase onto the base did not happen.
#[derive(Debug)]
pub enum RebaseError {
    /// These files conflict; the rebase was undone.
    Conflicts(Vec<String>),
    Failed(String),
}

/// Takes a commit of elsewhere into this branch (`revert` false), or
/// commits its opposite. On a conflict nothing changes and the files that
/// conflict are told.
pub fn replay_commit(root: &Path, hash: &str, revert: bool) -> Result<String, RebaseError> {
    let (verb, args): (&str, &[&str]) = if revert { ("revert", &["revert", "--no-edit"]) } else { ("cherry-pick", &["cherry-pick"]) };
    let mut all = args.to_vec();
    all.push(hash);
    match run(root, "git", &all) {
        Ok(_) => Ok(if revert { format!("Reverted {}", &hash[..hash.len().min(8)]) } else { format!("Picked {}", &hash[..hash.len().min(8)]) }),
        Err(e) => {
            let conflicts: Vec<String> = git(root, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default().lines().map(str::to_string).collect();
            let _ = run(root, "git", &[verb, "--abort"]);
            match conflicts.is_empty() {
                true => Err(RebaseError::Failed(e.lines().find(|line| line.starts_with("error") || line.starts_with("fatal")).unwrap_or(e.lines().next().unwrap_or_default()).to_string())),
                false => Err(RebaseError::Conflicts(conflicts)),
            }
        }
    }
}

/// A worktree with a new branch at a commit, where a new task's would be:
/// the app then shows it as a task.
pub fn commit_worktree(root: &Path, hash: &str, copy_env: bool) -> Result<PathBuf, String> {
    let repo = main_worktree(root).ok_or("not a git repository")?;
    worktree_at(&repo, &format!("from-{}", &hash[..hash.len().min(8)]), hash, copy_env)
}

/// Fetches the base and rebases the branch onto it, uncommitted changes
/// stashed and put back. On a conflict nothing changes.
pub fn rebase_onto(root: &Path, base: &str) -> Result<String, RebaseError> {
    if let Some((remote, branch)) = base.split_once('/') {
        let _ = run(root, "git", &["fetch", remote, branch]);
    }
    // What is not committed is set aside and put back after: where it
    // would not fit back, git leaves its conflict in the files, under an
    // agent that may be writing them. Tried on trees first, and not begun.
    if worktree_commit(root).is_some_and(|commit| commit != "HEAD") {
        if let Some(files) = merge_conflicts(root, None, base).filter(|files| !files.is_empty()) {
            return Err(RebaseError::Conflicts(files));
        }
    }
    match run(root, "git", &["rebase", "--autostash", base]) {
        Ok(_) => Ok(format!("Rebased onto {base}")),
        Err(e) => {
            let conflicts: Vec<String> = git(root, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            let _ = run(root, "git", &["rebase", "--abort"]);
            if conflicts.is_empty() {
                Err(RebaseError::Failed(e.lines().next().unwrap_or_default().to_string()))
            } else {
                Err(RebaseError::Conflicts(conflicts))
            }
        }
    }
}

/// What the worktree `theirs` has that `ours` has not, in the files named:
/// a patch as `git diff` makes one, of two folders as they are on disk,
/// files no commit knows of among them. Files the same in both are left out.
pub fn between(ours: &Path, theirs: &Path, files: &[String]) -> String {
    let mut patch = String::new();
    for file in files {
        let (a, b) = (std::fs::read(ours.join(file)).ok(), std::fs::read(theirs.join(file)).ok());
        if a == b {
            continue;
        }
        patch.push_str(&format!("diff --git a/{file} b/{file}\n"));
        match (&a, &b) {
            (None, _) => patch.push_str("new file mode 100644\n"),
            (_, None) => patch.push_str("deleted file mode 100644\n"),
            _ => {}
        }
        let text = |side: &Option<Vec<u8>>| String::from_utf8(side.clone().unwrap_or_default()).ok().filter(|text| !text.contains('\0'));
        let (Some(old), Some(new)) = (text(&a), text(&b)) else {
            patch.push_str(&format!("Binary files a/{file} and b/{file} differ\n"));
            continue;
        };
        let (from, to) = (if a.is_some() { format!("a/{file}") } else { "/dev/null".into() }, if b.is_some() { format!("b/{file}") } else { "/dev/null".into() });
        let diff = similar::TextDiff::from_lines(&old, &new);
        patch.push_str(&diff.unified_diff().context_radius(3).header(&from, &to).to_string());
    }
    patch
}

/// The files that would be left in conflict by taking in the upstream:
/// the merge is made of trees only, nothing in the worktree or the index
/// moves. What is not committed counts as it is on disk. None when git is
/// too old to tell (before 2.38) or there is no upstream.
pub fn pull_conflicts(root: &Path) -> Option<Vec<String>> {
    merge_conflicts(root, None, "@{u}")
}

/// The same for any commit taken into the worktree; `base` is what both
/// sides are compared with when it is not their common ancestor.
fn merge_conflicts(root: &Path, base: Option<&str>, theirs: &str) -> Option<Vec<String>> {
    let written = worktree_commit(root)?;
    let mine = written.as_str();
    let mut merge = crate::procs::command("git");
    merge.current_dir(root).args(["-c", "core.quotePath=false", "merge-tree", "--write-tree", "--name-only", "--no-messages"]);
    if let Some(base) = base {
        merge.args(["--merge-base", base]);
    }
    let merged = merge.args([mine, theirs]).output().ok()?;
    match merged.status.code() {
        Some(0) => Some(Vec::new()),
        // The tree it made, then the files it could not merge.
        Some(1) => Some(String::from_utf8_lossy(&merged.stdout).lines().skip(1).filter(|line| !line.is_empty()).map(str::to_string).collect()),
        _ => None,
    }
}

/// A commit of the worktree as it is, kept nowhere: HEAD when it is clean.
/// Made in an index of its own, a copy of the repository's: `git stash
/// create` takes the index's lock and writes the index, which fails an
/// agent's `git add` at that moment and has every watcher look again.
fn worktree_commit(root: &Path) -> Option<String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let git_dir = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])?.trim());
    let index = std::env::temp_dir().join(format!("ideae-index-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::copy(git_dir.join("index"), &index).ok()?;
    let run = |args: &[&str]| {
        let done = crate::procs::command("git")
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            // Whoever the user is to git, or nobody: the commit is shown to no one.
            .envs([("GIT_AUTHOR_NAME", "ideae"), ("GIT_AUTHOR_EMAIL", "ideae@localhost"), ("GIT_COMMITTER_NAME", "ideae"), ("GIT_COMMITTER_EMAIL", "ideae@localhost")])
            .args(args)
            .output()
            .ok()
            .filter(|done| done.status.success())?;
        Some(String::from_utf8_lossy(&done.stdout).trim().to_string())
    };
    let made = (|| {
        run(&["add", "-u"])?;
        let tree = run(&["write-tree"])?;
        if run(&["rev-parse", "HEAD^{tree}"])? == tree {
            return Some("HEAD".to_string());
        }
        run(&["commit-tree", &tree, "-p", "HEAD", "-m", "worktree"])
    })();
    let _ = std::fs::remove_file(&index);
    made
}

/// The files of a patch that have a conflict left in them, as markers at
/// the start of added lines, and the line of the first.
pub fn conflict_marked(patch: &str) -> Vec<(String, u32)> {
    let mut found: Vec<(String, u32)> = Vec::new();
    let (mut path, mut line, mut opened) = (String::new(), 0u32, None);
    for text in patch.lines() {
        if text.starts_with("diff --git ") {
            (path, opened) = (String::new(), None);
        } else if let Some(name) = text.strip_prefix("+++ b/") {
            path = name.trim_end_matches('\t').to_string();
        } else if let Some(at) = text.strip_prefix("@@ -") {
            line = at.split(" +").nth(1).and_then(|new| new.split([',', ' ']).next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if text.starts_with('+') {
            if text.starts_with("+<<<<<<< ") {
                opened = Some(line);
            } else if text.starts_with("+>>>>>>> ") {
                if let Some(at) = opened.take().filter(|_| !path.is_empty() && found.last().is_none_or(|(last, _)| *last != path)) {
                    found.push((path.clone(), at));
                }
            }
            line += 1;
        } else if text.starts_with(' ') {
            line += 1;
        }
    }
    found
}

pub fn pull_safe(root: &Path) -> Result<String, String> {
    run(root, "git", &["fetch", "--prune"])?;
    let ahead = run(root, "git", &["rev-list", "--count", "@{u}..HEAD"])?.trim().parse::<u32>().unwrap_or(0);
    let behind = run(root, "git", &["rev-list", "--count", "HEAD..@{u}"])?.trim().parse::<u32>().unwrap_or(0);
    if behind == 0 {
        return Ok("Already up to date".into());
    }
    // Not tried at all then: what is written but not committed would come
    // back out of the stash with the conflict in it, and nothing undoes that.
    if let Some(files) = pull_conflicts(root).filter(|files| !files.is_empty()) {
        return Err(format!("would conflict in {}, nothing changed", files.join(", ")));
    }
    if ahead == 0 {
        run(root, "git", &["merge", "--ff-only", "--autostash", "@{u}"])?;
        return Ok(format!("Fast-forwarded {behind} commit{}", if behind == 1 { "" } else { "s" }));
    }
    match run(root, "git", &["rebase", "--autostash", "@{u}"]) {
        Ok(_) => Ok(format!("Rebased {ahead} local commit{} onto {behind} new", if ahead == 1 { "" } else { "s" })),
        Err(e) => {
            let _ = run(root, "git", &["rebase", "--abort"]);
            Err(format!("conflicts, nothing changed: {}", e.lines().next().unwrap_or_default()))
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::{discard, restore, run, snapshot};

    #[test]
    fn discard_then_restore_brings_back_worktree_and_index() {
        let root = std::env::temp_dir().join(format!("ideae-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| run(&root, "git", args).unwrap();
        git(&["init", "-q"]);
        // Windows runners have core.autocrlf on, which would check out "one\r\n".
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        std::fs::write(root.join("a.txt"), "staged\n").unwrap();
        git(&["add", "a.txt"]);
        std::fs::write(root.join("a.txt"), "worktree\n").unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        let (a, new) = (snapshot(&root, "a.txt"), snapshot(&root, "new.txt"));
        discard(&root, "a.txt").unwrap();
        discard(&root, "new.txt").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");
        assert!(!root.join("new.txt").exists());
        restore(&root, &a).unwrap();
        restore(&root, &new).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "worktree\n");
        assert_eq!(git(&["show", ":a.txt"]), "staged");
        assert_eq!(std::fs::read_to_string(root.join("new.txt")).unwrap(), "fresh\n");
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn lock_owner_and_liveness() {
        let me = std::process::id();
        assert_eq!(lock_owner("claude agent agent-a3 (pid 1532754 start 82664722)"), Some((1532754, Some(82664722))));
        assert_eq!(lock_owner("moved to usb disk"), None);
        assert!(process_alive(me, None));
        if cfg!(target_os = "linux") {
            assert!(!process_alive(me, Some(1)));
        }
        assert!(!process_alive(u32::MAX - 1, None));
    }
}

// They script repositories with bash, which Windows lacks or has as WSL's.
#[cfg(all(test, unix))]
mod pull_tests {
    use super::{commit_worktree, history, replay_commit, HistoryQuery, RebaseError, branch_rows, branch_worktree, delete_branch, between, conflict_marked, diff_against, hunks, pull_conflicts, pull_safe, refresh, refs, stash, stash_apply, stash_drop, stash_patch, working_patch, Known};
    use std::{path::Path, process::Command};

    fn sh(dir: &Path, script: &str) {
        let ok = Command::new("bash").arg("-ec").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    }

    fn setup(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("ideae-pull-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        sh(&base, "git init -q --bare -b main remote.git
            git clone -q remote.git a 2>/dev/null; cd a; git config user.email t@t; git config user.name t
            printf 'one\\ntwo\\nthree\\n' > f.txt; git add f.txt; git commit -qm base; git push -q origin main
            cd ..; git clone -q remote.git b; cd b; git config user.email t@t; git config user.name t");
        base
    }

    #[test]
    fn fast_forward_keeps_local_edits() {
        let base = setup("ff");
        sh(&base, "cd a; printf 'one\\ntwo\\nthree\\nfour\\n' > f.txt; git commit -qam up; git push -q origin main
            cd ../b; echo note > untracked.txt; printf 'ONE\\ntwo\\nthree\\n' > f.txt");
        assert_eq!(pull_conflicts(&base.join("b")), Some(Vec::new()), "the lines written here are not the ones that come");
        let out = pull_safe(&base.join("b")).unwrap();
        assert!(out.starts_with("Fast-forwarded"), "{out}");
        let text = std::fs::read_to_string(base.join("b/f.txt")).unwrap();
        assert_eq!(text, "ONE\ntwo\nthree\nfour\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// What is only written, not committed, and what comes change the
    /// same line: the pull is told beforehand and not tried, where a stash
    /// coming back would have left its conflict in the file.
    #[test]
    fn a_pull_over_unsaved_work_it_would_break_is_not_tried() {
        let base = setup("unsaved");
        sh(&base, "cd a; printf 'one\\nTWO-remote\\nthree\\n' > f.txt; git commit -qam remote; git push -q origin main
            cd ../b; printf 'one\\nTWO-written\\nthree\\n' > f.txt; git fetch -q");
        let b = base.join("b");
        assert_eq!(pull_conflicts(&b), Some(vec!["f.txt".to_string()]));
        assert!(pull_safe(&b).unwrap_err().starts_with("would conflict in f.txt"));
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "one\nTWO-written\nthree\n", "as it was written");
        let stashes = Command::new("git").args(["stash", "list"]).current_dir(&b).output().unwrap().stdout;
        assert!(stashes.is_empty(), "nothing put away and left there");
        assert!(conflict_marked(&working_patch(&b)).is_empty());
        // The conflict as a stash would have left it is seen for what it is.
        std::fs::write(b.join("f.txt"), "one\n<<<<<<< Updated upstream\nTWO-remote\n=======\nTWO-written\n>>>>>>> Stashed changes\nthree\n").unwrap();
        assert_eq!(conflict_marked(&working_patch(&b)), [("f.txt".to_string(), 2)]);
        std::fs::write(b.join("f.txt"), "one\n    let marker = \"<<<<<<< \";\n>>>>>>> alone\nthree\n").unwrap();
        assert!(conflict_marked(&working_patch(&b)).is_empty(), "a marker in code, or one without its pair, is no conflict");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two folders compared as they are: what differs is a patch git itself
    /// applies, what is the same in both is not in it.
    #[test]
    fn two_worktrees_compare_file_by_file() {
        let base = setup("between");
        let (a, b) = (base.join("a"), base.join("b"));
        sh(&base, "cd a; printf 'one\\nTWO\\nthree\\n' > f.txt; echo same > s.txt; echo only-a > gone.txt
            cd ../b; echo same > s.txt; echo new > made.txt; printf '\\000\\001' > blob.bin");
        let files: Vec<String> = ["f.txt", "s.txt", "gone.txt", "made.txt", "blob.bin", "nowhere.txt"].map(String::from).to_vec();
        let patch = between(&a, &b, &files);
        assert!(!patch.contains("s.txt") && !patch.contains("nowhere.txt"), "the same in both, or in neither:\n{patch}");
        assert!(patch.contains("Binary files a/blob.bin and b/blob.bin differ"));
        assert_eq!(hunks(&patch).iter().map(|h| (h.path.as_str(), h.line)).collect::<Vec<_>>(), [("f.txt", 2)], "one change; whole files are no hunks");
        // Git takes it: a's files become b's.
        let text: String = patch.split_inclusive("diff --git ").filter(|part| !part.contains("blob.bin")).collect();
        std::fs::write(base.join("ab.patch"), text.trim_end_matches("diff --git ")).unwrap();
        sh(&a, "git apply ../ab.patch");
        assert_eq!(std::fs::read_to_string(a.join("f.txt")).unwrap(), "one\ntwo\nthree\n");
        assert!(!a.join("gone.txt").exists() && std::fs::read_to_string(a.join("made.txt")).unwrap() == "new\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Work is put away, seen in the list and as a patch, and brought back;
    /// one that would not go back in without a conflict stays in the list.
    #[test]
    fn work_is_put_away_and_brought_back() {
        let base = setup("stash");
        let b = base.join("b");
        assert!(refresh(&b, None, false, &mut Known::default()).stashes.is_empty());
        sh(&b, "printf 'one\\nTWO\\nthree\\n' > f.txt; echo note > new.txt");
        stash(&b, "before lunch").unwrap();
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "one\ntwo\nthree\n");
        assert!(!b.join("new.txt").exists(), "the file git did not know of went with it");
        let listed = refresh(&b, None, false, &mut Known::default()).stashes;
        assert_eq!(listed.iter().map(|s| (s.name.as_str(), s.what.as_str())).collect::<Vec<_>>(), [("stash@{0}", "On main: before lunch")]);
        let patch = stash_patch(&b, "stash@{0}");
        assert!(patch.contains("+TWO") && patch.contains("new.txt"), "{patch}");
        // The line it changed is changed again and committed: it cannot simply go back.
        sh(&b, "printf 'one\\n2\\nthree\\n' > f.txt; git commit -qam two");
        let put_away = listed[0].clone();
        let refused = stash_apply(&b, &put_away, true).unwrap_err();
        assert!(refused.ends_with("(the stash is kept)"), "{refused}");
        assert_eq!(refresh(&b, None, false, &mut Known::default()).stashes.len(), 1);
        assert!(refused.starts_with("would conflict in f.txt"), "{refused}");
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "one\n2\nthree\n", "and nothing was touched");
        sh(&b, "git reset -q --hard HEAD~1");
        // Another worktree put something away since: the one shown is one further down, and still the one brought back.
        sh(&b, "echo other > other.txt; git stash push -q --include-untracked -m other");
        stash_apply(&b, &put_away, true).unwrap();
        assert!(!b.join("other.txt").exists(), "not the one that has its place now");
        let other = refresh(&b, None, false, &mut Known::default()).stashes.remove(0);
        assert!(stash_drop(&b, &put_away).unwrap_err().contains("no longer in the list"));
        stash_drop(&b, &other).unwrap();
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "one\nTWO\nthree\n");
        assert!(b.join("new.txt").exists() && refresh(&b, None, false, &mut Known::default()).stashes.is_empty(), "back, and off the list");
        stash(&b, "").unwrap();
        let last = refresh(&b, None, false, &mut Known::default()).stashes.remove(0);
        stash_drop(&b, &last).unwrap();
        assert!(refresh(&b, None, false, &mut Known::default()).stashes.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// What there is to compare with is listed with what each is, and the
    /// worktree is compared with any of it: all of it, or one file.
    #[test]
    fn the_worktree_compares_with_a_branch_a_tag_or_a_commit() {
        let base = setup("against");
        let b = base.join("b");
        sh(&b, "git tag v1; printf 'one\\ntwo\\nthree\\nfour\\n' > f.txt; echo other > g.txt; git add g.txt; git commit -qam four
            git branch side v1; printf 'one\\ntwo\\nthree\\nfour\\nfive\\n' > f.txt");
        let found = refs(&b);
        let what = |name: &str| found.iter().find(|(known, _)| known == name).map(|(_, what)| what.split(" · ").next().unwrap_or_default().to_string());
        assert_eq!((what("main"), what("side"), what("origin/main"), what("v1")), (Some("branch".into()), Some("branch".into()), Some("remote branch".into()), Some("tag".into())));
        assert!(found.iter().any(|(_, what)| what.starts_with("commit · ") && what.ends_with("four")), "{found:?}");
        let against_tag = diff_against(&b, "v1", None);
        assert!(against_tag.contains("+four") && against_tag.contains("+five") && against_tag.contains("g.txt"), "{against_tag}");
        let one_file = diff_against(&b, "side", Some("f.txt"));
        assert!(one_file.contains("+five") && !one_file.contains("g.txt"), "{one_file}");
        assert_eq!(diff_against(&b, "HEAD", Some("g.txt")), "", "as committed");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The branches are listed with what is known of each; one gets a
    /// worktree of its own, a remote one a local branch that follows it;
    /// one is deleted, here and on the remote, not merged only when told.
    #[test]
    fn branches_are_listed_opened_and_deleted() {
        let base = setup("branches");
        let b = base.join("b");
        sh(&b, "git branch done; git checkout -q -b wip; echo more >> f.txt; git commit -qam wip; git push -q -u origin wip 2>/dev/null
            git checkout -q main; cd ../a; git checkout -q -b theirs; echo x > t.txt; git add t.txt; git commit -qm theirs; git push -q origin theirs 2>/dev/null
            cd ../b; git fetch -q");
        let rows = branch_rows(&b);
        let row = |name: &str| rows.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{name} in {rows:?}")).clone();
        assert!(rows.iter().all(|r| r.name != "main" && r.name != "origin/main"), "not the branch work starts from: {rows:?}");
        assert!(row("done").merged && !row("wip").merged && row("origin/theirs").remote && row("wip").subject == "wip");
        assert_eq!(row("done").held, None);

        let opened = branch_worktree(&b, &row("origin/theirs"), false).unwrap();
        assert!(opened.join("t.txt").is_file(), "the remote branch, in a worktree of its own");
        let rows = branch_rows(&b);
        let theirs = rows.iter().find(|r| r.name == "theirs").expect("a local branch that follows it");
        assert_eq!(theirs.held.as_ref().map(|p| p.canonicalize().unwrap()), Some(opened.canonicalize().unwrap()));
        assert!(branch_worktree(&b, &row("origin/theirs"), false).is_err(), "not twice");

        delete_branch(&b, &row("done"), false).unwrap();
        assert!(delete_branch(&b, &row("wip"), false).is_ok(), "pushed: git lets it go");
        delete_branch(&b, &row("origin/wip"), false).unwrap();
        sh(&b, "git branch loose; git checkout -q loose; echo y >> f.txt; git commit -qam loose; git checkout -q main");
        let loose = branch_rows(&b).into_iter().find(|r| r.name == "loose").unwrap();
        assert!(!loose.merged && delete_branch(&b, &loose, false).is_err(), "what it holds would be lost");
        delete_branch(&b, &loose, true).unwrap();
        let left: Vec<String> = branch_rows(&b).into_iter().map(|r| r.name).collect();
        assert!(!left.iter().any(|n| ["done", "wip", "origin/wip", "loose"].contains(&n.as_str())), "{left:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// History is read of any branch and narrowed by message, author and
    /// file; a commit of another branch is taken in, one is reverted, and
    /// one that would conflict changes nothing and names the files.
    #[test]
    fn history_is_filtered_and_commits_are_picked_and_reverted() {
        let base = setup("history");
        let b = base.join("b");
        sh(&b, "git checkout -q -b side; echo s > side.txt; git add side.txt; git commit -qm 'Add the side file'
            printf 'one\\nSIDE\\nthree\\n' > f.txt; git -c user.name=ann commit -qam 'Change two on the side'; git tag v2
            git checkout -q main; printf 'one\\nMAIN\\nthree\\n' > f.txt; git commit -qam 'Change two on main'");
        let subjects = |typed: &str, rev: &str| history(&b, 50, &HistoryQuery::typed(rev, typed)).into_iter().map(|c| c.subject).collect::<Vec<_>>();
        assert_eq!(subjects("", ""), ["Change two on main", "base"]);
        assert_eq!(subjects("", "side"), ["Change two on the side", "Add the side file", "base"], "another branch's");
        assert_eq!(subjects("SIDE FILE", "side"), ["Add the side file"], "in the message, whatever the case");
        assert_eq!(subjects("@ann", "side"), ["Change two on the side"]);
        assert_eq!(subjects("file:side.txt", "side"), ["Add the side file"]);
        assert_eq!(subjects("change @ann file:f.txt", "side"), ["Change two on the side"], "all of them at once");
        let side = history(&b, 50, &HistoryQuery::typed("side", ""));
        assert_eq!((side[0].refs.as_str(), side[2].refs.as_str()), ("tag: v2, side", "origin/main"), "what points at each");

        assert!(replay_commit(&b, &side[1].hash, false).unwrap().starts_with("Picked"));
        assert!(b.join("side.txt").is_file() && subjects("", "")[0] == "Add the side file");
        let Err(RebaseError::Conflicts(files)) = replay_commit(&b, &side[0].hash, false) else { panic!("the same line changed on both") };
        assert_eq!(files, ["f.txt"]);
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "one\nMAIN\nthree\n", "nothing changed");
        assert!(git_clean(&b), "and no pick left halfway");
        let picked = history(&b, 1, &HistoryQuery::default())[0].hash.clone();
        assert!(replay_commit(&b, &picked, true).unwrap().starts_with("Reverted"));
        assert!(!b.join("side.txt").exists() && subjects("", "")[0].starts_with("Revert"));

        let made = commit_worktree(&b, &side[1].hash, false).unwrap();
        assert!(made.join("side.txt").is_file() && made.file_name().unwrap().to_string_lossy().starts_with("from-"), "a task of its own, at that commit");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn git_clean(dir: &Path) -> bool {
        let state = Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output().unwrap().stdout;
        state.is_empty()
    }

    #[test]
    fn conflict_is_undone() {
        let base = setup("conflict");
        sh(&base, "cd a; printf 'one\\nTWO-remote\\nthree\\n' > f.txt; git commit -qam remote; git push -q origin main
            cd ../b; printf 'one\\nTWO-local\\nthree\\n' > f.txt; git commit -qam local; git fetch -q");
        let head_before = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(base.join("b")).output().unwrap().stdout;
        assert_eq!(pull_conflicts(&base.join("b")), Some(vec!["f.txt".to_string()]));
        let err = pull_safe(&base.join("b")).unwrap_err();
        assert!(err.starts_with("would conflict in f.txt"), "{err}");
        let head_after = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(base.join("b")).output().unwrap().stdout;
        assert_eq!(head_before, head_after);
        assert!(!base.join("b/.git/rebase-merge").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rebase_without_conflict() {
        let base = setup("rebase");
        sh(&base, "cd a; printf 'one\\ntwo\\nthree\\nremote\\n' > f.txt; git commit -qam remote; git push -q origin main
            cd ../b; echo local > g.txt; git add g.txt; git commit -qm local");
        let out = pull_safe(&base.join("b")).unwrap();
        assert!(out.starts_with("Rebased"), "{out}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fetch_base_brings_the_default_branch() {
        let base = setup("fetchbase");
        sh(&base, "cd a; echo later > h.txt; git add h.txt; git commit -qm later; git push -q origin main");
        let b = base.join("b");
        let before = super::git(&b, &["rev-parse", "origin/main"]).unwrap();
        super::fetch_base(&b).unwrap();
        let after = super::git(&b, &["rev-parse", "origin/main"]).unwrap();
        assert_ne!(before, after);
        assert_eq!(after, super::git(&base.join("a"), &["rev-parse", "HEAD"]).unwrap());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn touched_files_since_the_fork() {
        let base = setup("touched");
        sh(&base, "cd a; echo later > h.txt; git add h.txt; git commit -qm later; git push -q origin main
            cd ../b; git checkout -qb task; echo c > g.txt; echo x > Cargo.lock; git add .; git commit -qm task
            echo changed >> f.txt; echo new > n.txt; git fetch -q");
        let files: Vec<String> = super::touched_files(&base.join("b"), "origin/main").unwrap().into_iter().collect();
        assert_eq!(files, ["f.txt", "g.txt", "n.txt"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Local and remote branches, for picking what to diff against.
/// A branch of the repository, here or on a remote, for the list one is
/// chosen from.
#[derive(Clone, Debug, PartialEq)]
pub struct BranchRow {
    /// `fix-login`, `origin/fix-login`.
    pub name: String,
    pub remote: bool,
    /// All of it is in the branch work starts from.
    pub merged: bool,
    pub when: String,
    pub subject: String,
    /// The worktree it is checked out in: a task has it.
    pub held: Option<PathBuf>,
}

pub fn branch_rows(root: &Path) -> Vec<BranchRow> {
    let base = base_ref(root).unwrap_or_else(|| "HEAD".into());
    let listed = |merged: bool| {
        let mut args = vec!["for-each-ref", "--sort=-committerdate", "--format=%(refname)%1f%(refname:short)%1f%(committerdate:relative)%1f%(subject)"];
        if merged {
            args.extend(["--merged", base.as_str()]);
        }
        args.extend(["refs/heads", "refs/remotes"]);
        git(root, &args).unwrap_or_default()
    };
    let merged: std::collections::HashSet<String> = listed(true).lines().filter_map(|line| line.split('\x1f').next().map(str::to_string)).collect();
    let held: std::collections::HashMap<String, PathBuf> = worktrees(root).into_iter().map(|w| (w.branch, w.path)).collect();
    listed(false)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let (full, name) = (parts.next()?, parts.next()?.to_string());
            let remote = full.starts_with("refs/remotes/");
            // The branch work starts from is not one to tidy away, nor the remote's pointer to it.
            if name.ends_with("/HEAD") || name == "origin" || name == base || base.strip_prefix("origin/") == Some(name.as_str()) {
                return None;
            }
            Some(BranchRow {
                merged: merged.contains(full),
                held: (!remote).then(|| held.get(&name).cloned()).flatten(),
                when: parts.next()?.to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
                name,
                remote,
            })
        })
        .collect()
}

/// Deletes a branch: here, or on its remote (`origin/x` is told to drop
/// `x`). One not merged goes only with `force`.
pub fn delete_branch(root: &Path, branch: &BranchRow, force: bool) -> Result<String, String> {
    if branch.remote {
        let (remote, name) = branch.name.split_once('/').ok_or("not a remote branch")?;
        run(root, "git", &["push", remote, "--delete", name])?;
    } else {
        run(root, "git", &["branch", if force { "-D" } else { "-d" }, &branch.name])?;
    }
    Ok(format!("Deleted {}", branch.name))
}

/// A worktree for a branch there is already, next to the repository
/// where a new task's would be: the app then shows it as a task. A remote
/// branch gets a local one that follows it.
pub fn branch_worktree(root: &Path, branch: &BranchRow, copy_env: bool) -> Result<PathBuf, String> {
    let repo = main_worktree(root).ok_or("not a git repository")?;
    let local = if branch.remote { branch.name.split_once('/').map_or(branch.name.as_str(), |(_, name)| name) } else { branch.name.as_str() };
    let dir = task_worktree_dir(&repo, local);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let dir_s = dir.to_string_lossy().into_owned();
    let known = git(&repo, &["rev-parse", "--verify", "-q", &format!("refs/heads/{local}")]).is_some_and(|hash| !hash.trim().is_empty());
    if branch.remote && !known {
        git_status(&repo, &["worktree", "add", "--track", "-b", local, &dir_s, &branch.name])?;
    } else {
        git_status(&repo, &["worktree", "add", &dir_s, local])?;
    }
    if copy_env {
        copy_env_files(&repo, &dir);
    }
    Ok(dir)
}

/// What the worktree can be compared with: branches, tags, the latest
/// commits. Each as git names it, and what it is.
pub fn refs(root: &Path) -> Vec<(String, String)> {
    let listed = |args: &[&str]| git(root, args).unwrap_or_default();
    let named = listed(&["for-each-ref", "--sort=-committerdate", "--format=%(refname)%1f%(refname:short)%1f%(committerdate:relative)%1f%(subject)", "refs/heads", "refs/remotes", "refs/tags"]);
    let mut found: Vec<(String, String)> = named
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let (full, short, when, subject) = (parts.next()?, parts.next()?, parts.next()?, parts.next().unwrap_or_default());
            let kind = if full.starts_with("refs/tags") { "tag" } else if full.starts_with("refs/remotes") { "remote branch" } else { "branch" };
            (!short.ends_with("/HEAD") && short != "origin").then(|| (short.to_string(), format!("{kind} · {when} · {subject}")))
        })
        .collect();
    let commits = listed(&["log", "-60", "--format=%h%x1f%cr%x1f%s"]);
    found.extend(commits.lines().filter_map(|line| {
        let mut parts = line.split('\x1f');
        Some((parts.next()?.to_string(), format!("commit · {} · {}", parts.next()?, parts.next().unwrap_or_default())))
    }));
    found
}

/// The worktree against `rev`, all of it or the one file: what is here
/// that the commit, tag or branch does not have.
pub fn diff_against(root: &Path, rev: &str, file: Option<&str>) -> String {
    let mut args = vec!["diff", rev];
    args.extend_from_slice(DIFF_ARGS);
    if let Some(file) = file {
        args.extend(["--", file]);
    }
    git(root, &args).unwrap_or_default()
}

pub fn branches(root: &Path) -> Vec<String> {
    git(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads", "refs/remotes"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty() && !b.ends_with("/HEAD") && *b != "origin")
        .map(str::to_string)
        .collect()
}

fn git_status(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::procs::command("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// The main worktree of the repository containing `path`, or None when it is
/// not in one.
pub fn main_worktree(path: &Path) -> Option<PathBuf> {
    git(path, &["rev-parse", "--git-dir"])?;
    worktrees(path).into_iter().next().map(|w| w.path)
}

/// Fetches the remote's default branch, quietly: no prompt for a password
/// or key (it fails instead), no gc afterwards, given up after a minute.
pub fn fetch_base(repo: &Path) -> Result<(), String> {
    let head = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).map(|s| s.trim().to_string());
    let branch = match head.as_deref().and_then(|h| h.strip_prefix("origin/")) {
        Some(branch) => branch.to_string(),
        None => ["main", "master"]
            .into_iter()
            .find(|b| git(repo, &["rev-parse", "--verify", "-q", &format!("refs/remotes/origin/{b}")]).is_some_and(|s| !s.trim().is_empty()))
            .ok_or("no origin/main or origin/master")?
            .to_string(),
    };
    let mut child = crate::procs::command("git")
        .arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet", "--no-auto-maintenance", "origin", &branch])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(format!("git fetch: {status}")),
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git fetch: timed out".into());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

/// Where a new task branches from: the remote's default branch, fetched
/// first, else the main worktree's branch.
fn new_task_base(repo: &Path) -> String {
    if let Some(remote_head) = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        let remote_head = remote_head.trim().to_string();
        if let Some(branch) = remote_head.strip_prefix("origin/") {
            let _ = git_status(repo, &["fetch", "--quiet", "origin", branch]);
            return remote_head;
        }
    }
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_else(|| "HEAD".into())
}

pub fn task_worktree_dir(repo: &Path, branch: &str) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    repo.with_file_name(format!("{name}.worktrees")).join(branch.replace('/', "-"))
}

/// Creates the worktree of a new task next to the repository, in
/// `<repo>.worktrees/<branch>`, and copies the main worktree's untracked
/// `.env*` files into it.
pub fn create_task_worktree(repo: &Path, branch: &str, copy_env: bool) -> Result<PathBuf, String> {
    worktree_at(repo, branch, &new_task_base(repo), copy_env)
}

fn worktree_at(repo: &Path, branch: &str, base: &str, copy_env: bool) -> Result<PathBuf, String> {
    let dir = task_worktree_dir(repo, branch);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let dir_s = dir.to_string_lossy().into_owned();
    git_status(repo, &["worktree", "add", "--no-track", "-b", branch, &dir_s, base])?;
    if copy_env {
        copy_env_files(repo, &dir);
    }
    Ok(dir)
}

/// Worktrees for several tries at one task, all from one commit so their
/// diffs compare: `from`, or where a new task would start, looked up once.
///
/// With the commit they are from: what their diffs are counted against.
pub fn create_variant_worktrees(repo: &Path, branches: &[String], from: Option<&str>, copy_env: bool) -> Result<(String, Vec<Result<PathBuf, String>>), String> {
    let base = from.map_or_else(|| new_task_base(repo), str::to_string);
    let commit = git(repo, &["rev-parse", "--verify", "-q", &format!("{base}^{{commit}}")])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{base} is no commit"))?;
    let made = branches.iter().map(|branch| worktree_at(repo, branch, &commit, copy_env)).collect();
    Ok((commit, made))
}

/// The names a new task would run into, read once for asking about many:
/// the repository's branches and the directories its worktrees are in.
#[derive(Default)]
pub struct TakenNames {
    branches: std::collections::HashSet<String>,
    dirs: std::collections::HashSet<String>,
}

impl TakenNames {
    pub fn read(repo: &Path) -> Self {
        let branches = git(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]).unwrap_or_default().lines().map(str::to_string).collect();
        let dirs = std::fs::read_dir(task_worktree_dir(repo, ""))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        Self { branches, dirs }
    }

    pub fn has(&self, branch: &str) -> bool {
        self.branches.contains(branch) || self.dirs.contains(&branch.replace('/', "-"))
    }
}

/// Where a task's branch left its base.
pub fn fork_point(root: &Path) -> Option<String> {
    let base = base_ref(root)?;
    git(root, &["merge-base", "HEAD", &base]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// What a task changed since `base` (the commit it was made from; where
/// it forked when that is not known), uncommitted work included: files,
/// lines added, lines removed. New files not yet added count with theirs.
pub fn change_size(root: &Path, base: Option<&str>) -> Option<(usize, usize, usize)> {
    let fork = base.filter(|b| !b.is_empty()).map(str::to_string).or_else(|| fork_point(root))?;
    let (files, mut added, removed) = shortstat(&git(root, &["diff", "--shortstat", &fork])?);
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    let new: Vec<&str> = untracked.split('\0').filter(|f| !f.is_empty()).collect();
    for file in &new {
        // Text of a sensible size; a binary or a dump adds no lines.
        let path = root.join(file);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() < 1_000_000) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                added += text.lines().count();
            }
        }
    }
    Some((files + new.len(), added, removed))
}

/// " 3 files changed, 84 insertions(+), 12 deletions(-)".
fn shortstat(line: &str) -> (usize, usize, usize) {
    let number = |word: &str| {
        line.split(',').find(|part| part.contains(word)).and_then(|part| part.split_whitespace().next()?.parse().ok()).unwrap_or(0)
    };
    (number("file"), number("insertion"), number("deletion"))
}

/// What a worktree changed since `base` as one text, uncommitted work
/// included: the diff, then the new files not yet added, as they are.
pub fn diff_with_new_files(root: &Path, base: &str) -> String {
    const MOST: usize = 1_500_000;
    let mut text = git(root, &["diff", base]).unwrap_or_default();
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for file in untracked.split('\0').filter(|f| !f.is_empty()) {
        let path = root.join(file);
        if text.len() > MOST || !std::fs::metadata(&path).is_ok_and(|m| m.len() < 200_000) {
            text.push_str(&format!("\nnew file {file} (not shown)\n"));
        } else if let Ok(content) = std::fs::read_to_string(&path) {
            text.push_str(&format!("\nnew file {file}:\n{content}\n"));
        }
    }
    if text.len() > MOST {
        let mut end = MOST;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (cut)\n");
    }
    text
}

/// Where tries that were not kept are parked: refs no branch list shows.
pub const PARKED: &str = "refs/ideae/variants";

/// Removes a try's worktree and branch, and keeps what it had, uncommitted
/// work too, under `keep_ref`.
pub fn park_and_remove(repo: &Path, path: &Path, branch: &str, keep_ref: &str) -> Result<(), String> {
    if path.exists() {
        if git(path, &["status", "--porcelain"]).is_some_and(|s| !s.trim().is_empty()) {
            git_status(path, &["add", "-A"])?;
            // An identity of its own and no signing: the user's may not be
            // set, or ask for a key. Not committed is not removed.
            let own = ["-c", "user.name=ideae", "-c", "user.email=ideae@localhost", "-c", "commit.gpgsign=false"];
            git_status(path, &[&own[..], &["commit", "--no-verify", "-q", "-m", "As it was left"]].concat())?;
        }
        // A name of its own: never over what was parked before.
        let free = std::iter::once(keep_ref.to_string())
            .chain((2..).map(|n| format!("{keep_ref}-{n}")))
            .find(|name| git_status(repo, &["rev-parse", "--verify", "-q", name]).is_err())
            .unwrap_or_else(|| keep_ref.to_string());
        git_status(path, &["update-ref", &free, "HEAD"])?;
    }
    remove_task_worktree(repo, path, branch, false)
}

/// Drops the parked tries whose ref, `…/<unix seconds>-<group>/<letter>`,
/// is older than `seconds`.
pub fn prune_parked(repo: &Path, now: u64, seconds: u64) {
    let Some(refs) = git(repo, &["for-each-ref", "--format=%(refname)", PARKED]) else { return };
    for name in refs.lines() {
        let parked = name.strip_prefix(PARKED).and_then(|rest| rest.trim_start_matches('/').split('-').next()?.parse::<u64>().ok());
        if parked.is_some_and(|at| now.saturating_sub(at) > seconds) {
            let _ = git_status(repo, &["update-ref", "-d", name]);
        }
    }
}

fn copy_env_files(from: &Path, to: &Path) {
    let Some(out) = git(from, &["ls-files", "--others", "--ignored", "--exclude-standard", "-z", "--", ":(glob)**/.env*"])
    else {
        return;
    };
    for rel in out.split('\0').filter(|f| !f.is_empty()) {
        let target = to.join(rel);
        if target.exists() {
            continue;
        }
        if let Some(dir) = target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::copy(from.join(rel), &target);
    }
}

/// What deleting a task's worktree would lose: uncommitted files, commits on
/// no remote.
pub fn unsaved_work(root: &Path) -> (usize, usize) {
    let dirty = git(root, &["status", "--porcelain"]).map_or(0, |s| s.lines().count());
    let unpushed = git(root, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    (dirty, unpushed)
}

/// The file as the last commit has it, or with `since_fork` as the commit
/// the branch left its base at has it. Empty for a file that commit lacks:
/// every line of it is new. None for what git ignores, or outside a
/// repository: there is nothing to compare such a file with.
pub fn committed_text(root: &Path, file: &Path, since_fork: bool) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    let commit = since_fork.then(|| fork_point(root)).flatten().unwrap_or_else(|| "HEAD".into());
    if let Ok(text) = git_status(root, &["show", &format!("{commit}:./{rel}")]) {
        return Some(text);
    }
    let in_repository = git_status(root, &["rev-parse", "--is-inside-work-tree"]).is_ok();
    let ignored = git_status(root, &["check-ignore", "-q", "--", &rel]).is_ok();
    (in_repository && !ignored).then(String::new)
}

/// A new name for the branch a worktree is on.
pub fn rename_branch(root: &Path, from: &str, to: &str) -> Result<(), String> {
    git_status(root, &["branch", "-m", from, to]).map(|_| ())
}

/// Whether the repository has a branch of this name.
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git_status(repo, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok()
}

/// Whether the branch was pushed: it follows a branch of a remote, or the
/// remote has one of its name (a push without `-u`).
pub fn has_upstream(root: &Path) -> bool {
    if git_status(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok() {
        return true;
    }
    git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .is_some_and(|branch| git_status(root, &["rev-parse", "--verify", "-q", &format!("refs/remotes/origin/{}", branch.trim())]).is_ok())
}

/// Removes a task's worktree and its branch; a lock is overridden only
/// when `unlock`.
pub fn remove_task_worktree(repo: &Path, path: &Path, branch: &str, unlock: bool) -> Result<(), String> {
    let path_s = path.to_string_lossy().into_owned();
    if path.exists() {
        let mut args = vec!["worktree", "remove", "--force"];
        if unlock {
            args.push("--force");
        }
        args.push(&path_s);
        git_status(repo, &args)?;
    } else {
        let _ = git_status(repo, &["worktree", "prune"]);
    }
    if !branch.is_empty() && !branch.starts_with('(') {
        let _ = git_status(repo, &["branch", "-D", branch]);
    }
    Ok(())
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    // Scripted with bash, which Windows lacks or has as WSL's.
    #[cfg(unix)]
    fn sh(dir: &Path, script: &str) {
        let ok = std::process::Command::new("bash").arg("-ec").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    }

    #[cfg(unix)]
    #[test]
    fn variants_fork_from_one_commit_and_the_ones_not_kept_are_parked() {
        let base = std::env::temp_dir().join(format!("ideae-variants-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        sh(&base, "git init -q -b main repo; cd repo; git config user.email t@t; git config user.name t
            printf 'one\\n' > f.txt; git add f.txt; git commit -qm base");
        let repo = base.join("repo");
        let branches = ["fix-a".to_string(), "fix-b".to_string()];
        let (from, made) = create_variant_worktrees(&repo, &branches, None, false).unwrap();
        let made: Vec<PathBuf> = made.into_iter().map(Result::unwrap).collect();
        let taken = TakenNames::read(&repo);
        assert!(taken.has("fix-a") && taken.has("main") && !taken.has("fix-c"));
        let head = |dir: &Path| git(dir, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(head(&made[0]), head(&made[1]));
        assert_eq!((head(&made[0]), from.clone()), (head(&repo), head(&repo).trim().to_string()));

        // A commits, B leaves its work uncommitted, with a new file.
        sh(&made[0], "printf 'one\\ntwo\\n' > f.txt; git commit -qam more");
        sh(&made[1], "printf 'ONE\\n' > f.txt; echo new > g.txt");
        // No remote here: the base is main itself.
        assert_eq!(change_size(&made[0], Some(&from)), Some((1, 1, 0)));
        assert_eq!(change_size(&made[1], None), Some((2, 2, 1)), "the new file's line too");
        // A later try at the task starts where the first did.
        let later = create_variant_worktrees(&repo, &["fix-c".to_string()], fork_point(&made[0]).as_deref(), false).unwrap().1.remove(0).unwrap();
        assert_eq!(head(&later), head(&repo));

        // What the gutter compares a file with: the last commit, or where the branch forked.
        assert_eq!(committed_text(&made[0], &made[0].join("f.txt"), false).as_deref(), Some("one\ntwo\n"), "A committed its line");
        assert_eq!(committed_text(&made[0], &made[0].join("f.txt"), true).as_deref(), Some("one\n"), "before the branch");
        assert_eq!(committed_text(&made[1], &made[1].join("g.txt"), false).as_deref(), Some(""), "a new file: all of it is new");
        sh(&later, "echo 'build/' > .gitignore; mkdir build; echo x > build/out.txt");
        assert_eq!(committed_text(&later, &later.join("build/out.txt"), false), None, "what git ignores is compared with nothing");
        assert_eq!(committed_text(&base, &base.join("state.json"), false), None, "nor what is in no repository");

        // One change of two in a file is taken back, then staged, the other stays.
        sh(&repo, "seq 1 30 > n.txt; git add n.txt; git commit -qm numbers; sed -i.bak -e 's/^2$/two/' -e 's/^28$/twenty-eight/' n.txt; rm n.txt.bak");
        let found = hunks(&working_patch(&repo));
        assert_eq!(found.iter().map(|h| (h.path.as_str(), h.line, h.modified)).collect::<Vec<_>>(), [("n.txt", 2, true), ("n.txt", 28, true)]);
        apply_hunk(&repo, &found[0].patch(), true, false).unwrap();
        let text = std::fs::read_to_string(repo.join("n.txt")).unwrap();
        assert!(text.starts_with("1\n2\n3\n") && text.contains("twenty-eight"), "the first is back, the second is there");
        apply_hunk(&repo, &hunks(&working_patch(&repo))[0].stage(), false, true).unwrap();
        assert!(git(&repo, &["diff", "--cached"]).unwrap().contains("+twenty-eight"), "staged");
        assert!(apply_hunk(&repo, &found[0].patch(), true, false).is_err(), "what is no longer there cannot be taken back");
        sh(&repo, "git reset -q --hard; rm -f gone.txt; echo new > untracked.txt");
        assert!(hunks(&working_patch(&repo)).is_empty(), "a new file is no hunk");

        // Changes a line apart are one hunk to git, and still each its own:
        // a line changed, one removed, two added.
        sh(&repo, "git checkout -q -- n.txt; git reset -q; printf '1\\ntwo\\n3\\n5\\n6\\nsix\\nand a half\\n' > m.txt; seq 7 12 >> m.txt
            seq 1 12 > n.txt; git add n.txt; git commit -qm short; mv m.txt n.txt");
        let found = hunks(&working_patch(&repo));
        assert_eq!(found.iter().map(|h| (h.line, h.modified)).collect::<Vec<_>>(), [(2, true), (4, false), (7, true)]);
        let numbers = |repo: &Path| std::fs::read_to_string(repo.join("n.txt")).unwrap().replace('\n', " ");
        apply_hunk(&repo, &found[1].patch(), true, false).unwrap();
        assert_eq!(numbers(&repo), "1 two 3 4 5 6 six and a half 7 8 9 10 11 12 ", "the removed line is back, alone");
        apply_hunk(&repo, &found[2].stage(), false, true).unwrap();
        assert_eq!(git(&repo, &["diff", "--cached", "--numstat"]).unwrap().trim(), "2\t0\tn.txt", "only the two added lines are staged");
        let left = hunks(&working_patch(&repo));
        assert_eq!(left.len(), 2, "the patch is against the commit: what is staged is still in it");
        // Its neighbour in the same hunk is staged after it, then the first is taken out again.
        stage_change(&repo, &left[0], true).unwrap();
        assert_eq!(git(&repo, &["diff", "--cached", "--numstat"]).unwrap().trim(), "3\t1\tn.txt");
        assert_eq!(hunks(&refresh(&repo, None, false, &mut Known::default()).patches.unwrap().staged).len(), 2);
        stage_change(&repo, &found[2], false).unwrap();
        assert_eq!(git(&repo, &["diff", "--cached", "--numstat"]).unwrap().trim(), "1\t1\tn.txt", "the changed line stays staged");
        assert!(stage_change(&repo, &found[2], false).is_err(), "it is not staged any more");
        stage_change(&repo, &left[0], false).unwrap();
        assert_eq!(refresh(&repo, None, false, &mut Known::default()).patches.unwrap().staged, "", "nothing staged: not asked for");
        apply_hunk(&repo, &left[0].patch(), true, false).unwrap();
        assert_eq!(numbers(&repo), "1 2 3 4 5 6 six and a half 7 8 9 10 11 12 ");
        sh(&repo, "rm untracked.txt");

        assert!(!has_upstream(&made[0]));
        let parked = format!("{PARKED}/1000-fix/b");
        park_and_remove(&repo, &made[1], "fix-b", &parked).unwrap();
        assert!(!made[1].exists() && !branch_exists(&repo, "fix-b"));
        let kept = git(&repo, &["show", "--stat", "--format=", &parked]).unwrap();
        assert!(kept.contains("f.txt") && kept.contains("g.txt"), "what it had, uncommitted work too: {kept}");

        assert!(!branch_exists(&repo, "fix"));
        rename_branch(&made[0], "fix-a", "fix").unwrap();
        assert!(branch_exists(&repo, "fix") && !branch_exists(&repo, "fix-a"));

        prune_parked(&repo, 1000 + 10, 100);
        assert!(git(&repo, &["rev-parse", "--verify", "-q", &parked]).is_some(), "still young");
        prune_parked(&repo, 1000 + 200, 100);
        assert!(git(&repo, &["rev-parse", "--verify", "-q", &parked]).is_none_or(|s| s.trim().is_empty()), "gone when old");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn shortstat_reads_what_is_there() {
        assert_eq!(shortstat(" 3 files changed, 84 insertions(+), 12 deletions(-)\n"), (3, 84, 12));
        assert_eq!(shortstat(" 1 file changed, 1 deletion(-)\n"), (1, 0, 1));
        assert_eq!(shortstat(""), (0, 0, 0));
    }
}

#[cfg(all(test, unix))]
mod refresh_tests {
    use super::*;
    use gpui_kit::component::diff::{DiffFile, DiffFileStatus};

    fn sh(dir: &Path, script: &str) {
        let ok = Command::new("bash").arg("-ec").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    }

    #[test]
    fn status_tells_the_branch_the_states_and_the_untracked() {
        let out = "# branch.oid abc\0# branch.head task\0# branch.upstream origin/task\0# branch.ab +2 -1\0\
            1 .M N... 100644 100644 100644 aaa bbb src/a b.rs\0\
            1 A. N... 000000 100644 100644 000 bbb new.rs\0\
            2 R. N... 100644 100644 100644 aaa bbb R100 to here.rs\0from.rs\0\
            u UU N... 100644 100644 100644 100644 a b c both.rs\0\
            ? notes/todo.md\0! target/\0! .env\0";
        let status = parse_status(out);
        assert_eq!((status.info.head.as_str(), status.info.upstream.as_deref(), status.info.ahead, status.info.behind), ("task", Some("origin/task"), 2, 1));
        assert_eq!(status.oid, "abc");
        let state = |path: &str| status.states.get(path).map(|s| (s.staged, s.unstaged));
        assert_eq!(state("src/a b.rs"), Some((false, true)));
        assert_eq!(state("new.rs"), Some((true, false)));
        assert_eq!(state("to here.rs"), Some((true, false)));
        assert_eq!(state("both.rs"), Some((true, true)));
        assert_eq!(state("notes/todo.md"), Some((false, true)));
        assert_eq!((status.states.len(), status.untracked.as_slice()), (5, ["notes/todo.md".to_string()].as_slice()));
        let change = |path: &str| status.states.get(path).map(|s| s.change);
        assert_eq!(
            ["src/a b.rs", "new.rs", "to here.rs", "both.rs", "notes/todo.md"].map(change),
            [Change::Modified, Change::Added, Change::Modified, Change::Conflict, Change::Untracked].map(Some)
        );
        assert_eq!(status.ignored, ["target/", ".env"]);
    }

    /// A refresh of a real worktree: what it reads, and its patch of new
    /// files against the one git makes itself.
    #[test]
    fn a_refresh_reads_the_worktree_as_git_shows_it() {
        let base = std::env::temp_dir().join(format!("ideae-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        sh(&base, "git init -q --bare -b main remote.git; git clone -q remote.git a 2>/dev/null; cd a
            git config user.email t@t; git config user.name t; mkdir src
            printf 'one\\ntwo\\n' > f.txt; echo x > src/staged.txt; git add .; git commit -qm base; git push -q origin main
            git checkout -qb task
            echo three >> f.txt; echo y >> src/staged.txt; git add src/staged.txt
            printf 'new\\nfile\\n' > 'new one.txt'; printf 'no newline' > src/tail.txt; : > empty.txt
            printf 'a\\0b' > data.bin; printf '#!/bin/sh\\n' > run.sh; chmod +x run.sh; ln -s f.txt link
            head -c 700000 /dev/zero | tr '\\0' 'x' > big.txt");
        let root = base.join("a").canonicalize().unwrap();
        let mut known = Known::default();
        let mut read = refresh(&root, None, false, &mut known);
        let Patches { working, branch, .. } = read.patches.take().unwrap();

        assert_eq!((read.info.head.as_str(), read.info.base.as_deref(), read.info.has_origin, read.info.ahead), ("task", Some("origin/main"), true, 0));
        assert!(!read.info.last_hash.is_empty() && read.info.last_subject == "base");
        assert!(read.files.contains(&"src/tail.txt".to_string()) && read.files.contains(&"f.txt".to_string()));
        assert!(read.watch.0.contains(&root) && read.watch.0.contains(&root.join("src")) && read.watch.0.contains(&root.join(".git")));
        assert_eq!(read.watch.1, vec![root.join(".git/refs")]);
        let state = |path: &str| read.states.get(path).map(|s| (s.staged, s.unstaged));
        assert_eq!((state("f.txt"), state("src/staged.txt"), state("new one.txt")), (Some((false, true)), Some((true, false)), Some((false, true))));
        assert_eq!((read.base.as_deref(), branch == working), (Some("origin/main"), true), "no commit of its own: one patch");
        assert_eq!(*read.files, files(&root), "the tracked files and the new ones of the status are all of them");

        // Nothing changed: git is not asked for the patches again, by a task in view or out of it.
        let again = refresh(&root, None, false, &mut known);
        assert!(again.patches.is_none() && again.files == read.files && again.info == read.info && again.states == read.states);
        assert!(Arc::ptr_eq(&again.files, &read.files) && Arc::ptr_eq(&again.watch.0, &read.watch.0), "no file came or went: the list is the one made before, not another like it");
        assert!(refresh(&root, None, true, &mut known).patches.is_none());
        // The patches of a task out of view, and those of the few files named to git, are the same.
        let light = refresh(&root, None, true, &mut Known::default()).patches.unwrap();
        assert!(light.working == working && light.branch == branch);
        let named = changed_patch(&root, "", &["f.txt".to_string(), "src/staged.txt".to_string()], false);
        assert_eq!(named, tracked_patch(&root, "HEAD", false));
        // A changed file changes again, the status the same: asked.
        std::thread::sleep(std::time::Duration::from_millis(20));
        sh(&root, "echo four >> f.txt");
        let changed = refresh(&root, None, false, &mut known).patches.expect("another patch");
        assert!(changed.working.contains("+four") && changed.working != working);
        sh(&root, "printf 'one\\ntwo\\nthree\\n' > f.txt");
        let working = refresh(&root, None, false, &mut known).patches.unwrap().working;

        let ours = DiffFile::parse(&working).unwrap();
        // The same files as git makes them, one process each.
        let mut theirs = git(&root, &["diff", "HEAD"]).unwrap();
        for file in ["big.txt", "data.bin", "empty.txt", "link", "new one.txt", "run.sh", "src/tail.txt"] {
            theirs.push_str(&git(&root, &["diff", "--no-index", "--", "/dev/null", file]).unwrap());
        }
        let theirs = DiffFile::parse(&theirs).unwrap();
        let shape = |files: &[DiffFile]| {
            let mut all: Vec<_> = files.iter().map(|f| (f.path().to_string(), matches!(f.status(), DiffFileStatus::Added), f.additions(), f.deletions())).collect();
            all.sort();
            all
        };
        let (mut ours, theirs) = (shape(&ours), shape(&theirs));
        // Too large to show: named, where git gives all of it.
        let big = ours.iter_mut().find(|f| f.0 == "big.txt").unwrap();
        assert_eq!((big.1, big.2), (true, 0));
        big.2 = 1;
        assert_eq!(ours, theirs);
        assert!(working.len() < 10_000, "the large file is not in the patch");
        assert_eq!(first_changed_lines(&working).get("new one.txt"), Some(&1));

        // A renamed file, staged: named to git with where it was, it is a rename as in the whole diff.
        sh(&root, "git mv f.txt g.txt");
        let renamed = refresh(&root, None, false, &mut known).patches.unwrap().working;
        assert!(renamed.contains("rename from f.txt") && renamed.starts_with(&tracked_patch(&root, "HEAD", false)));
        sh(&root, "git mv g.txt f.txt");

        // A commit of its own: the branch's patch has it, the working one does not.
        sh(&root, "git commit -qm task");
        let mut read = refresh(&root, None, false, &mut known);
        assert_eq!((read.info.ahead, read.info.last_subject.as_str()), (1, "task"), "the commit is seen, what was known of the one before is not kept");
        let patches = read.patches.take().unwrap();
        let (working, branch) = (DiffFile::parse(&patches.working).unwrap(), DiffFile::parse(&patches.branch).unwrap());
        assert!(!working.iter().any(|f| f.path().as_ref() == "src/staged.txt") && branch.iter().any(|f| f.path().as_ref() == "src/staged.txt"));
        assert!(branch.iter().any(|f| f.path().as_ref() == "new one.txt"), "untracked files are in both");
        // Compared with a branch picked by hand.
        assert_eq!(refresh(&root, Some("main".into()), false, &mut known).base.as_deref(), Some("main"));
        // The base moves (a fetch): how far behind it is, is counted again.
        sh(&base, "git clone -q remote.git c 2>/dev/null; cd c; git config user.email t@t; git config user.name t; echo z > z.txt; git add z.txt; git commit -qm more; git push -q origin main");
        sh(&root, "git fetch -q");
        assert_eq!(refresh(&root, None, false, &mut known).info.behind_base, 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(all(test, unix))]
mod blame_tests {
    use super::*;

    #[test]
    fn lines_of_one_commit_share_what_is_known_of_it() {
        let base = std::env::temp_dir().join(format!("ideae-blame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let script = "git init -q -b main .; git config user.email t@t; git config user.name Ann; printf 'one\\ntwo\\nthree\\n' > f.txt; git add f.txt; git commit -qm first
            git config user.name Bob; printf 'one\\nTWO\\nthree\\n' > f.txt; git commit -qam second";
        assert!(Command::new("bash").arg("-ec").arg(script).current_dir(&base).status().unwrap().success());
        let lines = blame(&base, &base.join("f.txt"));
        let told: Vec<(&str, &str)> = lines.iter().map(|l| l.as_ref().map(|l| (l.author.as_str(), l.summary.as_str())).unwrap()).collect();
        assert_eq!(told, [("Ann", "first"), ("Bob", "second"), ("Ann", "first")]);
        assert!(std::sync::Arc::ptr_eq(lines[0].as_ref().unwrap(), lines[2].as_ref().unwrap()));
        assert!(lines[0].as_ref().unwrap().time > 0 && lines[1].as_ref().unwrap().hash.len() == 40);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(all(test, unix))]
mod edge_tests {
    use super::*;

    fn sh(dir: &Path, script: &str) {
        let ok = Command::new("bash").arg("-ec").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    }

    fn out(dir: &Path, script: &str) -> String {
        String::from_utf8_lossy(&Command::new("bash").arg("-ec").arg(script).current_dir(dir).output().unwrap().stdout).into_owned()
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ideae-edge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, "git init -q -b main .; git config user.email t@t; git config user.name t");
        dir.canonicalize().unwrap()
    }

    /// One change of a file that is also renamed and made executable is
    /// taken back, staged and unstaged alone: the name and the mode stay.
    #[test]
    fn a_change_of_a_renamed_file_alone() {
        let dir = repo("rename");
        sh(&dir, "seq 1 30 > old.txt; git add old.txt; git commit -qm base
            git mv old.txt new.txt; chmod +x new.txt; sed -i.bak -e 's/^2$/two/' -e 's/^28$/twenty-eight/' new.txt; rm new.txt.bak");
        let found = hunks(&working_patch(&dir));
        assert_eq!(found.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), ["new.txt", "new.txt"]);
        apply_hunk(&dir, &found[0].patch(), true, false).unwrap();
        assert!(!dir.join("old.txt").exists(), "the rename is not taken back with the line");
        assert_eq!(out(&dir, "sed -n 2p new.txt; test -x new.txt && echo x").trim(), "2\nx", "the line is, the mode is not");
        // The other one into the index and out again, the file there under its new name.
        let left = hunks(&working_patch(&dir));
        stage_change(&dir, &left[0], true).unwrap();
        assert!(out(&dir, "git diff --cached").contains("+twenty-eight"));
        stage_change(&dir, &left[0], false).unwrap();
        assert!(!out(&dir, "git diff --cached").contains("+twenty-eight") && out(&dir, "git status --short").starts_with("R"), "unstaged, still renamed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names git would quote, a file that is not UTF-8, a repository of its
    /// own in a directory, and a user whose git shows diffs their own way.
    #[test]
    fn names_encodings_and_the_users_own_git_settings() {
        let dir = repo("names");
        sh(&dir, "seq 1 9 > 'é.rs'; seq 1 9 > 'sp ace.rs'; seq 1 9 > ok.txt; printf 'caf\\xe9\\nline\\n' > latin.txt; git add .; git commit -qm base
            for f in 'é.rs' 'sp ace.rs' ok.txt; do sed -i.bak 's/^5$/five/' \"$f\"; rm \"$f.bak\"; done; printf 'caf\\xe9\\nmore\\n' > latin.txt
            mkdir nested; git init -q nested; echo x > nested/x.txt
            git config diff.noprefix true; git config diff.mnemonicPrefix true; git config color.ui always; git config diff.context 0");
        let read = refresh(&dir, None, false, &mut Known::default());
        assert!(!read.files.iter().any(|file| file.starts_with("nested")), "another repository's directory is no file of this one");
        let patches = read.patches.unwrap();
        // The Latin-1 file is shown, lossy, and the others with it.
        for name in ["é.rs", "sp ace.rs", "ok.txt", "latin.txt"] {
            assert!(patches.working.contains(&format!("+++ b/{name}")), "{name} in the patch");
        }
        let found = hunks(&patches.working);
        let mut paths: Vec<&str> = found.iter().map(|h| h.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["ok.txt", "sp ace.rs", "é.rs"], "a change of each, none of the file that would not apply");
        let lines = first_changed_lines(&patches.working);
        assert!(lines.contains_key("é.rs") && lines.contains_key("sp ace.rs"), "{lines:?}");
        for hunk in &found {
            stage_change(&dir, hunk, true).unwrap_or_else(|e| panic!("staging {}: {e}", hunk.path));
        }
        let staged = hunks(&refresh(&dir, None, false, &mut Known::default()).patches.unwrap().staged);
        assert_eq!(staged.len(), 3, "and found staged, whatever prefixes and colours the user's git has");
        stage_change(&dir, &found[0], false).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Work that is not committed and would not fit back on the new base:
    /// the rebase is not begun, and the files are as the agent left them.
    #[test]
    fn a_rebase_that_would_break_uncommitted_work_is_not_begun() {
        let dir = repo("rebase");
        sh(&dir, "printf 'one\\ntwo\\nthree\\n' > f.txt; echo x > g.txt; git add .; git commit -qm base; git branch base
            git checkout -qb task; echo mine > mine.txt; git add mine.txt; git commit -qm mine
            git checkout -q base; printf 'one\\nTWO\\nthree\\n' > f.txt; git commit -qam theirs; git checkout -q task");
        // The same line as the base changed, not committed.
        sh(&dir, "printf 'one\\n2\\nthree\\n' > f.txt");
        let head = out(&dir, "git rev-parse HEAD");
        assert!(matches!(rebase_onto(&dir, "base"), Err(RebaseError::Conflicts(files)) if files == ["f.txt"]));
        assert_eq!(out(&dir, "cat f.txt"), "one\n2\nthree\n", "no conflict markers in it");
        assert_eq!((out(&dir, "git rev-parse HEAD"), out(&dir, "git stash list")), (head, String::new()), "nothing moved, nothing set aside");
        // Work that does fit goes along.
        sh(&dir, "git checkout -q f.txt; echo more >> g.txt");
        assert!(rebase_onto(&dir, "base").is_ok());
        assert_eq!((out(&dir, "sed -n 2p f.txt").trim(), out(&dir, "tail -1 g.txt").trim()), ("TWO", "more"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Before the first commit what is added shows as new; and a file whose
    /// changes are megabytes is named, not shown.
    #[test]
    fn a_repository_without_commits_and_a_file_too_large_to_show() {
        let dir = repo("first");
        sh(&dir, "echo one > a.txt; git add a.txt; echo two > b.txt");
        let patches = refresh(&dir, None, false, &mut Known::default()).patches.unwrap();
        assert!(patches.working.contains("+++ b/a.txt") && patches.working.contains("+one") && patches.working.contains("+++ b/b.txt"), "{}", patches.working);

        sh(&dir, "git add .; git commit -qm base; seq 1 300000 > big.txt; echo three >> a.txt; git add big.txt; git commit -qm big; seq 300001 600000 > big.txt; echo more >> b.txt");
        let working = refresh(&dir, None, false, &mut Known::default()).patches.unwrap().working;
        assert!(working.len() < 10_000 && working.contains("Binary files a/big.txt and b/big.txt differ"), "{} bytes", working.len());
        assert!(working.contains("+more"), "the file next to it is shown");
        let files = gpui_kit::component::diff::DiffFile::parse(&working).unwrap();
        assert_eq!(files.iter().map(|f| f.path().to_string()).collect::<Vec<_>>(), ["a.txt", "b.txt", "big.txt"]);
        assert!(hunks(&working).iter().all(|hunk| hunk.path != "big.txt"), "no single change of it to take back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The commit a pull is tried against leaves the repository's index
    /// alone, and is made while something else holds its lock.
    #[test]
    fn the_worktree_is_committed_without_touching_the_index() {
        let dir = repo("index");
        sh(&dir, "echo one > f.txt; git add f.txt; git commit -qm base");
        assert_eq!(worktree_commit(&dir).as_deref(), Some("HEAD"), "clean");
        sh(&dir, "echo two >> f.txt; echo new > staged.txt; git add staged.txt; echo untracked > u.txt");
        let index = dir.join(".git/index");
        let before = (stamp(&index), std::fs::read(&index).unwrap());
        std::fs::write(dir.join(".git/index.lock"), "").unwrap();
        let made = worktree_commit(&dir).expect("made with the index locked");
        std::fs::remove_file(dir.join(".git/index.lock")).unwrap();
        assert!(before == (stamp(&index), std::fs::read(&index).unwrap()), "the index is as it was");
        assert_eq!(out(&dir, &format!("git ls-tree -r --name-only {made}")).lines().collect::<Vec<_>>(), ["f.txt", "staged.txt"], "what is tracked, as on disk");
        assert_eq!(out(&dir, &format!("git show {made}:f.txt")), "one\ntwo\n");
        assert!(out(&dir, "git stash list").is_empty() && out(&dir, "git status --short").contains("A  staged.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
