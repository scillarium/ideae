use super::*;

impl TaskView {
    /// Ctrl+PgUp/PgDn: the neighbouring tab in the focused pane, wrapping.
    /// Where you are: the file and cursor, or the tab that is no file.
    pub(super) fn nav_point(&self, cx: &App) -> Option<NavPoint> {
        let tab = self.tabs.get(self.focused_ix()?)?;
        Some(match &tab.kind {
            TabKind::File(file) => {
                let f = file.read(cx);
                NavPoint::File { path: f.path.clone(), line: f.cursor_line, col: f.cursor_col }
            }
            _ => NavPoint::Tab(tab.id),
        })
    }

    /// Called before a jump: remembers the place it leaves.
    pub(super) fn record_nav(&mut self, cx: &mut Context<Self>) {
        if self.navigating {
            return;
        }
        let Some(point) = self.nav_point(cx) else { return };
        if !self.nav_back.last().is_some_and(|last| last.same_place(&point)) {
            self.nav_back.push(point);
            if self.nav_back.len() > NAV_LIMIT {
                self.nav_back.remove(0);
            }
        }
        self.nav_forward.clear();
        cx.notify();
    }

    pub(super) fn nav_reachable(&self, point: &NavPoint) -> bool {
        match point {
            NavPoint::File { path, .. } => path.is_file(),
            NavPoint::Tab(id) => self.ix_of(*id).is_some(),
        }
    }

    /// For the buttons, drawn every frame: without asking the disk whether
    /// the files are still there, as `navigate` does.
    pub fn can_navigate(&self, back: bool) -> bool {
        let stack = if back { &self.nav_back } else { &self.nav_forward };
        stack.iter().any(|p| match p {
            NavPoint::File { .. } => true,
            NavPoint::Tab(id) => self.ix_of(*id).is_some(),
        })
    }

    /// Mouse Back/Forward, Alt+Left/Right: the place before (or after) the
    /// last jump, a tab or a spot in a file.
    pub fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.nav_point(cx);
        let target = loop {
            let stack = if back { &mut self.nav_back } else { &mut self.nav_forward };
            let Some(point) = stack.pop() else { return };
            if here.as_ref().is_some_and(|h| h.same_place(&point)) || !self.nav_reachable(&point) {
                continue;
            }
            break point;
        };
        if let Some(here) = here {
            let other = if back { &mut self.nav_forward } else { &mut self.nav_back };
            other.push(here);
        }
        self.navigating = true;
        match target {
            NavPoint::File { path, line, col } => self.open_file_at(path, line, col, window, cx),
            NavPoint::Tab(id) => {
                if let Some(ix) = self.ix_of(id) {
                    self.activate(ix, window, cx);
                }
            }
        }
        self.navigating = false;
        cx.notify();
    }

    /// Typing in a file: a place for Last edit location. Lines near the last
    /// one are the same edit, as in JetBrains.
    pub(super) fn record_edit(&mut self, path: PathBuf, line: u32, col: u32) {
        self.edits_back = 0;
        if let Some(NavPoint::File { path: last, line: last_line, col: last_col }) = self.edits.last_mut()
            && *last == path
            && last_line.abs_diff(line) <= 2
        {
            (*last_line, *last_col) = (line, col);
            return;
        }
        self.edits.push(NavPoint::File { path, line, col });
        if self.edits.len() > EDITS_KEPT {
            self.edits.remove(0);
        }
    }

    /// Ctrl+Shift+Backspace: where you typed last, then, pressed again, the
    /// place before it. Back returns to where it was pressed.
    pub fn last_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.nav_point(cx);
        let found = self.edits.iter().rev().enumerate().skip(self.edits_back).find(|(_, point)| {
            !here.as_ref().is_some_and(|h| h.same_place(point)) && self.nav_reachable(point)
        });
        let Some((back, NavPoint::File { path, line, col })) = found.map(|(back, point)| (back, point.clone())) else { return };
        self.edits_back = back + 1;
        self.record_nav(cx);
        self.navigating = true;
        self.open_file_at(path, line, col, window, cx);
        self.navigating = false;
    }

    pub fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let in_pane = self.in_pane(self.current);
        let Some(current) = self.focused_ix().and_then(|ix| in_pane.iter().position(|&i| i == ix)) else { return };
        let next = if forward { (current + 1) % in_pane.len() } else { (current + in_pane.len() - 1) % in_pane.len() };
        self.activate(in_pane[next], window, cx);
    }
}
