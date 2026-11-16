//! The Files panel: the worktree as a tree, each name in the color of what
//! git says of it, as JetBrains does. What `.gitignore` keeps out is there
//! too: git names an ignored folder once (`target/`), and what is in it is
//! read from disk when it is opened.

use super::*;

/// What the color of a name in the tree tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Modified,
    /// New, and added to git.
    Added,
    /// New, and not added to git.
    Untracked,
    Conflict,
    Ignored,
    /// Gone from disk: the Changes list shows it, the tree leaves it out.
    Deleted,
}

impl Mark {
    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        // On a light background the themes' blue and green are pale for text.
        let deepen = |color: Hsla| if dark { color } else { hsla(color.h, color.s, color.l * 0.8, color.a) };
        match self {
            Mark::Modified => deepen(theme.blue),
            Mark::Added => deepen(theme.green),
            Mark::Conflict => theme.red,
            // Brown-red as JetBrains' "unversioned", apart from a conflict's red.
            Mark::Untracked => {
                let red = theme.red;
                hsla((red.h + 0.04).fract(), red.s * 0.8, if dark { red.l } else { red.l * 0.8 }, 1.)
            }
            // Olive, dimmer than any name worked on.
            Mark::Ignored => hsla(theme.yellow.h, theme.yellow.s * 0.55, if dark { 0.55 } else { 0.4 }, 1.),
            Mark::Deleted => theme.muted_foreground,
        }
    }
}

/// A file's name in the color of its mark; unmarked, with its extension dimmed.
/// `dim`: seen already, as a viewed change.
pub(super) fn marked_label(name: SharedString, mark: Option<Mark>, dim: bool, cx: &App) -> StyledText {
    match mark {
        Some(mark) => {
            let color = mark.color(cx);
            let color = if dim { color.opacity(0.6) } else { color };
            let all = 0..name.len();
            StyledText::new(name).with_highlights([(all, HighlightStyle { color: Some(color), ..Default::default() })])
        }
        None => file_label(name, cx),
    }
}

impl From<git::Change> for Mark {
    fn from(change: git::Change) -> Self {
        match change {
            git::Change::Modified => Mark::Modified,
            git::Change::Added => Mark::Added,
            git::Change::Untracked => Mark::Untracked,
            git::Change::Conflict => Mark::Conflict,
            git::Change::Deleted => Mark::Deleted,
        }
    }
}

/// The marks of the changed files and of the folders they are in, by tree
/// id (a folder's ends in `/`). A folder with only new files in it is new
/// itself; one that holds a file git had before is modified. `files` is
/// sorted: a folder's files come together.
pub(super) fn tree_marks(states: &HashMap<String, git::FileState>, files: &[String]) -> HashMap<String, Mark> {
    let mut marks: HashMap<String, Mark> = HashMap::with_capacity(states.len());
    let mut folders: HashSet<&str> = HashSet::new();
    for (path, state) in states {
        marks.insert(path.clone(), state.change.into());
        folders.extend(path.match_indices('/').map(|(slash, _)| &path[..=slash]));
    }
    for folder in folders {
        let start = files.partition_point(|file| file.as_str() < folder);
        let mut mark = Mark::Untracked;
        for file in files[start..].iter().take_while(|file| file.starts_with(folder)) {
            match states.get(file).map(|state| state.change) {
                Some(git::Change::Untracked) => {}
                Some(git::Change::Added) => mark = Mark::Added,
                _ => {
                    mark = Mark::Modified;
                    break;
                }
            }
        }
        marks.insert(folder.to_string(), mark);
    }
    marks
}

/// What is in an ignored folder, read from disk: names, and whether each
/// is a folder; folders first.
pub(super) type Listing = Rc<Vec<(SharedString, bool)>>;

/// The most entries of an ignored folder the tree shows.
const LISTED: usize = 5000;

/// Id of the row standing in for what is not read yet, or not shown.
fn filler(folder: &str, what: &str) -> TreeItem {
    TreeItem::new(format!("{folder}\0"), what.to_string())
}

/// Nests sorted relative paths into tree items. Folder ids end in `/`, and
/// the folders in `expanded` stay open across rebuilds. The ignored folders
/// show what `listed` has of them; those open and not read yet are returned
/// to be read.
pub(super) fn build_tree(
    files: &[String],
    gone: &HashSet<String>,
    ignored: &[String],
    listed: &HashMap<SharedString, Listing>,
    expanded: &HashSet<SharedString>,
) -> (Vec<TreeItem>, Vec<SharedString>) {
    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: Vec<String>,
        /// Ignored: what is in it comes from disk.
        ignored: bool,
    }
    let mut root = Node::default();
    let mut add = |path: &String| {
        let mut node = &mut root;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            match parts.peek() {
                // `target/`: the folder itself.
                Some(&"") => {
                    node.dirs.entry(part.to_string()).or_default().ignored = true;
                    break;
                }
                Some(_) => node = node.dirs.entry(part.to_string()).or_default(),
                None => node.files.push(path.clone()),
            }
        }
    };
    // Deleted and not staged so, git lists it still: not on disk, not in the tree.
    files.iter().filter(|file| !gone.contains(*file)).chain(ignored).for_each(&mut add);

    fn from_disk(id: &SharedString, listed: &HashMap<SharedString, Listing>, expanded: &HashSet<SharedString>, unread: &mut Vec<SharedString>) -> Vec<TreeItem> {
        let Some(entries) = listed.get(id) else {
            if expanded.contains(id) {
                unread.push(id.clone());
            }
            return vec![filler(id, "…")];
        };
        let mut out: Vec<TreeItem> = entries
            .iter()
            .take(LISTED)
            .map(|(name, folder)| {
                if *folder {
                    let child = SharedString::from(format!("{id}{name}/"));
                    let open = expanded.contains(&child);
                    TreeItem::new(child.clone(), name.clone()).children(from_disk(&child, listed, expanded, unread)).expanded(open)
                } else {
                    TreeItem::new(format!("{id}{name}"), name.clone())
                }
            })
            .collect();
        if entries.len() > LISTED {
            out.push(filler(id, &format!("{} more", entries.len() - LISTED)));
        }
        out
    }

    fn items(node: &Node, prefix: &str, listed: &HashMap<SharedString, Listing>, expanded: &HashSet<SharedString>, unread: &mut Vec<SharedString>) -> Vec<TreeItem> {
        let mut out = Vec::new();
        for (name, child) in &node.dirs {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let id = SharedString::from(format!("{path}/"));
            let open = expanded.contains(&id);
            let children = if child.ignored { from_disk(&id, listed, expanded, unread) } else { items(child, &path, listed, expanded, unread) };
            out.push(TreeItem::new(id, name.clone()).children(children).expanded(open));
        }
        // Ignored files come after the others: sorted again with them.
        let mut files: Vec<&String> = node.files.iter().collect();
        if !files.is_sorted() {
            files.sort();
        }
        for file in files {
            let name = file.rsplit('/').next().unwrap_or(file);
            out.push(TreeItem::new(file.clone(), name.to_string()));
        }
        out
    }
    let mut unread = Vec::new();
    let items = items(&root, "", listed, expanded, &mut unread);
    (items, unread)
}

/// What is in `folder`, folders first, each by name.
fn list(folder: &std::path::Path) -> Vec<(SharedString, bool)> {
    let mut entries: Vec<(SharedString, bool)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned().into(), entry.file_type().is_ok_and(|t| t.is_dir())))
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries
}

impl TaskView {
    /// Whether the tree row `id` is an ignored folder or in one.
    pub(super) fn in_ignored(&self, id: &str) -> bool {
        self.tree_ignored.iter().any(|ignored| ignored.ends_with('/') && id.starts_with(ignored.as_str()))
    }

    pub(super) fn apply_ignored(&mut self, ignored: Vec<String>, cx: &mut Context<Self>) {
        if *self.tree_ignored == ignored {
            return;
        }
        self.tree_ignored = Rc::new(ignored);
        let ignored = self.tree_ignored.clone();
        self.ignored_listed.retain(|id, _| ignored.iter().any(|folder| folder.ends_with('/') && id.starts_with(folder.as_str())));
        self.tree_stale = true;
        self.fill_tree(cx);
        cx.notify();
    }

    /// Whether the files gone from disk are others: the tree is to be built again.
    pub(super) fn mark_tree(&mut self) -> bool {
        self.tree_marks = Rc::new(tree_marks(&self.file_states, &self.tree_files));
        let gone: HashSet<String> = self.file_states.iter().filter(|(_, state)| state.change == git::Change::Deleted).map(|(path, _)| path.clone()).collect();
        if *self.tree_gone == gone {
            return false;
        }
        self.tree_gone = Rc::new(gone);
        true
    }

    /// While presenting with ignored files hidden, the tree leaves them out:
    /// their names (`.env.local`) tell enough.
    pub(super) fn hide_ignored_in_tree(&mut self, cx: &mut Context<Self>) {
        let hide = crate::workspace::present::on() && crate::state::with_settings(|s| s.present_veil);
        if hide != self.tree_hides_ignored {
            self.tree_hides_ignored = hide;
            self.ignored_listed.clear();
            self.tree_stale = true;
            self.fill_tree(cx);
            cx.notify();
        }
    }

    pub(super) fn tree_expanded(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        self.expanded.insert(id.clone());
        // Read again on every opening: a build writes there all the time.
        if self.in_ignored(id) {
            self.list_ignored(id.clone(), cx);
        }
    }

    pub(super) fn tree_collapsed(&mut self, id: &SharedString) {
        self.expanded.remove(id);
        // Closed, what it showed is let go of: read again when opened.
        self.ignored_listed.retain(|listed, _| !listed.starts_with(id.as_ref()));
    }

    /// Reads an ignored folder off the UI thread, and shows it.
    pub(super) fn list_ignored(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if !self.ignored_reading.insert(id.clone()) {
            return;
        }
        let folder = self.root.join(id.as_ref());
        cx.spawn(async move |this, cx| {
            let entries = cx.background_spawn(async move { list(&folder) }).await;
            let _ = this.update(cx, |this, cx| {
                this.ignored_reading.remove(&id);
                // Closed or no longer ignored meanwhile: not wanted any more.
                if !this.expanded.contains(&id) || !this.in_ignored(&id) {
                    return;
                }
                if this.ignored_listed.get(&id).is_some_and(|seen| **seen == entries) {
                    return;
                }
                this.ignored_listed.insert(id, Rc::new(entries));
                this.tree_stale = true;
                this.fill_tree(cx);
            });
        })
        .detach();
    }

    /// The Files panel's tree, built when the panel is the one shown: it
    /// holds every path again, several times over, in every open task.
    pub(super) fn fill_tree(&mut self, cx: &mut Context<Self>) {
        let shown = self.visible && self.right == RightPane::Files;
        if self.tree_stale && shown {
            self.tree_stale = false;
            let ignored: &[String] = if self.tree_hides_ignored { &[] } else { &self.tree_ignored };
            let (items, unread) = build_tree(&self.tree_files, &self.tree_gone, ignored, &self.ignored_listed, &self.expanded);
            self.tree.update(cx, |t, cx| t.set_items(items, cx));
            for id in unread {
                self.list_ignored(id, cx);
            }
        } else if !self.tree_stale && !shown {
            // Let go of when not shown: every path again, in every task that showed it once.
            self.tree_stale = true;
            self.ignored_listed.clear();
            self.tree.update(cx, |t, cx| t.set_items(Vec::new(), cx));
        }
    }

    pub(super) fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        let root = self.root.clone();
        let marks = self.tree_marks.clone();
        let ignored = if self.tree_hides_ignored { Rc::default() } else { self.tree_ignored.clone() };
        let touched = self.agent_touched.clone();
        let editing = self.agent_editing.clone();
        div().flex_1().min_h_0().child(
            tree(&self.tree, move |ix, entry, selected, _, cx| {
                let item = entry.item();
                let row = ListItem::new(ix).w_full().py_0p5().px_2().pl(px(16.) * entry.depth() + px(4.)).selected(selected);
                // "…" while a folder is read, or what more it has than is shown.
                if item.id.ends_with('\0') {
                    return row.child(h_flex().text_sm().pl(px(40.)).text_color(cx.theme().muted_foreground).child(item.label.clone()));
                }
                // An ignored folder read empty has no rows under it: a folder still.
                let folder = item.id.ends_with('/');
                let mark = marks.get(item.id.as_ref()).copied().or_else(|| {
                    ignored.iter().any(|ignored| item.id.starts_with(ignored.as_str()) && (ignored.ends_with('/') || ignored.len() == item.id.len())).then_some(Mark::Ignored)
                });
                let label = if folder && mark.is_none() { StyledText::new(item.label.clone()) } else { marked_label(item.label.clone(), mark, false, cx) };
                // Folders have one icon, open or not; the chevron before it tells.
                let chevron = div().size_4().flex_none().flex().items_center().justify_center().when(entry.is_folder(), |this| {
                    let chevron = if entry.is_expanded() { IconName::ChevronDown } else { IconName::ChevronRight };
                    this.child(gpui_kit::component::Icon::new(chevron).size_3p5().text_color(cx.theme().muted_foreground))
                });
                let icon = crate::file_icons::icon(if folder { crate::file_icons::folder(&item.label) } else { crate::file_icons::file_kept(&item.label) }, cx);
                // The agent's work; a folder leads to the file being written.
                let on_the_way = editing.as_deref().is_some_and(|e| e.strip_prefix(item.id.as_ref()).is_some_and(|rest| rest.is_empty() || (folder && !rest.is_empty())));
                let agent = if on_the_way && !folder {
                    Some("editing")
                } else if on_the_way || touched.contains(&item.id) {
                    Some("●")
                } else {
                    None
                };
                let path = root.join(item.id.as_ref());
                let view = view.clone();
                row.child(
                    h_flex()
                        .gap_1p5()
                        .text_sm()
                        .child(chevron)
                        .child(icon)
                        .child(div().truncate().child(label))
                        .when_some(agent, |this, mark| this.child(div().text_xs().text_color(gpui_kit::blue()).child(mark))),
                )
                .on_click(move |event, window, cx| {
                    if folder {
                        return;
                    }
                    let path = path.clone();
                    let how = if event.click_count() > 1 { OpenAs::Kept } else { OpenAs::Preview };
                    let _ = view.update(cx, |t, cx| t.open_file_as(path, 0, 0, how, window, cx));
                })
            })
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Listing, Mark, build_tree, git, tree_marks};
    use gpui_kit::{SharedString, component::tree::TreeItem};
    use std::{
        collections::{HashMap, HashSet},
        rc::Rc,
    };

    fn state(change: git::Change) -> git::FileState {
        git::FileState { staged: false, unstaged: true, change }
    }

    #[test]
    fn a_folder_is_new_only_when_all_in_it_is() {
        let files: Vec<String> = ["a/new/one.rs", "a/new/two.rs", "a/old.rs", "b/added.rs", "b/new.rs"].map(String::from).to_vec();
        let states: HashMap<String, git::FileState> = [
            ("a/new/one.rs", git::Change::Untracked),
            ("a/new/two.rs", git::Change::Untracked),
            ("b/added.rs", git::Change::Added),
            ("b/new.rs", git::Change::Untracked),
        ]
        .map(|(path, change)| (path.to_string(), state(change)))
        .into();
        let marks = tree_marks(&states, &files);
        assert_eq!(marks.get("a/new/one.rs"), Some(&Mark::Untracked));
        assert_eq!(marks.get("a/new/"), Some(&Mark::Untracked));
        assert_eq!(marks.get("a/"), Some(&Mark::Modified), "a/old.rs is git's");
        assert_eq!(marks.get("b/"), Some(&Mark::Added));
        assert_eq!(marks.get("a/old.rs"), None);
    }

    #[test]
    fn ignored_folders_are_read_when_open() {
        let files: Vec<String> = ["src/main.rs", "web/app.js"].map(String::from).to_vec();
        let ignored: Vec<String> = ["target/", "web/node_modules/", ".env"].map(String::from).to_vec();
        let ids = |items: &[TreeItem]| items.iter().map(|i| i.id.to_string()).collect::<Vec<_>>();
        let mut expanded = HashSet::new();
        let gone: HashSet<String> = ["src/main.rs".to_string()].into();
        let (items, _) = build_tree(&files, &gone, &ignored, &HashMap::new(), &expanded);
        assert_eq!(ids(&items), ["target/", "web/", ".env"], "a deleted file is not in the tree");
        let none = HashSet::new();
        let (items, unread) = build_tree(&files, &none, &ignored, &HashMap::new(), &expanded);
        assert_eq!(ids(&items), ["src/", "target/", "web/", ".env"]);
        assert_eq!(ids(&items[1].children), ["target/\0"], "not read yet");
        assert!(unread.is_empty(), "nothing open");
        assert_eq!(ids(&items[2].children), ["web/node_modules/", "web/app.js"]);

        expanded.insert(SharedString::from("target/"));
        let (_, unread) = build_tree(&files, &none, &ignored, &HashMap::new(), &expanded);
        assert_eq!(unread, [SharedString::from("target/")]);

        let listed: HashMap<SharedString, Listing> = [(SharedString::from("target/"), Rc::new(vec![("debug".into(), true), ("x.log".into(), false)]))].into();
        let (items, unread) = build_tree(&files, &none, &ignored, &listed, &expanded);
        assert!(unread.is_empty());
        assert_eq!(ids(&items[1].children), ["target/debug/", "target/x.log"]);
        assert_eq!(ids(&items[1].children[0].children), ["target/debug/\0"]);
    }
}
