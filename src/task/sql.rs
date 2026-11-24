//! A `.sql` file that runs: a line over its text with the connection and the
//! database, and the results under it, as wide as the file. Only what is
//! asked for runs: the selection, else the statement at the cursor.

use std::{cell::RefCell, collections::VecDeque, ops::Range};

use gpui_kit::component::input::{RangeDecoration, RangeDecorationCollection, RangeDecorationStyle};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::table::{ColumnSort, DataTable, TableEvent, TableState};

use super::sql_grid::{SqlRows, StagedEdit, search_hits};
use super::sql_schema::{SchemaRow, SharedSchema};
use super::*;
use crate::db::{
    config::{self, Driver},
    export::{self, Format},
    session::{ColumnInfo, ColumnSource, Event, MOST_ROWS, SchemaState, Session},
    statement,
};

actions!(ideae_sql, [RunStatement, RunFile, StopStatement, ExplainStatement, SqlDefinition, ShowTableData, ShowTableDefinition]);

/// The key context of a `.sql` file's editor.
pub const SQL_CONTEXT: &str = "SqlFile";
/// Where its keys work: Ctrl+Enter is a new line in any other input.
pub const SQL_INPUT_CONTEXT: &str = "SqlFile > Input";

/// The text's share of the height over the results, unless dragged.
const TEXT_SHARE: f32 = 0.45;
const LEAST_SHARE: f32 = 0.12;
/// Characters of a value its dialog holds.
const VALUE_SHOWN: usize = 200_000;
/// Lines the Output tab keeps.
const LOG_KEPT: usize = 400;
/// Scratch files its menu lists.
const SCRATCH_LISTED: usize = 12;

pub(super) fn is_sql(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "sql" | "ddl" | "redis"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Off,
    Connecting,
    Ready,
    Running,
}

struct SqlResult {
    title: SharedString,
    statement: Range<usize>,
    /// As it ran: the file may have changed since, and an export runs it again.
    sql: String,
    table: Entity<TableState<SqlRows>>,
    /// Rows still coming.
    open: bool,
    /// Cut at the table's limit: a file of it needs the statement run again.
    cut: bool,
    said: Option<SharedString>,
    /// Kept when the next statement runs.
    pinned: bool,
    /// How to read it again with a filter or from another row.
    origin: Option<Origin>,
    /// Each row's fields, one under another, in place of the table.
    row_view: bool,
    /// The connection and database it was read from. A pinned result stays
    /// when the file moves to another: nothing of it may then go to that one.
    from: (Option<String>, Option<String>),
}

/// A statement that only reads, and the window onto it the table shows.
#[derive(Clone, Default)]
struct Origin {
    base: String,
    filter: String,
    offset: usize,
    /// A column's place and whether it goes down: the order asked of the server.
    sort: Option<(usize, bool)>,
    /// The primary key's places: rows of equal values stay put from page to page.
    tie_break: Vec<usize>,
}

impl Origin {
    /// The statement for this window. Added to where that can be, as a subquery
    /// would lose its order and fails on a join's two columns of one name.
    fn statement(&self) -> String {
        let base = self.base.trim().trim_end_matches(';').trim_end();
        let filter = self.filter.trim();
        if filter.is_empty() && self.sort.is_none() && self.offset == 0 {
            return base.to_string();
        }
        let words = statement::top_words(base);
        let own_order = words.iter().any(|w| w == "order");
        let own_limit = words.iter().any(|w| w == "limit" || w == "fetch");
        let mut order: Vec<String> = self.sort.iter().map(|(column, down)| format!("{}{}", column + 1, if *down { " DESC" } else { "" })).collect();
        order.extend(self.tie_break.iter().filter(|c| self.sort.is_none_or(|(sorted, _)| sorted != **c)).map(|c| (c + 1).to_string()));
        let window = if self.offset > 0 { format!(" LIMIT {MOST_ROWS} OFFSET {}", self.offset) } else { String::new() };
        if filter.is_empty() && !own_limit && (self.sort.is_none() || !own_order) {
            let by = if own_order || order.is_empty() { String::new() } else { format!(" ORDER BY {}", order.join(", ")) };
            return format!("{base}{by}{window}");
        }
        let condition = if filter.is_empty() { String::new() } else { format!(" WHERE {filter}") };
        let by = if order.is_empty() { String::new() } else { format!(" ORDER BY {}", order.join(", ")) };
        format!("SELECT * FROM ({base}) AS ideae_q{condition}{by}{window}")
    }

    /// Whether rows can move between pages: nothing orders them.
    fn pages_may_shift(&self) -> bool {
        self.offset > 0 && self.sort.is_none() && self.tie_break.is_empty() && !statement::top_words(&self.base).iter().any(|w| w == "order")
    }
}

/// What a change of a cell needs: where it really is, and how to tell its row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EditTarget {
    table: String,
    column: String,
    /// The primary key's columns, by name and by their place in the result.
    key: Vec<(String, usize)>,
}

/// The table, column and key of a result's cell, from what the server says
/// they read. None when it is an expression, of a view, of another database,
/// or its table's whole primary key is not in the result.
fn edit_target(schema: &crate::db::schema::Schema, columns: &[ColumnInfo], column: usize) -> Option<EditTarget> {
    let source: &ColumnSource = columns.get(column)?.source.as_ref()?;
    if source.database.as_ref().is_some_and(|d| !d.eq_ignore_ascii_case(&schema.database)) {
        return None;
    }
    let table = schema.table(&source.table).filter(|t| !t.view)?;
    let names: Vec<&str> = table.columns.iter().filter(|c| c.key == "PRI").map(|c| c.name.as_str()).collect();
    if names.is_empty() {
        return None;
    }
    let key: Option<Vec<(String, usize)>> = names
        .iter()
        .map(|name| columns.iter().position(|c| c.source.as_ref().is_some_and(|s| s.table == source.table && s.database == source.database && s.column == *name)).map(|at| ((*name).to_string(), at)))
        .collect();
    Some(EditTarget { table: source.table.clone(), column: source.column.clone(), key: key? })
}

/// The cells a summary is of.
type SelectionKey = (usize, Range<usize>, Range<usize>, usize);

/// How a selection is copied.
#[derive(Clone, Copy)]
enum CopyAs {
    Format(Format),
    InList,
}

/// Which row and column a change of a cell goes to.
#[derive(Clone)]
struct CellTarget {
    table: String,
    column: String,
    key: Vec<(String, String)>,
}

/// What the panel under the text shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Output,
    Schema,
    History,
    Result(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogKind {
    Statement,
    Info,
    Failed,
}

/// A line of the Output tab.
struct LogLine {
    at: SharedString,
    text: SharedString,
    kind: LogKind,
    /// Where in the file a failed statement is.
    place: Option<Range<usize>>,
}

pub(super) struct SqlFile {
    connection: Option<String>,
    pub(super) database: Option<String>,
    databases: Vec<String>,
    version: Option<SharedString>,
    /// Whether the connection as made is encrypted.
    encrypted: bool,
    status: Status,
    session: Option<Session>,
    /// What waits for the connection to be made.
    pending: Option<Vec<(Range<usize>, String)>>,
    /// The window onto a statement that the next result is.
    next_origin: Option<Origin>,
    /// The filter over the result shown.
    filter_input: Option<Entity<gpui_kit::component::input::InputState>>,
    /// The values parameters had last.
    parameters: std::collections::HashMap<String, String>,
    history: Rc<Vec<crate::db::history::Entry>>,
    /// The result whose cell changes are on their way to the server.
    applying: Option<usize>,
    applying_added: bool,
    /// To read again when the run ends: rows were added, and the server gave them their keys.
    reread: Option<usize>,
    /// The next run adds its result to those there.
    keep_results: bool,
    /// What the selection in the table adds up to.
    summary: Option<(SelectionKey, crate::db::value::Summary)>,
    _summary: Task<()>,
    /// The statement the cursor is in, framed; where the last error is, filled.
    cursor_mark: Option<RangeDecorationCollection>,
    error_mark: Option<RangeDecorationCollection>,
    cursor_seen: (usize, u64),
    _cursor: Task<()>,
    /// The search over the result shown, when it is open.
    search: Option<Entity<gpui_kit::component::input::InputState>>,
    _search: Task<()>,
    /// What the run under way is made of, for its results to say what they came from.
    running: Vec<(Range<usize>, String)>,
    results: Vec<SqlResult>,
    shown: Tab,
    pub(super) schema: SharedSchema,
    pub(super) schema_open: std::collections::HashSet<String>,
    pub(super) schema_filter: Option<Entity<gpui_kit::component::input::InputState>>,
    pub(super) schema_rows: Rc<Vec<SchemaRow>>,
    pub(super) schema_scroll: UniformListScrollHandle,
    /// The schema is being compared with the server, in the background.
    pub(super) schema_checking: bool,
    log: VecDeque<LogLine>,
    log_scroll: ScrollHandle,
    panel: bool,
    /// Statements wait for Commit.
    manual: bool,
    /// In manual mode, something ran that Commit or Rollback has not ended.
    uncommitted: bool,
    mark: Option<RangeDecorationCollection>,
    _pick: Task<()>,
    _events: Task<()>,
    _export: Task<()>,
    /// The schema asked for before the file connected, once for a database.
    /// With whether production was locked then: asked again once it is not.
    schema_asked: Option<(String, String, bool)>,
    _schema: Task<()>,
}

impl SqlFile {
    pub(super) fn new(path: &std::path::Path) -> Self {
        let choice = config::recall(path);
        if crate::db::scratch::is_scratch(path) {
            crate::db::scratch::opened(path.to_path_buf());
        }
        let mut file = Self::with_choice(path, choice);
        let swept = crate::db::scratch::take_swept();
        if !swept.is_empty() {
            file.say(LogKind::Info, format!("Moved to the trash, not opened for {} days: {}", crate::db::scratch::KEPT_DAYS, swept.join(", ")), None);
        }
        file
    }

    fn with_choice(path: &std::path::Path, choice: Option<config::FileChoice>) -> Self {
        SqlFile {
            connection: choice.as_ref().map(|c| c.connection.clone()),
            database: choice.and_then(|c| c.database),
            databases: Vec::new(),
            version: None,
            encrypted: false,
            status: Status::Off,
            session: None,
            pending: None,
            next_origin: None,
            applying: None,
            applying_added: false,
            reread: None,
            keep_results: false,
            summary: None,
            _summary: Task::ready(()),
            cursor_mark: None,
            error_mark: None,
            cursor_seen: (usize::MAX, 0),
            _cursor: Task::ready(()),
            search: None,
            _search: Task::ready(()),
            filter_input: None,
            parameters: Default::default(),
            history: Rc::default(),
            running: Vec::new(),
            results: Vec::new(),
            shown: Tab::Output,
            schema: super::sql_schema::shared_schema(path),
            schema_open: Default::default(),
            schema_filter: None,
            schema_rows: Rc::default(),
            schema_scroll: UniformListScrollHandle::new(),
            schema_checking: false,
            log: VecDeque::new(),
            log_scroll: ScrollHandle::new(),
            panel: false,
            manual: false,
            uncommitted: false,
            mark: None,
            _pick: Task::ready(()),
            _events: Task::ready(()),
            _export: Task::ready(()),
            schema_asked: None,
            _schema: Task::ready(()),
        }
    }

    pub(super) fn connection_id(&self) -> Option<&str> {
        self.connection.as_deref()
    }

    /// Whether what the file is connected to now is what this result came from.
    fn is_from_here(&self, result: &SqlResult) -> bool {
        result.from.0 == self.connection && result.from.1 == self.database
    }

    /// What a result came from, in words, when that is not where the file is now.
    fn elsewhere(&self, result: &SqlResult) -> Option<String> {
        if self.is_from_here(result) {
            return None;
        }
        let name = result.from.0.as_deref().and_then(config::find).map_or_else(|| "a removed connection".to_string(), |c| c.name);
        Some(match &result.from.1 {
            Some(database) => format!("{name} / {database}"),
            None => name,
        })
    }

    /// What closing the file would lose.
    fn holds(&self, cx: &App) -> bool {
        self.uncommitted || self.results.iter().any(|r| r.table.read(cx).delegate().has_changes())
    }

    pub(super) fn busy(&self) -> bool {
        matches!(self.status, Status::Connecting | Status::Running)
    }

    /// With the task: what runs stops, and the connection closes.
    pub(super) fn end(&mut self) {
        self.session = None;
        self.pending = None;
        self.running.clear();
        self.uncommitted = false;
        self._events = Task::ready(());
        self.status = Status::Off;
    }

    fn say(&mut self, kind: LogKind, text: impl Into<SharedString>, place: Option<Range<usize>>) {
        let now = crate::state::now();
        self.log.push_back(LogLine { at: crate::local_time::clock(now).into(), text: text.into(), kind, place });
        while self.log.len() > LOG_KEPT {
            self.log.pop_front();
        }
        self.log_scroll.scroll_to_bottom();
    }
}

/// "1 row", "3 rows".
fn rows_said(count: u64) -> String {
    if count == 1 { "1 row".into() } else { format!("{count} rows") }
}

/// Whether a statement changes what the database holds, so the tree and the completions are old.
fn changes_schema(sql: &str) -> bool {
    let first = sql.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    matches!(first.as_str(), "create" | "alter" | "drop" | "rename" | "truncate")
}

/// The database of a `USE name`.
fn used_database(sql: &str) -> Option<String> {
    let mut words = sql.split_whitespace();
    words.next().filter(|w| w.eq_ignore_ascii_case("use"))?;
    let name = words.next()?.trim_matches(|c| matches!(c, '`' | '"' | ';'));
    (words.next().is_none() && !name.is_empty()).then(|| name.to_string())
}

/// Whether the server turned the login down, as against being out of reach.
fn login_refused(message: &str) -> bool {
    ["Access denied", "password authentication failed", "WRONGPASS", "NOAUTH", "no password supplied"].iter().any(|m| message.contains(m))
}

/// Whether running it again only reads: what an export of a cut result does.
fn only_reads(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    matches!(first, "select" | "show" | "desc" | "describe" | "explain" | "table" | "values")
        && !lower.split_whitespace().any(|w| w == "into")
        && !lower.contains("for update")
        && !lower.contains("for share")
}

/// What a run takes from the text.
fn pick(driver: Driver, text: &str, selection: Range<usize>, cursor: usize) -> Vec<(Range<usize>, String)> {
    if !selection.is_empty()
        && let Some(selected) = text.get(selection.clone())
    {
        return statement::split_for(driver, selected)
            .into_iter()
            .map(|r| (r.start + selection.start..r.end + selection.start, selected[r].to_string()))
            .collect();
    }
    statement::at(text, &statement::split_for(driver, text), cursor).map(|r| (r.clone(), text[r].to_string())).into_iter().collect()
}

/// A result's name on its tab: the table it reads, else its first word.
fn title_of(sql: &str) -> String {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let from = words.iter().position(|w| w.eq_ignore_ascii_case("from")).and_then(|i| words.get(i + 1));
    let name = from.or(words.first()).copied().unwrap_or("result");
    name.trim_matches(|c: char| matches!(c, '`' | ';' | '(' | ')' | ',')).chars().take(24).collect()
}

fn took(duration: Duration) -> String {
    if duration.as_secs() >= 1 { format!("{:.2} s", duration.as_secs_f32()) } else { format!("{} ms", duration.as_millis()) }
}

/// The handle between the text and the results, dragged.
#[derive(Clone)]
pub(super) struct SqlSplitDrag;

impl Render for SqlSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub(super) fn text_share() -> f32 {
    crate::state::with_settings(|s| s.split_shares.get("sql").copied()).unwrap_or(TEXT_SHARE).clamp(LEAST_SHARE, 1. - LEAST_SHARE)
}

fn set_text_share(share: f32) {
    let mut settings = crate::state::settings();
    settings.split_shares.insert("sql".to_string(), share.clamp(LEAST_SHARE, 1. - LEAST_SHARE));
    crate::state::set_settings(settings);
}

impl FileView {
    pub(super) fn sql_panel_open(&self) -> bool {
        self.sql.as_ref().is_some_and(|s| s.panel)
    }

    pub(super) fn sql_busy(&self) -> bool {
        self.sql.as_ref().is_some_and(SqlFile::busy)
    }

    pub(super) fn sql_holds(&self, cx: &App) -> bool {
        self.sql.as_ref().is_some_and(|s| s.holds(cx))
    }

    pub(super) fn end_sql(&mut self, cx: &mut Context<Self>) {
        if let Some(sql) = &mut self.sql {
            sql.end();
            cx.notify();
        }
    }

    pub(super) fn drag_sql_split(&mut self, event: &DragMoveEvent<SqlSplitDrag>, cx: &mut Context<Self>) {
        let bounds = event.bounds;
        if bounds.size.height <= px(0.) {
            return;
        }
        set_text_share((event.event.position.y - bounds.top()) / bounds.size.height);
        cx.notify();
    }

    pub(super) fn run_statement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(connection) = sql.connection.clone().filter(|id| config::find(id).is_some()) else {
            return self.ask_which_connection(window, cx);
        };
        let driver = config::find(&connection).map_or(Driver::MySql, |c| c.driver);
        let editor = self.editor.read(cx);
        let (rope, selection, cursor) = (editor.text().clone(), editor.selected_range(), editor.cursor());
        let picking = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let statements = cx.background_spawn(async move { pick(driver, &rope.to_string(), selection, cursor) }).await;
            let _ = this.update_in(cx, |this, window, cx| this.with_parameter_values(connection, statements, window, cx));
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = picking;
        }
    }

    /// A run with no connection chosen: asks which, and goes on with it.
    fn ask_which_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connections = config::list();
        if connections.is_empty() {
            return self.ask_for_connection(window, cx);
        }
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (list, this) = (connections.clone(), this.clone());
            dialog.title("Run against which connection?").w(px(520.)).content(move |content, _, _| {
                content.child(v_flex().id("sql-which").max_h(px(420.)).overflow_y_scroll().gap_1().children(list.iter().enumerate().map(|(ix, c)| {
                    let (this, id) = (this.clone(), c.id.clone());
                    let about = format!("{} · {}@{}:{}{}", c.driver.name(), c.user, c.host, c.port, c.ssh.as_ref().map(|h| format!(" · through {}", h.host)).unwrap_or_default());
                    let mark = c.color.and_then(|i| config::COLORS.get(i as usize)).map(|(_, hue)| hsla(*hue, 0.75, 0.5, 1.));
                    h_flex()
                        .id(("sql-which-row", ix))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|row| row.bg(muted.opacity(0.15)))
                        .child(div().size(px(8.)).flex_none().rounded_full().when_some(mark, |dot, color| dot.bg(color)))
                        .child(v_flex().flex_1().min_w_0().child(div().text_sm().child(c.name.clone())).child(div().text_xs().text_color(muted).truncate().child(about)))
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = this.update(cx, |f, cx| {
                                let database = config::find(&id).and_then(|c| c.database);
                                config::remember(&f.path, &id, database.as_deref());
                                if let Some(sql) = &mut f.sql {
                                    sql.connection = Some(id.clone());
                                    sql.database = database;
                                }
                                f.run_statement(window, cx);
                            });
                        })
                })))
            })
        });
    }

    fn ask_for_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        super::sql_dialog::connection_dialog(
            None,
            Some(Rc::new(move |id: String, window: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |f, cx| f.choose_connection(id, window, cx));
            })),
            window,
            cx,
        );
    }

    /// An `UPDATE` or `DELETE` with no `WHERE` is asked about first. Enter does
    /// not answer: it takes the pointer on the button.
    fn with_parameter_values(&mut self, connection: String, statements: Vec<(Range<usize>, String)>, window: &mut Window, cx: &mut Context<Self>) {
        let driver = config::find(&connection).map_or(Driver::MySql, |c| c.driver);
        let every: Vec<String> = if driver == Driver::Redis { Vec::new() } else { statements.iter().filter(|(_, s)| statement::changes_every_row(s)).map(|(_, s)| one_line(s)).collect() };
        if every.is_empty() {
            return self.fill_parameters(connection, statements, window, cx);
        }
        let target = config::find(&connection).map_or_else(String::new, |c| c.name);
        let this = cx.weak_entity();
        let held = Rc::new(RefCell::new(Some((connection, statements))));
        window.open_dialog(cx, move |dialog, _, cx| {
            let (mono, muted) = (cx.theme().mono_font_family.clone(), cx.theme().muted_foreground);
            let (every, target, this, held) = (every.clone(), target.clone(), this.clone(), held.clone());
            dialog
                .title("Change every row?")
                .w(px(640.))
                .content(move |content, _, _| {
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().text_color(muted).child(format!("No WHERE: on {target} this changes every row of the table.")))
                            .children(every.iter().map(|s| div().text_sm().font_family(mono.clone()).child(s.clone()))),
                    )
                })
                .footer(h_flex().justify_end().gap_2().child(Button::new("sql-every-cancel").primary().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                    Button::new("sql-every-run").danger().label("Run it").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let Some((connection, statements)) = held.borrow_mut().take() else { return };
                        let _ = this.update(cx, |f, cx| f.fill_parameters(connection, statements, window, cx));
                    }),
                ))
        });
    }

    /// Asks for the values of `:name` parameters, if the statements have any.
    fn fill_parameters(&mut self, connection: String, statements: Vec<(Range<usize>, String)>, window: &mut Window, cx: &mut Context<Self>) {
        let driver = config::find(&connection).map_or(Driver::MySql, |c| c.driver);
        // A Redis line's `:` is its own: key names are written that way.
        if driver == Driver::Redis {
            return self.start_run(connection, statements, window, cx);
        }
        let mut names: Vec<String> = Vec::new();
        for (_, text) in &statements {
            for name in statement::parameters(text) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            return self.start_run(connection, statements, window, cx);
        }
        use gpui_kit::component::input::{Input, InputState};
        let last = self.sql.as_ref().map(|s| s.parameters.clone()).unwrap_or_default();
        let fields: Vec<(String, Entity<InputState>)> = names
            .into_iter()
            .map(|name| {
                let value = last.get(&name).cloned().unwrap_or_default();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("A value, a number or NULL"));
                input.update(cx, |i, cx| i.set_value(value, window, cx));
                (name, input)
            })
            .collect();
        let first = fields.first().map(|(_, input)| input.clone());
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let go = {
                let (fields, this, connection, statements) = (fields.clone(), this.clone(), connection.clone(), statements.clone());
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    let values: std::collections::HashMap<String, String> = fields.iter().map(|(n, i)| (n.clone(), i.read(cx).value().to_string())).collect();
                    let filled: Vec<_> = statements.iter().map(|(r, text)| (r.clone(), statement::with_parameters(text, driver, |n| values.get(n).cloned()))).collect();
                    window.close_dialog(cx);
                    let connection = connection.clone();
                    let _ = this.update(cx, |f, cx| {
                        if let Some(sql) = &mut f.sql {
                            sql.parameters.extend(values);
                        }
                        f.start_run(connection, filled, window, cx);
                    });
                })
            };
            let rows = fields.clone();
            dialog
                .title("Parameters")
                .w(px(480.))
                .on_ok({
                    let go = go.clone();
                    move |_, window, cx| {
                        go(window, cx);
                        false
                    }
                })
                .content(move |content, _, _| {
                    content.child(v_flex().gap_2().children(rows.iter().map(|(name, input)| {
                        h_flex().gap_2().items_center().child(div().w(px(120.)).flex_none().text_sm().child(format!(":{name}"))).child(div().flex_1().child(Input::new(input)))
                    })))
                })
                .footer(h_flex().justify_end().gap_2().child(Button::new("sql-params-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                    Button::new("sql-params-run").primary().label("Run").on_click({
                        let go = go.clone();
                        move |_, window, cx| go(window, cx)
                    }),
                ))
        });
        // After the dialog took the keyboard: what is typed next is the value.
        if let Some(input) = first {
            input.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            });
        }
    }

    /// Every statement of the file, once it has been said how many.
    pub(super) fn run_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(connection) = sql.connection.clone().filter(|id| config::find(id).is_some()) else {
            return self.ask_for_connection(window, cx);
        };
        let target = config::find(&connection).map_or_else(String::new, |c| c.name);
        let driver = config::find(&connection).map_or(Driver::MySql, |c| c.driver);
        let rope = self.editor.read(cx).text().clone();
        let picking = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let statements = cx
                .background_spawn(async move {
                    let text = rope.to_string();
                    statement::split_for(driver, &text).into_iter().map(|r| (r.clone(), text[r].to_string())).collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |_, window, cx| {
                let count = statements.len();
                let weak = cx.weak_entity();
                let (held, connection) = (Rc::new(RefCell::new(Some(statements))), connection.clone());
                window.open_dialog(cx, move |dialog, _, _| {
                    let (held, weak, connection) = (held.clone(), weak.clone(), connection.clone());
                    dialog
                        .title("Run the whole file?")
                        .w(px(460.))
                        .content({
                            let text = format!("{} on {target}, one after another. The run stops at the first error.", if count == 1 { "1 statement".to_string() } else { format!("{count} statements") });
                            move |content, _, _| content.child(div().text_sm().child(text.clone()))
                        })
                        .footer(h_flex().justify_end().gap_2().child(Button::new("sql-file-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                            Button::new("sql-file-run").primary().label("Run all").on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let Some(statements) = held.borrow_mut().take() else { return };
                                let connection = connection.clone();
                                let _ = weak.update(cx, |f, cx| f.with_parameter_values(connection, statements, window, cx));
                            }),
                        ))
                });
            });
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = picking;
        }
    }

    /// Lays out the selection's statements, else the whole file's. Statements
    /// it cannot lay out without changing them stay as they are.
    pub(super) fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_redis = self.sql.as_ref().and_then(|s| s.connection.as_deref()).and_then(config::find).is_some_and(|c| c.driver == Driver::Redis);
        if is_redis || self.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("redis")) {
            return;
        }
        let editor = self.editor.read(cx);
        let (rope, selection) = (editor.text().clone(), editor.selected_range());
        let version = self.edit_version;
        let picking = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let done = cx
                .background_spawn(async move {
                    let text = rope.to_string();
                    let within = if selection.is_empty() { 0..text.len() } else { selection };
                    crate::db::format::replace(&text, within)
                })
                .await;
            let Some((range, laid_out)) = done else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                // Typed in meanwhile: the places no longer hold.
                if this.edit_version != version {
                    return;
                }
                this.editor.update(cx, |state, cx| {
                    state.set_selected_range(range, cx);
                    state.replace(laid_out, window, cx);
                });
            });
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = picking;
        }
    }

    /// `EXPLAIN` of the statement at the cursor; the file is not changed.
    pub(super) fn explain_statement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(connection) = sql.connection.clone().filter(|id| config::find(id).is_some()) else {
            return self.ask_for_connection(window, cx);
        };
        let driver = config::find(&connection).map_or(Driver::MySql, |c| c.driver);
        let editor = self.editor.read(cx);
        let (rope, selection, cursor) = (editor.text().clone(), editor.selected_range(), editor.cursor());
        let picking = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let statements = cx.background_spawn(async move { pick(driver, &rope.to_string(), selection, cursor) }).await;
            let explained: Vec<_> = statements.into_iter().take(1).map(|(r, text)| (r, format!("EXPLAIN {text}"))).collect();
            let _ = this.update_in(cx, |this, window, cx| this.with_parameter_values(connection, explained, window, cx));
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = picking;
        }
    }

    /// A new file for statements that belong to no project, kept with the app's data.
    /// A new file of no project, on this file's connection.
    fn new_scratch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (task, connection, database) = (self.task.clone(), self.sql.as_ref().and_then(|s| s.connection.clone()), self.sql.as_ref().and_then(|s| s.database.clone()));
        let making = cx.background_spawn(async move { crate::db::scratch::create() });
        let opening = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            match making.await {
                Ok(path) => {
                    if let Some(connection) = &connection {
                        config::remember(&path, connection, database.as_deref());
                    }
                    // Not from inside this file's update: opening reads the file in view.
                    let _ = task.update_in(cx, |t, window, cx| t.open_file(path, 0, window, cx));
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.scratch_failed(format!("No scratch file: {e}"), cx));
                }
            }
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = opening;
        }
    }

    fn scratch_failed(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(sql) = &mut self.sql {
            sql.say(LogKind::Failed, text, None);
            sql.panel = true;
            sql.shown = Tab::Output;
        }
        cx.notify();
    }

    fn rename_scratch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Input, InputState};
        self.save_now(cx);
        let from = self.path.clone();
        let stem = from.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |i, cx| i.set_value(stem, window, cx));
        let (task, focused, said): (_, _, Rc<RefCell<Option<String>>>) = (self.task.clone(), input.clone(), Rc::default());
        window.open_dialog(cx, move |dialog, _, _| {
            let go = {
                let (input, from, task, said) = (input.clone(), from.clone(), task.clone(), said.clone());
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    let typed = input.read(cx).value().to_string();
                    let (job, task, said, from) = (from.clone(), task.clone(), said.clone(), from.clone());
                    let renaming = cx.background_spawn(async move { crate::db::scratch::rename(&job, &typed) });
                    window
                        .spawn(cx, async move |cx| {
                            let outcome = renaming.await;
                            let _ = cx.update(|window, cx| match outcome {
                                Ok(to) => {
                                    window.close_dialog(cx);
                                    config::file_moved(&from, &to);
                                    let _ = task.update(cx, |t, cx| t.renamed(from.clone(), to, window, cx));
                                }
                                Err(why) => {
                                    *said.borrow_mut() = Some(why);
                                    window.refresh();
                                }
                            });
                        })
                        .detach();
                })
            };
            let (field, said) = (input.clone(), said.clone());
            dialog
                .title("Rename scratch file")
                .w(px(440.))
                .on_ok({
                    let go = go.clone();
                    move |_, window, cx| {
                        go(window, cx);
                        false
                    }
                })
                .content(move |content, _, _| {
                    content.child(v_flex().gap_2().child(Input::new(&field)).when_some(said.borrow().clone(), |this, why| this.child(div().text_xs().text_color(gpui_kit::red()).child(why))))
                })
                .footer(h_flex().justify_end().gap_2().child(Button::new("sql-scratch-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                    Button::new("sql-scratch-rename").primary().label("Rename").on_click({
                        let go = go.clone();
                        move |_, window, cx| go(window, cx)
                    }),
                ))
        });
        focused.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    /// Into the project, under a name chosen there; the tab goes with it.
    fn scratch_to_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_now(cx);
        let from = self.path.clone();
        let suggested = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&self.root, Some(&suggested));
        let task = self.task.clone();
        let moving = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let Ok(Ok(Some(to))) = picked.await else { return };
            let (job_from, job_to) = (from.clone(), to.clone());
            let outcome = cx.background_spawn(async move { crate::db::scratch::move_out(&job_from, &job_to) }).await;
            match outcome {
                Ok(()) => {
                    config::file_moved(&from, &to);
                    let _ = task.update_in(cx, |t, window, cx| t.renamed(from, to, window, cx));
                }
                Err(why) => {
                    let _ = this.update(cx, |this, cx| this.scratch_failed(format!("Not moved: {why}"), cx));
                }
            }
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = moving;
        }
    }

    fn delete_scratch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let holds = self.sql_holds(cx);
        let task = self.task.clone();
        let description = if holds { "Its uncommitted transaction or cell changes not saved are lost too." } else { "It goes to the system's trash, where there is one." };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (path, task) = (path.clone(), task.clone());
            dialog.confirm().title(format!("Delete {name}?")).description(description).ok_text("Delete").on_ok(move |_, window, cx| {
                let (job, task) = (path.clone(), task.clone());
                let _ = task.update(cx, |t, cx| {
                    if let Some(ix) = t.tabs.iter().position(|tab| matches!(&tab.kind, TabKind::File(f) if f.read(cx).path == job)) {
                        t.remove_tab(ix, window, cx);
                    }
                });
                let gone = path.clone();
                cx.background_spawn(async move {
                    if let Err(e) = crate::db::scratch::delete(&gone) {
                        eprintln!("scratch: {e}");
                    }
                })
                .detach();
                true
            })
        });
    }

    /// New, the latest ones to open, and what can be done with this one.
    fn scratch_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (this, task, path) = (cx.weak_entity(), self.task.clone(), self.path.clone());
        let here = crate::db::scratch::is_scratch(&self.path);
        Button::new("sql-console").xsmall().ghost().label("Scratch ▾").tooltip("SQL files of no project, kept by the app").dropdown_menu(move |mut menu, _, _| {
            let new = this.clone();
            menu = menu.item(PopupMenuItem::new("New scratch file").on_click(move |_, window, cx| {
                let _ = new.update(cx, |f, cx| f.new_scratch(window, cx));
            }));
            let files = crate::db::scratch::list();
            if !files.is_empty() {
                menu = menu.separator().label("Open");
            }
            let now = crate::state::now();
            for file in files.into_iter().filter(|f| f.size > 0 || f.path == path).take(SCRATCH_LISTED) {
                let connection = config::recall(&file.path).and_then(|c| config::find(&c.connection)).map_or_else(|| "no connection".to_string(), |c| c.name);
                let label = format!("{} · {connection} · {}", file.name.trim_end_matches(".sql"), crate::local_time::day(file.used, now));
                let (task, target, current) = (task.clone(), file.path.clone(), file.path == path);
                menu = menu.item(PopupMenuItem::new(label).checked(current).on_click(move |_, window, cx| {
                    let target = target.clone();
                    let _ = task.update(cx, |t, cx| t.open_file(target, 0, window, cx));
                }));
            }
            if here {
                let (rename, keep, delete) = (this.clone(), this.clone(), this.clone());
                menu = menu
                    .separator()
                    .item(PopupMenuItem::new("Rename…").on_click(move |_, window, cx| {
                        let _ = rename.update(cx, |f, cx| f.rename_scratch(window, cx));
                    }))
                    .item(PopupMenuItem::new("Save to Project…").on_click(move |_, window, cx| {
                        let _ = keep.update(cx, |f, cx| f.scratch_to_project(window, cx));
                    }))
                    .item(PopupMenuItem::new("Delete…").on_click(move |_, window, cx| {
                        let _ = delete.update(cx, |f, cx| f.delete_scratch(window, cx));
                    }));
            }
            menu
        })
    }

    fn show_history(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        sql.shown = Tab::History;
        let reading = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            let entries = cx.background_spawn(async { crate::db::history::list() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(sql) = &mut this.sql {
                    sql.history = Rc::new(entries);
                }
                cx.notify();
            });
        });
        sql._export = reading;
        cx.notify();
    }

    fn insert_at_cursor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
            state.replace(text, window, cx);
        });
    }

    fn render_sql_history(&self, sql: &SqlFile, cx: &mut Context<Self>) -> AnyElement {
        let entries = sql.history.clone();
        let this = cx.weak_entity();
        let count = entries.len();
        if count == 0 {
            return div().p_3().text_xs().text_color(cx.theme().muted_foreground).child("Nothing has run yet").into_any_element();
        }
        let connection = sql.connection.clone();
        let header = h_flex()
            .flex_none()
            .gap_2()
            .px_2()
            .py_1()
            .items_center()
            .text_xs()
            .child(div().flex_1().text_color(cx.theme().muted_foreground).child(format!("{count} statements, the latest 1000 at most. Statements that set a password are not kept.")))
            .child(Button::new("sql-history-clear").xsmall().ghost().label("Clear ▾").dropdown_menu({
                let this = this.clone();
                move |mut menu, _, _| {
                    if let Some(id) = connection.clone() {
                        let name = config::find(&id).map_or_else(|| "this connection".to_string(), |c| c.name);
                        let (one, id) = (this.clone(), id.clone());
                        menu = menu.item(PopupMenuItem::new(format!("Clear for {name}")).on_click(move |_, _, cx| {
                            crate::db::history::clear(Some(id.clone()));
                            let _ = one.update(cx, |f, cx| f.history_changed(Some(id.clone()), cx));
                        }));
                    }
                    let all = this.clone();
                    menu.item(PopupMenuItem::new("Clear all").on_click(move |_, _, cx| {
                        crate::db::history::clear(None);
                        let _ = all.update(cx, |f, cx| f.history_changed(None, cx));
                    }))
                }
            }));
        let list = uniform_list("sql-history-list", count, move |range, _, cx| {
            let muted = cx.theme().muted_foreground;
            let now = crate::state::now();
            range
                .map(|ix| {
                    let entry = &entries[ix];
                    let name = config::find(&entry.connection).map_or_else(|| "removed".to_string(), |c| c.name);
                    let (put, run, forget) = (this.clone(), this.clone(), this.clone());
                    let (to_put, to_run, to_forget) = (entry.sql.clone(), entry.sql.clone(), entry.clone());
                    h_flex()
                        .id(("sql-history", ix))
                        .h(px(24.))
                        .px_2()
                        .gap_3()
                        .items_center()
                        .text_xs()
                        .child(div().w(px(150.)).flex_none().text_color(muted).child(crate::local_time::day_and_hour(entry.at, now)))
                        .child(div().w(px(120.)).flex_none().truncate().text_color(muted).child(name))
                        .child(div().flex_1().min_w_0().truncate().font_family(cx.theme().mono_font_family.clone()).child(one_line(&entry.sql)))
                        .child(Button::new(("sql-history-put", ix)).xsmall().ghost().label("Insert").on_click(move |_, window, cx| {
                            let _ = put.update(cx, |f, cx| f.insert_at_cursor(&to_put, window, cx));
                        }))
                        .child(Button::new(("sql-history-run", ix)).xsmall().ghost().label("Run").on_click(move |_, window, cx| {
                            let _ = run.update(cx, |f, cx| f.run_text(to_run.clone(), window, cx));
                        }))
                        .child(Button::new(("sql-history-forget", ix)).xsmall().ghost().label("×").tooltip("Forget this statement").on_click(move |_, _, cx| {
                            crate::db::history::remove(to_forget.clone());
                            let gone = to_forget.clone();
                            let _ = forget.update(cx, |f, cx| {
                                if let Some(sql) = &mut f.sql {
                                    sql.history = Rc::new(sql.history.iter().filter(|e| **e != gone).cloned().collect());
                                }
                                cx.notify();
                            });
                        }))
                })
                .collect::<Vec<_>>()
        })
        .flex_1()
        .min_h_0();
        v_flex().size_full().child(header).child(list).into_any_element()
    }

    fn history_changed(&mut self, connection: Option<String>, cx: &mut Context<Self>) {
        if let Some(sql) = &mut self.sql {
            sql.history = Rc::new(match connection {
                Some(id) => sql.history.iter().filter(|e| e.connection != id).cloned().collect(),
                None => Vec::new(),
            });
        }
        cx.notify();
    }

    /// The result shown again, through its filter or from another row.
    fn read_window(&mut self, ix: usize, filter: Option<String>, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(result) = sql.results.get(ix) else { return };
        if let Some(from) = sql.elsewhere(result) {
            sql.say(LogKind::Failed, format!("This result is from {from}: it is not read again on the connection the file has now"), None);
            sql.shown = Tab::Output;
            return cx.notify();
        }
        let mut origin = result.origin.clone().unwrap_or(Origin { base: result.sql.clone(), ..Default::default() });
        if let Some(filter) = filter {
            origin.filter = filter;
        }
        origin.offset = offset;
        if origin.pages_may_shift() {
            sql.say(LogKind::Info, "Nothing orders these rows: a page can repeat or miss some. Sort by a column to be sure", None);
        }
        let text = origin.statement();
        sql.next_origin = Some(origin);
        self.run_text(text, window, cx);
    }

    /// A sort of a result cut at the limit: asked of the server, from its first row.
    fn sort_on_server(&mut self, table: &WeakEntity<TableState<SqlRows>>, column: usize, sort: ColumnSort, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(result) = sql.results.iter().find(|r| r.table.downgrade() == *table) else { return };
        if let Some(from) = sql.elsewhere(result) {
            sql.say(LogKind::Failed, format!("This result is from {from}: only the file's current connection can sort on the server"), None);
            sql.shown = Tab::Output;
            return cx.notify();
        }
        let Some(mut origin) = result.origin.clone() else { return };
        origin.sort = match sort {
            ColumnSort::Ascending => Some((column, false)),
            ColumnSort::Descending => Some((column, true)),
            ColumnSort::Default => None,
        };
        origin.offset = 0;
        let text = origin.statement();
        sql.next_origin = Some(origin);
        self.run_text(text, window, cx);
    }

    /// This file's connection is production and locked: asks, and runs
    /// `again` once it is unlocked. False when there is nothing to ask.
    fn production_asks(&self, again: impl Fn(&mut FileView, &mut Window, &mut Context<FileView>) + 'static, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let locked = self.sql.as_ref().and_then(|s| s.connection.as_deref()).and_then(config::find).is_some_and(|c| crate::db::guard::may_run(&c).is_err());
        if !locked {
            return false;
        }
        let (this, again) = (cx.weak_entity(), Rc::new(again));
        let then: super::sql_guard::Then = Rc::new(move |window, cx| {
            let again = again.clone();
            let _ = this.update(cx, |f, cx| again(f, window, cx));
        });
        super::sql_guard::production_gate(then, window, cx);
        true
    }

    fn start_run(&mut self, connection: String, statements: Vec<(Range<usize>, String)>, window: &mut Window, cx: &mut Context<Self>) {
        // Production runs nothing until unlocked: asked here, and refused by the session too.
        // What this run was for (a page, a sort, a followed key) waits with it, and is
        // gone with a Cancel, not left for the next statement.
        if config::find(&connection).is_some_and(|c| crate::db::guard::may_run(&c).is_err()) {
            let held = self.sql.as_mut().map(|sql| (sql.next_origin.take(), std::mem::take(&mut sql.keep_results)));
            let held = Rc::new(RefCell::new(held));
            let this = cx.weak_entity();
            let then: super::sql_guard::Then = Rc::new(move |window, cx| {
                let (connection, statements) = (connection.clone(), statements.clone());
                let held = held.borrow_mut().take();
                let _ = this.update(cx, |f, cx| {
                    if let (Some(sql), Some((origin, keep))) = (&mut f.sql, held) {
                        sql.next_origin = origin;
                        sql.keep_results = keep;
                    }
                    f.start_run(connection, statements, window, cx)
                });
            });
            return super::sql_guard::production_gate(then, window, cx);
        }
        let color = cx.theme().primary.opacity(0.12);
        let marks: Vec<RangeDecoration> = statements.iter().map(|(r, _)| RangeDecoration::new(r.clone()).with_style(RangeDecorationStyle::Fill).with_color(color)).collect();
        let editor = self.editor.clone();
        let Some(sql) = &mut self.sql else { return };
        sql.panel = true;
        if statements.is_empty() {
            sql.say(LogKind::Failed, "No statement here to run", None);
            sql.shown = Tab::Output;
            cx.notify();
            return;
        }
        // Cell changes not saved yet are not thrown away by the next statement,
        // nor is the result a foreign key was followed from.
        if !std::mem::take(&mut sql.keep_results) {
            sql.results.retain(|r| r.pinned || r.table.read(cx).delegate().has_changes());
        }
        sql.shown = Tab::Output;
        if let Some(mark) = &sql.error_mark {
            mark.clear(cx);
        }
        sql.running = statements.clone();
        if let Some(id) = &sql.connection {
            // A page or a filter is the same statement looked at: not one more in the history.
            if sql.next_origin.is_none() {
                crate::db::history::add(id.clone(), sql.database.clone(), statements.iter().map(|(_, s)| s.clone()).collect());
            }
        }
        match &sql.mark {
            Some(mark) => mark.set(marks, cx),
            None => sql.mark = Some(editor.update(cx, |state, cx| state.create_range_decorations_collection(marks, cx))),
        }
        match &sql.session {
            Some(session) if sql.status == Status::Ready => {
                sql.status = Status::Running;
                session.run(statements);
            }
            _ => {
                sql.pending = Some(statements);
                self.connect(connection, window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn stop_statement(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        match sql.status {
            Status::Running => {
                if let Some(session) = &sql.session {
                    session.stop();
                }
            }
            Status::Connecting => {
                sql.end();
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn choose_connection(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let holds = self.sql_holds(cx);
        if let Some(sql) = &mut self.sql
            && holds
        {
            sql.say(LogKind::Failed, "Commit or roll back, and apply or discard the cell changes first: another connection would lose them", None);
            sql.panel = true;
            sql.shown = Tab::Output;
            return cx.notify();
        }
        let database = config::find(&id).and_then(|c| c.database);
        config::remember(&self.path, &id, database.as_deref());
        let Some(sql) = &mut self.sql else { return };
        sql.end();
        sql.connection = Some(id.clone());
        sql.database = database;
        sql.databases.clear();
        sql.version = None;
        *sql.schema.borrow_mut() = None;
        self.refresh_schema_rows(cx);
        self.connect(id, window, cx);
        cx.notify();
    }

    fn choose_database(&mut self, name: String, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let holds = self.sql_holds(cx);
        let Some(sql) = &mut self.sql else { return };
        if sql.status == Status::Running {
            return;
        }
        if holds {
            sql.say(LogKind::Failed, "Commit or roll back, and save or discard the cell changes first: another database would lose them", None);
            sql.panel = true;
            sql.shown = Tab::Output;
            return cx.notify();
        }
        if let Some(connection) = &sql.connection {
            config::remember(&path, connection, Some(&name));
        }
        *sql.schema.borrow_mut() = None;
        if let Some(session) = &sql.session {
            session.use_database(name.clone());
            session.load_schema(name.clone());
        }
        sql.database = Some(name);
        self.refresh_schema_rows(cx);
        cx.notify();
    }

    /// Typing in a file that has a database and has not connected yet: its
    /// schema is read for the completions, as kept or from the server. A
    /// locked production connection gives only what was kept.
    pub(super) fn want_schema(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        if sql.session.is_some() || sql.schema.borrow().is_some() {
            return;
        }
        let (Some(id), Some(database)) = (sql.connection.clone(), sql.database.clone()) else { return };
        let asked = (id, database);
        // Per keystroke: no more than comparing names, until something is to be read.
        if let Some((id, database, was_locked)) = &sql.schema_asked
            && (id, database) == (&asked.0, &asked.1)
            && !(*was_locked && crate::db::guard::is_open())
        {
            return;
        }
        let Some(connection) = crate::db::config::find(&asked.0) else { return };
        let locked = crate::db::guard::may_run(&connection).is_err();
        sql.schema_asked = Some((asked.0.clone(), asked.1.clone(), locked));
        sql._schema = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            // What was kept first: the keyring is asked only when there is nothing.
            let (id, database) = (connection.id.clone(), asked.1.clone());
            let mut read = cx.background_spawn(async move { crate::db::schema_cache::read(&id, &database).map(|(_, schema)| schema) }).await;
            if read.is_none() && crate::db::guard::may_run(&connection).is_ok() {
                let Ok(password) = this.update(cx, |_, cx| crate::db::password::read_for(&connection, cx)) else { return };
                if let Ok(password) = password.await {
                    let database = asked.1.clone();
                    read = cx.background_spawn(crate::procs::on_thread(move || crate::db::browse::schema(&connection, password, &database))).await.ok();
                }
            }
            let Some(schema) = read else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(sql) = &this.sql else { return };
                let same = sql.connection.as_ref() == Some(&asked.0) && sql.database.as_ref() == Some(&asked.1);
                if same && sql.schema.borrow().is_none() {
                    *sql.schema.borrow_mut() = Some(Arc::new(schema));
                    this.refresh_schema_rows(cx);
                }
            });
        });
    }

    fn connect(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = config::find(&id) else { return };
        // Not now: the first statement asks, and connects once unlocked.
        if crate::db::guard::may_run(&connection).is_err() {
            if let Some(sql) = &mut self.sql {
                sql.session = None;
                sql.status = Status::Off;
                sql.say(LogKind::Info, "Production is locked: it connects when a statement runs and you unlock it", None);
            }
            return cx.notify();
        }
        let password = crate::db::password::read_for(&connection, cx);
        let Some(sql) = &mut self.sql else { return };
        sql.session = None;
        sql.status = Status::Connecting;
        let database = sql.database.clone();
        sql._events = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let password = match password.await {
                Ok(password) => password,
                Err(e) => {
                    let said = if e.to_string() == crate::db::guard::LOCKED { e.to_string() } else { format!("The keyring: {e}") };
                    let _ = this.update_in(cx, |this, window, cx| this.sql_event(Event::Lost(said), window, cx));
                    return;
                }
            };
            let (session, events) = Session::start(connection, password, database);
            if this.update(cx, |this, _| {
                if let Some(sql) = &mut this.sql {
                    sql.session = Some(session);
                }
            }).is_err() {
                return;
            }
            while let Ok(event) = events.recv().await {
                if this.update_in(cx, |this, window, cx| this.sql_event(event, window, cx)).is_err() {
                    break;
                }
            }
        });
    }

    fn sql_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        match event {
            Event::Connected { version, encrypted } => {
                sql.encrypted = encrypted;
                let product = sql.connection.as_deref().and_then(config::find).map_or("Server", |c| c.driver.name());
                let tunnel = sql.connection.as_deref().and_then(config::find).is_some_and(|c| c.ssh.is_some());
                let how = match (tunnel, encrypted) {
                    (true, true) => "through the bastion, encrypted to the server",
                    (true, false) => "through the bastion, not encrypted past it",
                    (false, true) => "encrypted",
                    (false, false) => "NOT encrypted",
                };
                sql.say(if tunnel || encrypted { LogKind::Info } else { LogKind::Failed }, format!("Connected: {product} {version}, {how}"), None);
                sql.version = Some(version.into());
                sql.status = Status::Ready;
                if let Some(session) = &sql.session {
                    session.list_databases();
                    if let Some(database) = sql.database.clone() {
                        session.load_schema(database);
                    }
                    if sql.manual {
                        session.set_manual(true);
                    }
                    if let Some(statements) = sql.pending.take() {
                        sql.status = Status::Running;
                        session.run(statements);
                    }
                }
            }
            Event::Lost(message) => {
                sql.next_origin = None;
                sql.applying = None;
                if sql.uncommitted {
                    sql.say(LogKind::Failed, "The open transaction is gone with the connection: its changes were not kept", None);
                }
                let refused = login_refused(&message);
                sql.say(LogKind::Failed, message, None);
                sql.session = None;
                sql.pending = None;
                sql.running.clear();
                sql.uncommitted = false;
                sql.status = Status::Off;
                sql.version = None;
                sql.panel = true;
                sql.shown = Tab::Output;
                // A connection imported, or a password changed: asked where it can be typed.
                if refused && let Some(id) = sql.connection.clone() {
                    sql.say(LogKind::Info, "Check the login and the password of this connection", None);
                    let this = cx.weak_entity();
                    let again = id.clone();
                    super::sql_dialog::connection_dialog(
                        Some(id),
                        Some(Rc::new(move |_: String, window: &mut Window, cx: &mut App| {
                            let _ = this.update(cx, |f, cx| f.choose_connection(again.clone(), window, cx));
                        })),
                        window,
                        cx,
                    );
                }
            }
            Event::Databases(names) => sql.databases = names,
            Event::Result { statement, columns } => {
                let ran = sql.running.iter().find(|(r, _)| *r == statement).map(|(_, s)| s.clone()).unwrap_or_default();
                let schema = sql.schema.borrow().clone();
                let mut origin = sql.next_origin.take().or_else(|| only_reads(&ran).then(|| Origin { base: ran.clone(), ..Default::default() }));
                if let Some(origin) = &mut origin {
                    // Of the one table's key in this result, if it has one to go by.
                    origin.tie_break = schema
                        .as_ref()
                        .and_then(|schema| (0..columns.len()).find_map(|c| edit_target(schema, &columns, c)))
                        .map(|target| target.key.into_iter().map(|(_, at)| at).collect())
                        .unwrap_or_default();
                }
                let current = origin.as_ref().and_then(|o| o.sort).map(|(c, down)| (c, if down { ColumnSort::Descending } else { ColumnSort::Ascending }));
                let table = new_table(columns, window, cx);
                if origin.is_some() {
                    let (file, weak) = (cx.weak_entity(), table.downgrade());
                    table.update(cx, |t, _| {
                        t.delegate_mut().set_server_sort(
                            Rc::new(move |column, sort, window, cx| {
                                let _ = file.update(cx, |f, cx| f.sort_on_server(&weak, column, sort, window, cx));
                            }),
                            current,
                        );
                    });
                }
                // Named after what it looks at: a filter's wrapper would name it SELECT.
                let title = title_of(origin.as_ref().map_or(&ran, |o| &o.base));
                let from = (sql.connection.clone(), sql.database.clone());
                sql.results.push(SqlResult { title: title.into(), origin, statement, sql: ran, table, open: true, cut: false, said: None, pinned: false, row_view: false, from });
                sql.shown = Tab::Result(sql.results.len() - 1);
            }
            Event::Rows(batch) => {
                if let Some(table) = sql.results.last().filter(|r| r.open).map(|r| r.table.clone()) {
                    table.update(cx, |table, cx| {
                        // Sized once, by the first rows: a refresh takes back widths the user dragged.
                        if table.delegate_mut().append(batch) {
                            table.refresh(cx);
                        } else {
                            cx.notify();
                        }
                    });
                }
            }
            Event::ResultDone { statement, affected, took: time, cut } => {
                let ran = sql.running.iter().find(|(r, _)| *r == statement).map(|(_, s)| s.clone()).unwrap_or_default();
                sql.say(LogKind::Statement, one_line(&ran), None);
                // What a statement did is known by what it is, a row returned or not.
                if sql.manual && !ran.is_empty() && !only_reads(&ran) {
                    sql.uncommitted = true;
                }
                let open = sql.results.len().checked_sub(1).filter(|&ix| sql.results[ix].open && sql.results[ix].statement == statement);
                match open {
                    Some(ix) => {
                        let result = &mut sql.results[ix];
                        result.open = false;
                        result.cut = cut;
                        result.table.update(cx, |t, _| t.delegate_mut().set_cut(cut));
                        let rows = result.table.read(cx).delegate().len();
                        let said = if cut { format!("the first {rows} rows (at most {MOST_ROWS}) · {}", took(time)) } else { format!("{} · {}", rows_said(rows as u64), took(time)) };
                        result.said = Some(said.clone().into());
                        sql.say(LogKind::Info, said, None);
                    }
                    None => {
                        sql.say(LogKind::Info, format!("{} affected · {}", rows_said(affected), took(time)), None);
                        if changes_schema(&ran)
                            && let (Some(session), Some(database)) = (&sql.session, sql.database.clone())
                        {
                            session.load_schema(database);
                        }
                        if let Some(database) = used_database(&ran) {
                            if let Some(id) = &sql.connection {
                                config::remember(&self.path, id, Some(&database));
                            }
                            *sql.schema.borrow_mut() = None;
                            if let Some(session) = &sql.session {
                                session.load_schema(database.clone());
                            }
                            sql.database = Some(database);
                        }
                    }
                }
            }
            Event::Failed { statement, message, at } => {
                let ran = sql.running.iter().find(|(r, _)| *r == statement).map(|(_, s)| s.clone()).unwrap_or_default();
                if let Some(result) = sql.results.last_mut() {
                    result.open = false;
                }
                if !ran.is_empty() {
                    sql.say(LogKind::Statement, one_line(&ran), None);
                }
                // Or the next statement's result would take this one's filter for its own.
                sql.next_origin = None;
                sql.applying = None;
                // A place in the statement is one in the file only while the text is as it ran.
                let as_ran = !statement.is_empty() && statement.end <= self.editor.read(cx).text().len() && self.editor.read(cx).text().slice(statement.clone()).to_string() == ran;
                let place = at.filter(|_| as_ran).map(|at| {
                    let rest = ran.get(at..).unwrap_or("");
                    let length = rest.find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')')).unwrap_or(rest.len()).max(1);
                    let start = statement.start + at.min(ran.len());
                    start..(start + length).min(statement.end).max(start)
                });
                if let Some(place) = place.clone().filter(|p| !p.is_empty()) {
                    let mark = vec![RangeDecoration::new(place).with_style(RangeDecorationStyle::Fill).with_color(hsla(0.0, 0.85, 0.55, 0.35))];
                    match &sql.error_mark {
                        Some(collection) => collection.set(mark, cx),
                        None => sql.error_mark = Some(self.editor.update(cx, |state, cx| state.create_range_decorations_collection(mark, cx))),
                    }
                }
                sql.say(LogKind::Failed, message, place.or((!statement.is_empty()).then_some(statement)));
                sql.shown = Tab::Output;
            }
            Event::Idle { stopped } => {
                if let Some(ix) = sql.reread.take() {
                    let offset = sql.results.get(ix).and_then(|r| r.origin.as_ref()).map_or(0, |o| o.offset);
                    sql.status = Status::Ready;
                    sql.running.clear();
                    sql.keep_results = false;
                    self.read_window(ix, None, offset, window, cx);
                    return cx.notify();
                }
                sql.status = Status::Ready;
                sql.running.clear();
                sql.next_origin = None;
                if stopped {
                    if let Some(result) = sql.results.last_mut().filter(|r| r.open) {
                        result.open = false;
                        let rows = result.table.read(cx).delegate().len();
                        result.said = Some(format!("stopped · {rows} rows read").into());
                    }
                    sql.say(LogKind::Info, "Stopped", None);
                }
            }
            Event::Note { text, failed } => {
                sql.say(if failed { LogKind::Failed } else { LogKind::Info }, text, None);
                if failed {
                    sql.panel = true;
                    sql.shown = Tab::Output;
                }
            }
            Event::Manual(manual) => sql.manual = manual,
            Event::TransactionEnded => sql.uncommitted = false,
            Event::BatchDone { error } => {
                let applied_ix = sql.applying.take();
                let applied = applied_ix.and_then(|ix| sql.results.get(ix));
                match (error, applied) {
                    (None, Some(result)) => {
                        let count = result.table.read(cx).delegate().changes();
                        let added = result.table.read(cx).delegate().columns().len() > 0 && sql.applying_added;
                        result.table.update(cx, |t, cx| {
                            t.delegate_mut().keep_edits();
                            cx.notify();
                        });
                        if added {
                            sql.reread = applied_ix;
                        }
                        if sql.manual {
                            sql.uncommitted = true;
                        }
                        let tail = if sql.manual { ": waiting for Commit" } else { "" };
                        sql.say(LogKind::Info, format!("Saved {count} changes{tail}"), None);
                    }
                    (Some(error), _) => {
                        sql.say(LogKind::Failed, format!("Nothing was saved: {error}"), None);
                        sql.panel = true;
                        sql.shown = Tab::Output;
                    }
                    _ => {}
                }
            }
            Event::SchemaState(state) => sql.schema_checking = state == SchemaState::Checking,
            Event::Schema(schema) => {
                *sql.schema.borrow_mut() = Some(schema);
                self.refresh_schema_rows(cx);
            }
            Event::Definition { table, text } => {
                sql.say(LogKind::Statement, format!("SHOW CREATE TABLE {table}"), None);
                sql.say(LogKind::Info, text, None);
                sql.panel = true;
                sql.shown = Tab::Output;
            }
        }
        cx.notify();
    }

    /// Reads what the chosen database holds, for the tree and the completions.
    pub(super) fn load_schema(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        match (&sql.session, sql.database.clone()) {
            (Some(session), Some(database)) => session.load_schema(database),
            _ => sql.say(LogKind::Info, "Connect and choose a database to read its schema", None),
        }
        cx.notify();
    }

    pub(super) fn show_table_definition(&mut self, table: String, cx: &mut Context<Self>) {
        if let Some(session) = self.sql.as_ref().and_then(|s| s.session.as_ref()) {
            session.show_create(table);
        }
        cx.notify();
    }

    /// Runs a statement that is not in the file: a table's data, a page of it.
    pub(super) fn run_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        if sql.busy() {
            return;
        }
        let Some(connection) = sql.connection.clone().filter(|id| config::find(id).is_some()) else {
            return self.ask_for_connection(window, cx);
        };
        self.with_parameter_values(connection, vec![(0..0, text)], window, cx);
    }

    fn ask_filter(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Input, InputState};
        let Some(sql) = &mut self.sql else { return };
        let current = sql.results.get(ix).and_then(|r| r.origin.as_ref()).map(|o| o.filter.clone()).unwrap_or_default();
        let input = sql.filter_input.clone().unwrap_or_else(|| {
            let made = cx.new(|cx| InputState::new(window, cx).placeholder("status = 'new' AND amount > 10"));
            made
        });
        sql.filter_input = Some(input.clone());
        input.update(cx, |i, cx| i.set_value(current, window, cx));
        let focused = input.clone();
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let apply = {
                let (input, this) = (input.clone(), this.clone());
                Rc::new(move |clear: bool, window: &mut Window, cx: &mut App| {
                    let filter = if clear { String::new() } else { input.read(cx).value().trim().to_string() };
                    window.close_dialog(cx);
                    let _ = this.update(cx, |f, cx| f.read_window(ix, Some(filter), 0, window, cx));
                })
            };
            let field = input.clone();
            let (on_ok, on_clear, on_apply) = (apply.clone(), apply.clone(), apply.clone());
            dialog
                .title("Filter (WHERE)")
                .w(px(560.))
                .on_ok(move |_, window, cx| {
                    on_ok(false, window, cx);
                    false
                })
                .content(move |content, _, _| content.child(Input::new(&field)))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("sql-filter-clear").ghost().label("Clear").on_click(move |_, window, cx| on_clear(true, window, cx)))
                        .child(Button::new("sql-filter-apply").primary().label("Apply").on_click(move |_, window, cx| on_apply(false, window, cx))),
                )
        });
        focused.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    fn toggle_manual(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        if sql.status == Status::Running {
            return;
        }
        // `SET autocommit = 1` would commit what is open without anyone having said so.
        if sql.manual && sql.uncommitted {
            sql.say(LogKind::Failed, "Commit or roll back first", None);
            sql.panel = true;
            sql.shown = Tab::Output;
            return cx.notify();
        }
        let manual = !sql.manual;
        match &sql.session {
            Some(session) => session.set_manual(manual),
            None => sql.manual = manual,
        }
        cx.notify();
    }

    fn end_transaction(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if commit && self.production_asks(|f, window, cx| f.end_transaction(true, window, cx), window, cx) {
            return;
        }
        let Some(sql) = &mut self.sql else { return };
        if sql.status == Status::Running {
            return;
        }
        // Nothing open any more once the server says so: a refused Commit leaves it open.
        if let Some(session) = &sql.session {
            if commit { session.commit() } else { session.rollback() }
        }
        cx.notify();
    }

    fn copy_as(&mut self, ix: usize, format: Format, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        let Some(result) = sql.results.get(ix) else { return };
        let (columns, rows, order) = result.table.read(cx).delegate().snapshot();
        let (table, name) = (result.title.to_string(), format.name());
        let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
        sql._export = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            let text = cx.background_spawn(async move { export::to_string(format, columns, &table, driver, order.iter().map(|&i| &*rows[i as usize])) }).await;
            let _ = this.update(cx, |this, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                if let Some(sql) = &mut this.sql {
                    sql.say(LogKind::Info, format!("Copied as {name}"), None);
                }
                cx.notify();
            });
        });
    }

    /// Whole: a result cut at the limit is run again and streamed; one that
    /// would change data is never run again, and only what was read is saved.
    fn ask_save(&mut self, ix: usize, format: Format, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        let Some(result) = sql.results.get(ix) else { return };
        if !result.cut {
            return self.save_as(ix, format, false, cx);
        }
        let rows = result.table.read(cx).delegate().len();
        if let Some(from) = sql.elsewhere(result) {
            sql.say(LogKind::Info, format!("Saving the {rows} rows read from {from}: it is not the file's connection now, so nothing is run again"), None);
            return self.save_as(ix, format, false, cx);
        }
        if !only_reads(&result.sql) {
            sql.say(LogKind::Info, format!("Saving the {rows} rows read: this statement is not run again"), None);
            return self.save_as(ix, format, false, cx);
        }
        let name = sql.connection.as_deref().and_then(config::find).map_or_else(String::new, |c| c.name);
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (loaded, again) = (this.clone(), this.clone());
            let about = format!("The table holds the first {rows} rows. All of them means running the statement again on {name}: the data may have changed since.");
            dialog
                .title("Export which rows?")
                .w(px(560.))
                .content(move |content, _, _| content.child(div().text_sm().child(about.clone())))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("sql-export-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("sql-export-loaded").outline().label(format!("The {rows} loaded")).on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = loaded.update(cx, |f, cx| f.save_as(ix, format, false, cx));
                        }))
                        .child(Button::new("sql-export-all").primary().label("Run again, all rows").on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = again.update(cx, |f, cx| f.save_as(ix, format, true, cx));
                        })),
                )
        });
    }

    fn save_as(&mut self, ix: usize, format: Format, run_again: bool, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        let Some(result) = sql.results.get(ix) else { return };
        let suggested = format!("{}.{}", result.title, format.extension());
        let dir = self.path.parent().map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        let task = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let job = this.update(cx, |this, cx| {
                let sql = this.sql.as_mut()?;
                let result = sql.results.get(ix)?;
                if run_again && result.cut && only_reads(&result.sql) && sql.is_from_here(result) && sql.status == Status::Ready {
                    let (statement, table) = (result.sql.clone(), result.title.to_string());
                    let session = sql.session.as_ref()?;
                    session.export(statement, format, path, table);
                    sql.status = Status::Running;
                    cx.notify();
                    return None;
                }
                let (columns, rows, order) = result.table.read(cx).delegate().snapshot();
                let (refused, title) = (run_again, result.title.to_string());
                if refused {
                    sql.say(LogKind::Failed, "Not run again: saving the rows that were read", None);
                }
                let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
                Some((columns, rows, order, title, path, driver))
            });
            let Ok(Some((columns, rows, order, table, path, driver))) = job else { return };
            let shown = path.display().to_string();
            let written = cx
                .background_spawn(async move {
                    let tmp = path.with_extension(format!("{}.part", format.extension()));
                    let write = || -> std::io::Result<u64> {
                        let file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
                        let mut writer = export::Writer::start(file, format, columns, &table, driver)?;
                        for &i in order.iter() {
                            writer.row(&rows[i as usize])?;
                        }
                        let count = writer.finish()?;
                        std::fs::rename(&tmp, &path)?;
                        Ok(count)
                    };
                    let outcome = write();
                    if outcome.is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                    outcome
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(sql) = &mut this.sql {
                    match written {
                        Ok(rows) => sql.say(LogKind::Info, format!("Exported {rows} rows to {shown}"), None),
                        Err(e) => sql.say(LogKind::Failed, format!("Export failed: {e}"), None),
                    }
                }
                cx.notify();
            });
        });
        if let Some(sql) = &mut self.sql {
            sql._export = task;
        }
    }

    fn go_to_statement(&mut self, statement: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(statement, cx);
            state.focus(window, cx);
        });
    }

    /// A production connection's mark: locked, or until when it is not; a click turns it.
    fn production_badge(&self, cx: &mut Context<Self>) -> AnyElement {
        let until = crate::db::guard::open_until();
        let red = gpui_kit::red();
        Button::new("sql-production")
            .xsmall()
            .ghost()
            .label(match until {
                Some(until) => format!("PRODUCTION · unlocked until {}", crate::local_time::hour_minute(until)),
                None => "PRODUCTION · locked".into(),
            })
            .text_color(red)
            .tooltip(if until.is_some() { "Click to lock production now" } else { "Statements here ask before production is unlocked. Click to unlock" })
            .on_click(cx.listener(move |_, _, window, cx| {
                if crate::db::guard::is_open() {
                    super::sql_guard::lock_now(cx);
                } else {
                    super::sql_guard::production_gate(Rc::new(|_, _| {}), window, cx);
                }
            }))
            .into_any_element()
    }

    /// The line over the text: run, stop, the connection, the database.
    pub(super) fn render_sql_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sql = self.sql.as_ref()?;
        let this = cx.weak_entity();
        let current = sql.connection.as_deref().and_then(config::find);
        let scratch = self.scratch_button(cx);
        let is_redis = current.as_ref().is_some_and(|c| c.driver == Driver::Redis);
        let connection_label = current.as_ref().map_or_else(|| "Choose a connection".to_string(), |c| c.name.clone());
        let (chosen, database_label) = (sql.connection.clone(), sql.database.clone().unwrap_or_else(|| "No database".into()));
        let connections = {
            let this = this.clone();
            Button::new("sql-connection").xsmall().ghost().label(format!("{connection_label} ▾")).dropdown_menu(move |mut menu, _, _| {
                for c in config::list() {
                    let (this, id) = (this.clone(), c.id.clone());
                    menu = menu.item(PopupMenuItem::new(c.name).checked(chosen.as_deref() == Some(&c.id)).on_click(move |_, window, cx| {
                        let _ = this.update(cx, |f, cx| f.choose_connection(id.clone(), window, cx));
                    }));
                }
                let new = this.clone();
                menu = menu.separator().item(PopupMenuItem::new("New connection…").on_click(move |_, window, cx| {
                    let _ = new.update(cx, |f, cx| f.ask_for_connection(window, cx));
                }));
                if let Some(id) = chosen.clone() {
                    let edit = this.clone();
                    menu = menu.item(PopupMenuItem::new("Edit this connection…").on_click(move |_, window, cx| {
                        let (edit, again) = (edit.clone(), id.clone());
                        super::sql_dialog::connection_dialog(
                            Some(id.clone()),
                            Some(Rc::new(move |_: String, window: &mut Window, cx: &mut App| {
                                let _ = edit.update(cx, |f, cx| f.choose_connection(again.clone(), window, cx));
                            })),
                            window,
                            cx,
                        );
                    }));
                }
                menu
            })
        };
        let databases = {
            let (names, selected) = (sql.databases.clone(), sql.database.clone());
            let this = this.clone();
            Button::new("sql-database").xsmall().ghost().label(format!("{database_label} ▾")).disabled(names.is_empty()).dropdown_menu(move |mut menu, _, _| {
                for name in &names {
                    let (this, chosen) = (this.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(selected.as_ref() == Some(name)).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |f, cx| f.choose_database(chosen.clone(), cx));
                    }));
                }
                menu
            })
        };
        let state: Option<(SharedString, Hsla)> = match sql.status {
            Status::Connecting => Some(("connecting…".into(), cx.theme().muted_foreground)),
            Status::Running => Some(("running…".into(), cx.theme().muted_foreground)),
            Status::Ready => sql.version.clone().map(|v| {
                let tunnel = current.as_ref().is_some_and(|c| c.ssh.is_some());
                let how = match (tunnel, sql.encrypted) {
                    (true, true) => "SSH + TLS",
                    (true, false) => "SSH",
                    (false, true) => "TLS",
                    (false, false) => "not encrypted",
                };
                let color = if tunnel || sql.encrypted { gpui_kit::green() } else { hsla(0.08, 0.9, 0.45, 1.) };
                (format!("● {} {v} · {how}", current.as_ref().map_or("Server", |c| c.driver.name())).into(), color)
            }),
            Status::Off => None,
        };
        Some(
            h_flex()
                .id("sql-bar")
                .test_support()
                .flex_none()
                .flex_wrap()
                .when_some(current.as_ref().and_then(|c| c.color).and_then(|i| config::COLORS.get(i as usize)), |bar, (_, hue)| bar.bg(hsla(*hue, 0.75, 0.5, 0.16)).border_l_2().border_color(hsla(*hue, 0.75, 0.5, 1.)))
                .gap_1()
                .px_2()
                .py(px(2.))
                .items_center()
                .text_xs()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Button::new("sql-run")
                        .xsmall()
                        .ghost()
                        .label("▶ Run")
                        .disabled(sql.busy())
                        .tooltip(format!("The selection, else the statement at the cursor ({})", crate::keymap::label("sql_run")))
                        .on_click(cx.listener(|this, _, window, cx| this.run_statement(window, cx))),
                )
                .child(
                    Button::new("sql-run-file")
                        .xsmall()
                        .ghost()
                        .label("Run all")
                        .disabled(sql.busy())
                        .tooltip(format!("Every statement of the file, after asking ({})", crate::keymap::label("sql_run_file")))
                        .on_click(cx.listener(|this, _, window, cx| this.run_file(window, cx))),
                )
                .when(!is_redis, |bar| bar.child(
                    Button::new("sql-explain")
                        .xsmall()
                        .ghost()
                        .label("Explain")
                        .disabled(sql.busy())
                        .tooltip("EXPLAIN of the statement at the cursor")
                        .on_click(cx.listener(|this, _, window, cx| this.explain_statement(window, cx))),
                ))
                .child(
                    Button::new("sql-stop")
                        .xsmall()
                        .ghost()
                        .label("■")
                        .disabled(!sql.busy())
                        .tooltip(format!("Stop ({})", crate::keymap::label("sql_stop")))
                        .on_click(cx.listener(|this, _, _, cx| this.stop_statement(cx))),
                )
                .children(current.as_ref().filter(|c| c.production).map(|_| self.production_badge(cx)))
                .child(connections)
                .child(databases)
                .when(!is_redis, |bar| bar.child(
                    Button::new("sql-tx-mode")
                        .xsmall()
                        .ghost()
                        .label(if sql.manual { "Tx: Manual" } else { "Tx: Auto" })
                        .tooltip(if sql.manual { "Statements wait for Commit. Click for auto-commit" } else { "Each statement is kept at once. Click to keep them only on Commit" })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_manual(cx))),
                ))
                .when(sql.manual, |bar| {
                    bar.child(Button::new("sql-commit").xsmall().ghost().label("Commit").disabled(sql.status != Status::Ready).on_click(cx.listener(|this, _, window, cx| this.end_transaction(true, window, cx))))
                        .child(Button::new("sql-rollback").xsmall().ghost().label("Rollback").disabled(sql.status != Status::Ready).on_click(cx.listener(|this, _, window, cx| this.end_transaction(false, window, cx))))
                })
                .when(sql.uncommitted, |bar| bar.child(div().flex_none().text_color(hsla(0.08, 0.9, 0.5, 1.)).child("● uncommitted")))
                .child(div().flex_1())
                .child(scratch)
                .when_some(state, |bar, (text, color)| bar.child(div().flex_none().text_color(color).child(text)))
                .into_any_element(),
        )
    }

    /// Output, then a tab for each result, and what the one shown holds.
    pub(super) fn render_sql_results(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sql = self.sql.as_ref().filter(|s| s.panel)?;
        let muted = cx.theme().muted_foreground;
        let shown = match sql.shown {
            Tab::Result(ix) => sql.results.get(ix).map(|r| (ix, r)),
            _ => None,
        };
        let schema = Button::new("sql-schema-tab").xsmall().ghost().selected(sql.shown == Tab::Schema).label("Schema").on_click(cx.listener(|this, _, window, cx| {
            this.ensure_schema_filter(window, cx);
            if let Some(sql) = &mut this.sql {
                sql.shown = Tab::Schema;
                if sql.schema.borrow().is_none() {
                    this.load_schema(cx);
                }
            }
            cx.notify();
        }));
        let history = Button::new("sql-history-tab").xsmall().ghost().selected(sql.shown == Tab::History).label("History").on_click(cx.listener(|this, _, _, cx| this.show_history(cx)));
        let output = Button::new("sql-output").xsmall().ghost().selected(sql.shown == Tab::Output).label("Output").on_click(cx.listener(|this, _, _, cx| {
            if let Some(sql) = &mut this.sql {
                sql.shown = Tab::Output;
                sql.log_scroll.scroll_to_bottom();
                cx.notify();
            }
        }));
        let tabs: Vec<_> = sql.results.iter().enumerate().map(|(ix, result)| {
            let label: SharedString = if result.pinned { format!("• {}", result.title).into() } else { result.title.clone() };
            Button::new(("sql-result", ix)).xsmall().ghost().selected(sql.shown == Tab::Result(ix)).label(label).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(sql) = &mut this.sql {
                    sql.shown = Tab::Result(ix);
                    cx.notify();
                }
            }))
        }).collect();
        let about = shown.and_then(|(_, r)| r.said.clone());
        let actions = shown.filter(|(_, r)| !r.open).map(|(ix, result)| {
            let this = cx.weak_entity();
            let cut = result.cut;
            let offset = result.origin.as_ref().map_or(0, |o| o.offset);
            let elsewhere = sql.elsewhere(result);
            let paged = elsewhere.is_none() && result.origin.is_some() && (cut || offset > 0);
            let filterable = elsewhere.is_none() && result.origin.is_some();
            let waiting = result.table.read(cx).delegate().changes();
            let can_add = elsewhere.is_none() && sql.schema.borrow().as_ref().is_some_and(|schema| result.table.read(cx).delegate().columns().iter().filter_map(|c| c.source.as_ref()).any(|s| schema.table(&s.table).is_some_and(|t| !t.view)));
            let columns: Vec<(usize, String, bool)> = {
                let delegate = result.table.read(cx).delegate();
                delegate.columns().iter().enumerate().map(|(real, c)| (real, c.name.clone(), delegate.is_visible(real))).collect()
            };
            let table = result.table.clone();
            h_flex()
                .flex_none()
                .flex_wrap()
                .gap_1()
                .when_some(elsewhere, |bar, from| bar.child(div().flex_none().text_color(hsla(0.08, 0.9, 0.45, 1.)).child(format!("from {from}"))))
                .when(waiting > 0, |bar| {
                    bar.child(div().flex_none().text_color(hsla(0.08, 0.9, 0.5, 1.)).child(format!("● {waiting} not saved")))
                        .child(Button::new("sql-edits-review").xsmall().primary().label("Save…").tooltip("Shows what will run; nothing is written before you confirm it").on_click(cx.listener(move |this, _, window, cx| this.review_edits(ix, window, cx))))
                        .child(Button::new("sql-edits-discard").xsmall().ghost().label("Discard").on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(result) = this.sql.as_ref().and_then(|s| s.results.get(ix)) {
                                result.table.update(cx, |t, cx| {
                                    t.delegate_mut().discard_edits();
                                    cx.notify();
                                });
                            }
                            cx.notify();
                        })))
                })
                .when(can_add, |bar| {
                    bar.child(Button::new("sql-add-row").xsmall().ghost().label("+ Row").tooltip("An empty row to fill in: written when you Save").on_click(cx.listener(move |this, _, window, cx| this.add_row(ix, window, cx))))
                })
                .child(Button::new("sql-search").xsmall().ghost().selected(sql.search.is_some()).label("Search").on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))))
                .child(Button::new("sql-columns").xsmall().ghost().label("Columns ▾").dropdown_menu(move |mut menu, _, _| {
                    for (real, name, shown) in &columns {
                        let (table, real) = (table.clone(), *real);
                        menu = menu.item(PopupMenuItem::new(name.clone()).checked(*shown).on_click(move |_, _, cx| {
                            table.update(cx, |t, cx| {
                                t.delegate_mut().toggle_column(real);
                                t.refresh(cx);
                            });
                        }));
                    }
                    menu
                }))
                .child(Button::new("sql-row-mode").xsmall().ghost().selected(result.row_view).label("Row").tooltip("One row, a field a line").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(result) = this.sql.as_mut().and_then(|s| s.results.get_mut(ix)) {
                        result.row_view = !result.row_view;
                        cx.notify();
                    }
                })))
                .when(filterable, |bar| {
                    bar.child(Button::new("sql-filter").xsmall().ghost().label(if result.origin.as_ref().is_some_and(|o| !o.filter.is_empty()) { "Filter ●" } else { "Filter…" }).tooltip("A WHERE condition over this result, run on the server").on_click(cx.listener(move |this, _, window, cx| this.ask_filter(ix, window, cx))))
                })
                .when(paged, |bar| {
                    bar.child(Button::new("sql-page-prev").xsmall().ghost().label("◀").disabled(offset == 0).on_click(cx.listener(move |this, _, window, cx| this.read_window(ix, None, offset.saturating_sub(MOST_ROWS), window, cx))))
                        .child(div().flex_none().child(format!("from {}", offset + 1)))
                        .child(Button::new("sql-page-next").xsmall().ghost().label("▶").disabled(!cut).on_click(cx.listener(move |this, _, window, cx| this.read_window(ix, None, offset + MOST_ROWS, window, cx))))
                })
                .child(Button::new("sql-export").xsmall().ghost().label("Export ▾").dropdown_menu(move |mut menu, _, _| {
                    for format in Format::ALL {
                        let copy = this.clone();
                        menu = menu.item(PopupMenuItem::new(format!("Copy as {}", format.name())).on_click(move |_, _, cx| {
                            let _ = copy.update(cx, |f, cx| f.copy_as(ix, format, cx));
                        }));
                    }
                    menu = menu.separator();
                    for format in Format::ALL {
                        let save = this.clone();
                        let label = format!("Save as {}…", format.name());
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ = save.update(cx, |f, cx| f.ask_save(ix, format, window, cx));
                        }));
                    }
                    menu
                }))
                .child(Button::new("sql-pin").xsmall().ghost().label(if result.pinned { "Unpin" } else { "Pin" }).tooltip("Stays when the next statement runs; the new result opens after it").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(result) = this.sql.as_mut().and_then(|s| s.results.get_mut(ix)) {
                        result.pinned = !result.pinned;
                        cx.notify();
                    }
                })))
                .child(Button::new("sql-result-close").xsmall().ghost().label("Close tab").disabled(waiting > 0).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(sql) = &mut this.sql
                        && ix < sql.results.len()
                    {
                        sql.results.remove(ix);
                        sql.shown = match ix.checked_sub(1).or((!sql.results.is_empty()).then_some(0)).filter(|_| !sql.results.is_empty()) {
                            Some(next) => Tab::Result(next),
                            None => Tab::Output,
                        };
                        cx.notify();
                    }
                })))
        });
        let body: AnyElement = match shown {
            Some((_, result)) if result.row_view => self.render_row_view(result, cx),
            Some((_, result)) => v_flex()
                .id("sql-table")
                .test_support()
                .size_full()
                .when_some(sql.search.clone(), |view, search| {
                    let found = result.table.read(cx).delegate().shown_len();
                    let all = result.table.read(cx).delegate().len();
                    view.child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .items_center()
                            .text_xs()
                            .child(div().w(px(320.)).child(gpui_kit::component::input::Input::new(&search).xsmall()))
                            .child(div().text_color(muted).child(format!("{found} of {all} rows"))),
                    )
                })
                .child(div().flex_1().min_h_0().child(DataTable::new(&result.table).stripe(true).bordered(false)))
                .when_some(sql.summary.as_ref().filter(|(key, _)| Some(key.0) == shown.map(|(ix, _)| ix)).map(|(_, s)| s.clone()), |view, s| {
                    let mut parts = vec![format!("{} cells", s.cells)];
                    if s.nulls > 0 {
                        parts.push(format!("{} NULL", s.nulls));
                    }
                    if let (Some(sum), Some(average), Some(least), Some(most)) = (&s.sum, &s.average, &s.least, &s.most) {
                        parts.push(format!("sum {sum}"));
                        parts.push(format!("avg {average}"));
                        parts.push(format!("min {least}"));
                        parts.push(format!("max {most}"));
                    }
                    view.child(h_flex().id("sql-summary").test_support().flex_none().justify_end().px_2().py(px(2.)).text_xs().text_color(muted).border_t_1().border_color(cx.theme().border).child(parts.join(" · ")))
                })
                .into_any_element(),
            None if sql.shown == Tab::Schema => self.render_sql_schema(cx),
            None if sql.shown == Tab::History => self.render_sql_history(sql, cx),
            None => self.render_sql_log(sql, cx),
        };
        Some(
            v_flex()
                .id("sql-results")
                .test_support()
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .flex_none()
                        .flex_wrap()
                        .gap_1()
                        .px_2()
                        .py(px(2.))
                        .items_center()
                        .text_xs()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(output)
                        .child(schema)
                        .child(history)
                        .children(tabs)
                        .child(div().flex_1())
                        .when_some(about, |bar, about| bar.child(div().flex_none().text_color(muted).child(about)))
                        .children(actions)
                        .child(Button::new("sql-close").xsmall().ghost().label("×").tooltip("Close the results").on_click(cx.listener(|this, _, _, cx| {
                            if let Some(sql) = &mut this.sql {
                                sql.panel = false;
                                cx.notify();
                            }
                        }))),
                )
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element(),
        )
    }

    /// The console: what ran, what it did, what failed, oldest first.
    fn render_sql_log(&self, sql: &SqlFile, cx: &mut Context<Self>) -> AnyElement {
        let (muted, foreground) = (cx.theme().muted_foreground, cx.theme().foreground);
        let lines: Vec<_> = sql.log.iter().enumerate().map(|(ix, line)| {
            let color = match line.kind {
                LogKind::Failed => gpui_kit::red(),
                LogKind::Statement => foreground,
                LogKind::Info => muted,
            };
            let place = line.place.clone();
            h_flex()
                .id(("sql-log", ix))
                .gap_3()
                .items_start()
                .child(div().flex_none().text_color(muted).child(line.at.clone()))
                .child(div().flex_1().min_w_0().text_color(color).child(crate::copying::text(("sql-log-text", ix), line.text.clone())))
                .when_some(place, |row, place| {
                    row.child(Button::new(("sql-log-go", ix)).xsmall().ghost().label("Go to").on_click(cx.listener(move |this, _, window, cx| this.go_to_statement(place.clone(), window, cx))))
                })
        }).collect();
        v_flex()
            .id("sql-output-log")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&sql.log_scroll)
            .gap_0p5()
            .px_3()
            .py_2()
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .when(sql.log.is_empty(), |log| log.child(div().text_color(muted).child("Nothing has run yet")))
            .children(lines)
            .into_any_element()
    }
}

fn new_table(columns: Vec<ColumnInfo>, window: &mut Window, cx: &mut Context<FileView>) -> Entity<TableState<SqlRows>> {
    let table = cx.new(|cx| TableState::new(SqlRows::new(columns), window, cx).col_movable(false).cell_selectable(true).loop_selection(false));
    cx.subscribe_in(&table, window, |this: &mut FileView, table, event: &TableEvent, window, cx| {
        if let TableEvent::DoubleClickedCell(row, col) = event {
            this.edit_cell(table.clone(), *row, *col, window, cx);
        }
    })
    .detach();
    cx.observe(&table, |this: &mut FileView, table, cx| this.summarize_selection(&table, cx)).detach();
    let (file, weak) = (cx.weak_entity(), table.downgrade());
    table.update(cx, |t, _| t.delegate_mut().set_menu(Rc::new(move |rows, at, real, menu, cx| cell_menu(&file, &weak, rows, at, real, menu, cx))));
    cx.subscribe(&table, |_: &mut FileView, table, event: &TableEvent, cx| {
        if let TableEvent::ColumnWidthsChanged(widths) = event {
            table.update(cx, |t, _| t.delegate_mut().remember_widths(widths));
        }
    })
    .detach();
    table
}

impl FileView {
    /// Where a change of this cell goes; None when the server's answer does not tell.
    fn cell_target(&self, table: &Entity<TableState<SqlRows>>, at: u32, real: usize, cx: &App) -> Option<CellTarget> {
        let sql = self.sql.as_ref()?;
        let driver = sql.connection.as_deref().and_then(config::find)?.driver;
        if driver == Driver::Redis {
            return None;
        }
        let result = sql.results.iter().find(|r| r.table == *table)?;
        // A change goes only to the database its row was read from.
        if !sql.is_from_here(result) {
            return None;
        }
        if result.origin.as_ref().is_some_and(|o| statement::top_words(&o.base).iter().any(|w| w == "union" || w == "group")) {
            return None;
        }
        let schema = sql.schema.borrow().clone()?;
        let delegate = table.read(cx).delegate();
        if delegate.is_added(at) {
            let source = delegate.columns().get(real)?.source.as_ref()?;
            if source.database.as_ref().is_some_and(|d| !d.eq_ignore_ascii_case(&schema.database)) {
                return None;
            }
            schema.table(&source.table).filter(|t| !t.view)?;
            return Some(CellTarget { table: source.table.clone(), column: source.column.clone(), key: Vec::new() });
        }
        let target = edit_target(&schema, delegate.columns(), real)?;
        // The key as it was read: a change waiting on it does not move the row.
        let key: Option<Vec<(String, String)>> = target.key.iter().map(|(name, place)| delegate.read_value(at, *place).filter(|c| !c.is_null()).map(|c| (name.clone(), export::sql_value(c, driver)))).collect();
        Some(CellTarget { table: target.table, column: target.column, key: key? })
    }

    /// A new value for a cell, or NULL: it waits for Save.
    fn stage_cell(&mut self, table: &Entity<TableState<SqlRows>>, at: u32, real: usize, value: Option<String>, cx: &mut Context<Self>) {
        use crate::db::value::{Cell, Kind};
        let Some(target) = self.cell_target(table, at, real, cx) else { return };
        let Some(driver) = self.sql.as_ref().and_then(|s| s.connection.as_deref()).and_then(config::find).map(|c| c.driver) else { return };
        let kind = table.read(cx).delegate().columns().get(real).map_or(Kind::Text, |c| c.kind);
        let (literal, new_value) = match value {
            None => ("NULL".to_string(), Cell::Null),
            Some(text) => {
                let literal = if kind.right_aligned() { statement::literal(&text, driver) } else { driver.string(&text) };
                (literal, kind.cell(mysql::Value::Bytes(text.into_bytes())))
            }
        };
        let edit = StagedEdit { table: target.table, column: target.column, key: target.key, literal, value: new_value };
        table.update(cx, |t, cx| {
            t.delegate_mut().stage(at, real, edit);
            cx.notify();
        });
        cx.notify();
    }

    /// A double click: typed in place where it can change and fits, else shown whole.
    fn edit_cell(&mut self, table: Entity<TableState<SqlRows>>, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        use crate::db::value::Cell;
        use gpui_kit::component::input::InputState;
        let delegate = table.read(cx).delegate();
        let (real, Some(at)) = (delegate.real_column(col), delegate.row_id(row)) else { return };
        let current = delegate.value(at, real).cloned().unwrap_or(Cell::Null);
        let text = if current.is_null() { String::new() } else { current.copied().into_owned() };
        let in_place = !delegate.is_deleted(at) && !matches!(current, Cell::Bytes(_)) && !text.contains('\n') && text.chars().count() <= 500 && self.cell_target(&table, at, real, cx).is_some();
        if !in_place {
            return self.open_value(table, at, real, window, cx);
        }
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |i, cx| i.set_value(text.clone(), window, cx));
        let (held, typed, was_null) = (table.clone(), text, current.is_null());
        cx.subscribe(&input, move |this: &mut FileView, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.finish_cell(&held, &input, &typed, was_null, cx);
            }
        })
        .detach();
        table.update(cx, |t, cx| {
            t.delegate_mut().begin_edit(at, real, input.clone());
            cx.notify();
        });
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    fn finish_cell(&mut self, table: &Entity<TableState<SqlRows>>, input: &Entity<gpui_kit::component::input::InputState>, before: &str, was_null: bool, cx: &mut Context<Self>) {
        if !table.read(cx).delegate().is_editing(input) {
            return;
        }
        let text = input.read(cx).value().to_string();
        let Some((at, real, _)) = table.update(cx, |t, cx| {
            cx.notify();
            t.delegate_mut().end_edit()
        }) else {
            return;
        };
        // Left as it was: a NULL does not become an empty string for having been looked at.
        if text != before {
            self.stage_cell(table, at, real, Some(text), cx);
        } else if was_null {
            cx.notify();
        }
    }

    fn add_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.sql.as_ref().and_then(|s| s.results.get(ix)).map(|r| r.table.clone()) else { return };
        let row = table.update(cx, |t, cx| {
            let row = t.delegate_mut().add_row();
            t.refresh(cx);
            t.set_selected_cell(row, 0, cx);
            row
        });
        self.edit_cell(table, row, 0, window, cx);
    }

    fn toggle_delete_row(&mut self, table: &Entity<TableState<SqlRows>>, at: u32, cx: &mut Context<Self>) {
        let delegate = table.read(cx).delegate();
        let added = delegate.is_added(at);
        let columns = delegate.columns().len();
        // Any column of the row tells its table and key.
        let target = if added { Some(CellTarget { table: String::new(), column: String::new(), key: Vec::new() }) } else { (0..columns).find_map(|real| self.cell_target(table, at, real, cx)) };
        let Some(target) = target else { return };
        table.update(cx, |t, cx| {
            t.delegate_mut().toggle_delete(at, target.table, target.key);
            t.refresh(cx);
        });
        cx.notify();
    }

    /// A value whole, in a dialog: to read, and where it can change, to change.
    fn open_value(&mut self, table: Entity<TableState<SqlRows>>, at: u32, real: usize, window: &mut Window, cx: &mut Context<Self>) {
        use crate::db::value::Cell;
        use gpui_kit::component::input::{Textarea, TextareaState};
        let delegate = table.read(cx).delegate();
        let current = delegate.value(at, real).cloned().unwrap_or(Cell::Null);
        let value = if current.is_null() { String::new() } else { current.copied().into_owned() };
        let binary = matches!(current, Cell::Bytes(_));
        let deleted = delegate.is_deleted(at);
        let can_edit = !binary && !deleted && self.cell_target(&table, at, real, cx).is_some();
        let shown: String = value.chars().take(VALUE_SHOWN).collect();
        let cut = shown.len() < value.len();
        let json = looks_like_json(&shown);
        let area = cx.new(|cx| TextareaState::new(window, cx).auto_grow(6, 24));
        area.update(cx, |a, cx| a.set_value(shown.clone(), window, cx));
        let focused = area.clone();
        let is_null = current.is_null();
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (field, value, formatting) = (area.clone(), value.clone(), area.clone());
            let update = can_edit && !cut;
            let (table, area, original, this) = (table.clone(), area.clone(), shown.clone(), this.clone());
            let save = Rc::new(move |to_null: bool, window: &mut Window, cx: &mut App| {
                let text = area.read(cx).value().to_string();
                window.close_dialog(cx);
                if !to_null && text == original && !is_null {
                    return;
                }
                if !to_null && text.is_empty() && is_null {
                    return;
                }
                let table = table.clone();
                let _ = this.update(cx, |f, cx| f.stage_cell(&table, at, real, (!to_null).then_some(text), cx));
                window.refresh();
            });
            let to_null = save.clone();
            dialog
                .title(if cut { "Value (its beginning; Copy takes it whole)" } else { "Value" })
                .w(px(720.))
                .content(move |content, _, _| content.child(Textarea::new(&field)))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .when(json, |row| {
                            row.child(Button::new("sql-value-json").ghost().label("Format JSON").on_click(move |_, window, cx| {
                                let text = formatting.read(cx).value().to_string();
                                if let Some(pretty) = pretty_json(&text) {
                                    formatting.update(cx, |a, cx| a.set_value(pretty, window, cx));
                                }
                            }))
                        })
                        .child(Button::new("sql-value-copy").ghost().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(value.clone()))))
                        .when(update, |row| {
                            row.child(Button::new("sql-value-null").ghost().label("Set NULL").on_click(move |_, window, cx| to_null(true, window, cx)))
                                .child(Button::new("sql-value-update").ghost().label("Change").tooltip("Waits with the other changes until you press Save").on_click(move |_, window, cx| save(false, window, cx)))
                        })
                        .child(Button::new("sql-value-close").primary().label("Close").on_click(|_, window, cx| window.close_dialog(cx))),
                )
        });
        focused.update(cx, |a, cx| a.focus(window, cx));
    }

    /// Adds up the cells selected, off the UI thread, once for each selection.
    fn summarize_selection(&mut self, table: &Entity<TableState<SqlRows>>, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        let Tab::Result(ix) = sql.shown else { return };
        if sql.results.get(ix).is_none_or(|r| r.table != *table) {
            return;
        }
        let state = table.read(cx);
        let Some((rows, cols)) = state.selected_cells().filter(|(rows, cols)| rows.len() * cols.len() > 1) else {
            if sql.summary.take().is_some() {
                sql._summary = Task::ready(());
                cx.notify();
            }
            return;
        };
        let delegate = state.delegate();
        let key: SelectionKey = (ix, rows.clone(), cols.clone(), delegate.changes());
        if sql.summary.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (_, all, order) = delegate.snapshot();
        let reals: Vec<usize> = cols.map(|c| delegate.real_column(c)).collect();
        sql._summary = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            cx.background_executor().timer(Duration::from_millis(60)).await;
            let summary = cx
                .background_spawn(async move {
                    let mut cells = Vec::new();
                    for &row in &order[rows.start.min(order.len())..rows.end.min(order.len())] {
                        if let Some(values) = all.get(row as usize) {
                            cells.extend(reals.iter().filter_map(|&c| values.get(c)));
                        }
                    }
                    crate::db::value::summarize(cells.into_iter())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(sql) = &mut this.sql {
                    sql.summary = Some((key, summary));
                }
                cx.notify();
            });
        });
    }

    /// Frames the statement the cursor is in, once it stays a moment.
    pub(super) fn sql_cursor_moved(&mut self, cx: &mut Context<Self>) {
        let version = self.edit_version;
        let editor = self.editor.read(cx);
        let (cursor, selecting) = (editor.cursor(), !editor.selected_range().is_empty());
        let rope = editor.text().clone();
        let Some(sql) = &mut self.sql else { return };
        if sql.cursor_seen == (cursor, version) {
            return;
        }
        sql.cursor_seen = (cursor, version);
        let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
        sql._cursor = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let place = if selecting {
                None
            } else {
                cx.background_spawn(async move {
                    let text = rope.to_string();
                    statement::at(&text, &statement::split_for(driver, &text), cursor)
                })
                .await
            };
            let _ = this.update(cx, |this, cx| {
                let color = cx.theme().muted_foreground.opacity(0.45);
                let editor = this.editor.clone();
                let Some(sql) = &mut this.sql else { return };
                let marks: Vec<RangeDecoration> = place.into_iter().map(|p| RangeDecoration::new(p).with_style(RangeDecorationStyle::Frame).with_color(color)).collect();
                match &sql.cursor_mark {
                    Some(mark) => mark.set(marks, cx),
                    None => sql.cursor_mark = Some(editor.update(cx, |state, cx| state.create_range_decorations_collection(marks, cx))),
                }
            });
        });
    }

    /// The selection, written out for pasting.
    fn copy_selection(&mut self, table: &WeakEntity<TableState<SqlRows>>, how: CopyAs, cx: &mut Context<Self>) {
        let Some(table) = table.upgrade() else { return };
        let Some(sql) = &self.sql else { return };
        let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
        let state = table.read(cx);
        let Some((rows, cols)) = state.selected_cells() else { return };
        let delegate = state.delegate();
        let reals: Vec<usize> = cols.map(|c| delegate.real_column(c)).collect();
        let names: Vec<String> = reals.iter().map(|&c| delegate.columns()[c].name.clone()).collect();
        let picked: Vec<Vec<crate::db::value::Cell>> = rows.filter_map(|r| delegate.row_id(r)).map(|at| reals.iter().filter_map(|&c| delegate.value(at, c).cloned()).collect()).collect();
        let source = reals.first().and_then(|&c| delegate.columns()[c].source.as_ref()).map(|s| s.table.clone());
        let name = source.or_else(|| sql.results.iter().find(|r| r.table == table).map(|r| r.title.to_string())).unwrap_or_else(|| "result".into());
        let copying = cx.spawn(async move |this: WeakEntity<FileView>, cx| {
            let text = cx
                .background_spawn(async move {
                    match how {
                        CopyAs::Format(format) => export::to_string(format, names, &name, driver, picked.iter().map(Vec::as_slice)),
                        CopyAs::InList => {
                            let mut seen = std::collections::BTreeSet::new();
                            let values: Vec<String> = picked.iter().flatten().filter(|c| !c.is_null()).map(|c| export::sql_value(c, driver)).filter(|v| seen.insert(v.clone())).collect();
                            format!("({})", values.join(", "))
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
        });
        if let Some(sql) = &mut self.sql {
            sql._export = copying;
        }
    }

    /// The result narrowed to what is true of a cell, on the server.
    fn add_filter(&mut self, table: &WeakEntity<TableState<SqlRows>>, condition: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        let Some(ix) = sql.results.iter().position(|r| r.table.downgrade() == *table) else { return };
        let old = sql.results[ix].origin.as_ref().map(|o| o.filter.trim().to_string()).unwrap_or_default();
        let filter = if old.is_empty() { condition } else { format!("({old}) AND {condition}") };
        self.read_window(ix, Some(filter), 0, window, cx);
    }

    /// A row a foreign key leads to, in a result of its own beside this one.
    fn open_related(&mut self, statement: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sql) = &mut self.sql {
            if sql.busy() {
                return;
            }
            sql.keep_results = true;
        }
        self.run_text(statement, window, cx);
    }

    /// A CSV file into a table: its columns matched by name, shown, then run on a word.
    pub(super) fn import_csv(&mut self, table: String, window: &mut Window, cx: &mut Context<Self>) {
        let again = table.clone();
        if self.production_asks(move |f, window, cx| f.import_csv(again.clone(), window, cx), window, cx) {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        let importing = cx.spawn_in(window, async move |this: WeakEntity<FileView>, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let reading = path.clone();
            let looked = cx.background_spawn(async move { crate::db::csv::look_at(&reading) }).await;
            let _ = this.update_in(cx, |this, window, cx| this.confirm_import(table, path, looked, window, cx));
        });
        if let Some(sql) = &mut self.sql {
            sql._pick = importing;
        }
    }

    fn confirm_import(&mut self, table: String, path: PathBuf, looked: std::io::Result<(Vec<String>, usize, u8)>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        let known = sql.schema.borrow().as_ref().and_then(|s| s.table(&table).map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())).unwrap_or_default();
        let problem = match &looked {
            Err(e) => Some(format!("{}: {e}", path.display())),
            Ok((header, ..)) => {
                let unknown: Vec<&String> = header.iter().filter(|h| !known.iter().any(|k| k.eq_ignore_ascii_case(h))).collect();
                (!unknown.is_empty()).then(|| format!("{table} has no column {}", unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")))
            }
        };
        if let Some(problem) = problem {
            sql.say(LogKind::Failed, format!("Nothing was imported: {problem}"), None);
            sql.panel = true;
            sql.shown = Tab::Output;
            return cx.notify();
        }
        let Ok((header, count, separator)) = looked else { return };
        let columns: Vec<String> = header.iter().filter_map(|h| known.iter().find(|k| k.eq_ignore_ascii_case(h)).cloned()).collect();
        let empty_is_null = Rc::new(std::cell::Cell::new(true));
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (about, list, flag, read_flag, this, table, columns, path) = (format!("{count} rows from {} into {table}.", path.display()), columns.join(", "), empty_is_null.clone(), empty_is_null.clone(), this.clone(), table.clone(), columns.clone(), path.clone());
            dialog
                .title("Import this file?")
                .w(px(640.))
                .content(move |content, _, _| {
                    let flag = flag.clone();
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child(about.clone()))
                            .child(div().text_xs().text_color(muted).child(format!("Columns: {list}")))
                            .child(gpui_kit::component::checkbox::Checkbox::new("sql-import-null").checked(flag.get()).label("An empty field is NULL (\"\" stays an empty string)").on_click(move |checked, window, _| {
                                flag.set(*checked);
                                window.refresh();
                            }))
                            .child(div().text_xs().text_color(muted).child("In one transaction: all of the rows are kept, or none.")),
                    )
                })
                .footer(h_flex().justify_end().gap_2().child(Button::new("sql-import-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                    Button::new("sql-import-go").primary().label("Import").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let (table, columns, path, empty) = (table.clone(), columns.clone(), path.clone(), read_flag.get());
                        let _ = this.update(cx, |f, cx| {
                            let Some(sql) = &mut f.sql else { return };
                            if sql.status != Status::Ready {
                                sql.say(LogKind::Failed, "Not connected: nothing was imported", None);
                                return cx.notify();
                            }
                            if let Some(session) = &sql.session {
                                sql.status = Status::Running;
                                if sql.manual {
                                    sql.uncommitted = true;
                                }
                                sql.panel = true;
                                sql.shown = Tab::Output;
                                session.import_csv(table, columns, path, separator, empty);
                            }
                            cx.notify();
                        });
                    }),
                ))
        });
    }

    /// The name at the cursor, in the statement around it.
    fn name_at_cursor(&self, cx: &App) -> Option<crate::db::schema::NameAt> {
        let sql = self.sql.as_ref()?;
        let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
        let editor = self.editor.read(cx);
        let (text, cursor) = (editor.text().to_string(), editor.cursor());
        let around = statement::at(&text, &statement::split_for(driver, &text), cursor)?;
        if cursor < around.start || cursor > around.end {
            return None;
        }
        let schema = sql.schema.borrow().clone();
        let found = crate::db::schema::name_at(schema.as_deref(), &text[around.clone()], cursor - around.start)?;
        Some(match found {
            crate::db::schema::NameAt::Alias(place) => crate::db::schema::NameAt::Alias(place.start + around.start..place.end + around.start),
            other => other,
        })
    }

    fn table_at_cursor(&self, cx: &App) -> Option<String> {
        use crate::db::schema::NameAt;
        match self.name_at_cursor(cx)? {
            NameAt::Table(table) | NameAt::Column { table, .. } => Some(table),
            NameAt::Alias(_) => None,
        }
    }

    /// The table in the Schema tab, opened, its keys and indexes under it.
    fn show_in_schema(&mut self, table: String, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_schema_filter(window, cx);
        let Some(sql) = &mut self.sql else { return };
        sql.panel = true;
        sql.shown = Tab::Schema;
        sql.schema_open.insert(table.clone());
        if let Some(filter) = sql.schema_filter.clone() {
            filter.update(cx, |f, cx| f.set_value("", window, cx));
        }
        self.refresh_schema_rows(cx);
        let Some(sql) = &mut self.sql else { return };
        let row = sql.schema_rows.iter().position(|r| matches!(r, SchemaRow::Table { name, .. } if name.as_ref() == table));
        if let Some(row) = row {
            sql.schema_scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Ctrl+B: a language server's answer if there is one; else an alias's
    /// declaration, the name in the schema, or a search of the project.
    pub(super) fn sql_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::db::schema::NameAt;
        use gpui_kit::component::input::GoToDefinition;
        let found = if self.lsp_client.is_some() { None } else { self.name_at_cursor(cx) };
        match found {
            // The cursor goes there, nothing selected: the next key must not type over the name.
            Some(NameAt::Alias(place)) => self.editor.update(cx, |state, cx| state.set_selected_range(place.start..place.start, cx)),
            Some(NameAt::Table(table) | NameAt::Column { table, .. }) => self.show_in_schema(table, window, cx),
            None => window.dispatch_action(Box::new(GoToDefinition), cx),
        }
    }

    pub(super) fn show_table_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.table_at_cursor(cx) {
            Some(table) => {
                let text = self.table_data_statement(&table);
                self.run_text(text, window, cx);
            }
            None => self.say_no_table(cx),
        }
    }

    pub(super) fn show_ddl_at_cursor(&mut self, cx: &mut Context<Self>) {
        match self.table_at_cursor(cx) {
            Some(table) => self.show_table_definition(table, cx),
            None => self.say_no_table(cx),
        }
    }

    fn say_no_table(&mut self, cx: &mut Context<Self>) {
        if let Some(sql) = &mut self.sql {
            let why = if sql.schema.borrow().is_some() { "The cursor is not on a table or a column of this database" } else { "Connect first: the schema tells what this name is" };
            sql.say(LogKind::Info, why, None);
            sql.panel = true;
            sql.shown = Tab::Output;
        }
        cx.notify();
    }

    /// The statements the waiting changes make, before they run.
    fn review_edits(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = &self.sql else { return };
        let Some(result) = sql.results.get(ix) else { return };
        let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
        let statements = result.table.read(cx).delegate().save_statements(driver);
        let adds = result.table.read(cx).delegate().edits().keys().any(|(at, _)| result.table.read(cx).delegate().is_added(*at));
        let manual = sql.manual;
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (mono, muted) = (cx.theme().mono_font_family.clone(), cx.theme().muted_foreground);
            let (list, run, this) = (statements.clone(), statements.clone(), this.clone());
            dialog
                .title(if statements.len() == 1 { "Save this change?".to_string() } else { format!("Save these {} changes?", statements.len()) })
                .w(px(760.))
                .content(move |content, _, _| {
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_xs().text_color(muted).child(if manual { "They join the open transaction, and wait for Commit." } else { "In one transaction: all of them are kept, or none." }))
                            .child(v_flex().id("sql-review-list").max_h(px(380.)).overflow_y_scroll().gap_1().text_sm().font_family(mono.clone()).children(list.iter().map(|s| div().child(s.clone())))),
                    )
                })
                .footer(h_flex().justify_end().gap_2().child(Button::new("sql-review-cancel").ghost().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx))).child(
                    Button::new("sql-review-apply").primary().label("Save").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let run = run.clone();
                        let _ = this.update(cx, |f, cx| {
                            if let Some(sql) = &mut f.sql {
                                sql.applying_added = adds;
                            }
                            f.apply_edits(ix, run, window, cx)
                        });
                    }),
                ))
        });
    }

    fn apply_edits(&mut self, ix: usize, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let again = statements.clone();
        if self.production_asks(move |f, window, cx| f.apply_edits(ix, again.clone(), window, cx), window, cx) {
            return;
        }
        let Some(sql) = &mut self.sql else { return };
        if sql.status != Status::Ready {
            sql.say(LogKind::Failed, "Not connected: the changes still wait for Save", None);
            sql.shown = Tab::Output;
            return cx.notify();
        }
        if let Some(from) = sql.results.get(ix).and_then(|r| sql.elsewhere(r)) {
            sql.say(LogKind::Failed, format!("Not saved: these rows are from {from}. Go back to it, or discard the changes"), None);
            sql.shown = Tab::Output;
            return cx.notify();
        }
        for text in &statements {
            sql.say(LogKind::Statement, one_line(text), None);
        }
        if let Some(session) = &sql.session {
            sql.applying = Some(ix);
            sql.status = Status::Running;
            session.apply_batch(statements);
        }
        cx.notify();
    }

    /// Opens or closes the search over the results.
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::InputState;
        let Some(sql) = &mut self.sql else { return };
        if sql.search.take().is_some() {
            for result in &sql.results {
                result.table.update(cx, |t, cx| {
                    t.delegate_mut().set_hits(None);
                    cx.notify();
                });
            }
            return cx.notify();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search in this result"));
        cx.subscribe(&input, |this: &mut FileView, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.search_result(cx);
            }
        })
        .detach();
        input.update(cx, |i, cx| i.focus(window, cx));
        sql.search = Some(input);
        cx.notify();
    }

    /// After a pause in typing, and off the UI thread: it reads every cell.
    fn search_result(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = &mut self.sql else { return };
        let Tab::Result(ix) = sql.shown else { return };
        let (Some(result), Some(input)) = (sql.results.get(ix), &sql.search) else { return };
        let needle = input.read(cx).value().to_string();
        let (table, rows) = (result.table.clone(), result.table.read(cx).delegate().all_rows());
        sql._search = cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let hits = if needle.is_empty() { None } else { Some(cx.background_spawn(async move { search_hits(&rows, &needle) }).await) };
            let _ = table.update(cx, |t, cx| {
                t.delegate_mut().set_hits(hits);
                cx.notify();
            });
        });
    }

    /// One row, a field a line: for a table too wide to read across.
    fn render_row_view(&self, result: &SqlResult, cx: &mut Context<Self>) -> AnyElement {
        let table = result.table.clone();
        let (muted, border) = (cx.theme().muted_foreground, cx.theme().border);
        let state = table.read(cx);
        let delegate = state.delegate();
        let count = delegate.shown_len();
        let row = state.selected_cell().map_or(0, |(row, _)| row).min(count.saturating_sub(1));
        let at = delegate.row_id(row);
        let fields: Vec<_> = delegate
            .columns()
            .iter()
            .enumerate()
            .map(|(real, column)| {
                let cell = at.and_then(|at| delegate.value(at, real));
                let null = cell.is_none_or(|c| c.is_null());
                let text: SharedString = cell.map_or("NULL".to_string(), |c| c.shown().into_owned()).into();
                h_flex()
                    .gap_3()
                    .py(px(3.))
                    .items_start()
                    .border_b_1()
                    .border_color(border)
                    .child(div().w(px(220.)).flex_none().truncate().text_color(muted).child(column.name.clone()))
                    .child(div().flex_1().min_w_0().when(null, |v| v.italic().text_color(muted)).child(text))
            })
            .collect();
        let step = |id: &'static str, label: &'static str, to: Option<usize>, table: Entity<TableState<SqlRows>>, cx: &mut Context<Self>| {
            Button::new(id).xsmall().ghost().label(label).disabled(to.is_none()).on_click(cx.listener(move |_, _, _, cx| {
                if let Some(to) = to {
                    table.update(cx, |t, cx| t.set_selected_cell(to, 0, cx));
                    cx.notify();
                }
            }))
        };
        let previous = step("sql-row-prev", "◀", row.checked_sub(1), table.clone(), cx);
        let next = step("sql-row-next", "▶", (row + 1 < count).then_some(row + 1), table, cx);
        v_flex()
            .id("sql-row-view")
            .test_support()
            .size_full()
            .text_xs()
            .child(h_flex().flex_none().gap_2().px_3().py_1().items_center().child(previous).child(div().text_color(muted).child(format!("row {} of {count}", row + 1))).child(next))
            .child(v_flex().id("sql-row-fields").flex_1().min_h_0().overflow_y_scroll().px_3().children(fields))
            .into_any_element()
    }
}

/// A cell's menu past Copy: what it can be pasted as, what it narrows the
/// result to, where its foreign keys lead, and what can be done to its row.
fn cell_menu(file: &WeakEntity<FileView>, table: &WeakEntity<TableState<SqlRows>>, rows: &SqlRows, at: u32, real: usize, mut menu: gpui_kit::component::menu::PopupMenu, cx: &App) -> gpui_kit::component::menu::PopupMenu {
    let Some(view) = file.upgrade() else { return menu };
    let f = view.read(cx);
    let Some(sql) = &f.sql else { return menu };
    let driver = sql.connection.as_deref().and_then(config::find).map_or(Driver::MySql, |c| c.driver);
    menu = menu.separator().label("Copy selection as");
    let kinds = [("TSV with headers", CopyAs::Format(Format::Tsv)), ("CSV", CopyAs::Format(Format::Csv)), ("JSON", CopyAs::Format(Format::Json)), ("Markdown", CopyAs::Format(Format::Markdown)), ("SQL INSERT", CopyAs::Format(Format::Insert)), ("IN (…) list", CopyAs::InList)];
    for (label, how) in kinds {
        if driver == Driver::Redis && matches!(how, CopyAs::InList | CopyAs::Format(Format::Insert)) {
            continue;
        }
        let (file, table) = (file.clone(), table.clone());
        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
            let _ = file.update(cx, |f, cx| f.copy_selection(&table, how, cx));
        }));
    }
    let Some(column) = rows.columns().get(real) else { return menu };
    let value = rows.read_value(at, real).cloned();
    let result = sql.results.iter().find(|r| r.table.downgrade() == *table);
    let here = result.is_some_and(|r| sql.is_from_here(r));
    if here && driver != Driver::Redis && result.is_some_and(|r| r.origin.is_some()) && !rows.is_added(at) {
        let name = driver.quoted(&column.name);
        let conditions: Vec<(String, String)> = match &value {
            Some(cell) if cell.is_null() => vec![(format!("{} IS NULL", column.name), format!("{name} IS NULL")), (format!("{} IS NOT NULL", column.name), format!("{name} IS NOT NULL"))],
            Some(cell) => {
                let literal = export::sql_value(cell, driver);
                let shown: String = literal.chars().take(32).collect();
                vec![(format!("{} = {shown}", column.name), format!("{name} = {literal}")), (format!("{} <> {shown}", column.name), format!("{name} <> {literal}"))]
            }
            None => Vec::new(),
        };
        menu = menu.separator().label("Filter");
        for (label, condition) in conditions {
            let (file, table) = (file.clone(), table.clone());
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = file.update(cx, |f, cx| f.add_filter(&table, condition.clone(), window, cx));
            }));
        }
    }
    let schema = sql.schema.borrow().clone();
    if let (true, Some(schema), Some(source), Some(cell)) = (here, &schema, &column.source, value.as_ref().filter(|c| !c.is_null()))
        && source.database.as_ref().is_none_or(|d| d.eq_ignore_ascii_case(&schema.database))
    {
        let literal = export::sql_value(cell, driver);
        let shown: String = literal.chars().take(24).collect();
        let out = schema.foreign_keys.iter().filter(|k| k.table == source.table && k.column == source.column).map(|k| (&k.to_table, &k.to_column));
        let back = schema.foreign_keys.iter().filter(|k| k.to_table == source.table && k.to_column == source.column).map(|k| (&k.table, &k.column));
        let links: Vec<(String, String)> = out.chain(back).map(|(table, col)| (format!("{table} where {col} = {shown}"), format!("SELECT * FROM {} WHERE {} = {literal} LIMIT 500", driver.quoted(table), driver.quoted(col)))).collect();
        if !links.is_empty() {
            menu = menu.separator().label("Open related");
            for (label, statement) in links {
                let file = file.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = file.update(cx, |f, cx| f.open_related(statement.clone(), window, cx));
                }));
            }
        }
    }
    if let Some(strong) = table.upgrade() {
        let open = {
            let (file, strong) = (file.clone(), strong.clone());
            PopupMenuItem::new("Show value…").on_click(move |_, window, cx| {
                let strong = strong.clone();
                let _ = file.update(cx, |f, cx| f.open_value(strong, at, real, window, cx));
            })
        };
        menu = menu.separator().item(open);
        let editable = here && !rows.is_deleted(at) && column.source.is_some() && driver != Driver::Redis && result.is_some() && schema.as_ref().is_some_and(|s| column.source.as_ref().is_some_and(|c| s.table(&c.table).is_some_and(|t| !t.view)));
        if editable {
            let (file_null, table_null) = (file.clone(), strong.clone());
            menu = menu.item(PopupMenuItem::new("Set NULL").on_click(move |_, _, cx| {
                let _ = file_null.update(cx, |f, cx| f.stage_cell(&table_null, at, real, None, cx));
            }));
        }
        if editable || (here && rows.is_deleted(at)) {
            let (file, strong) = (file.clone(), strong.clone());
            let label = if rows.is_deleted(at) { "Keep this row" } else { "Delete row" };
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                let _ = file.update(cx, |f, cx| f.toggle_delete_row(&strong, at, cx));
            }));
        }
    }
    menu
}

fn looks_like_json(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('[')) && pretty_json(text).is_some()
}

fn pretty_json(text: &str) -> Option<String> {
    serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(text).ok()?).ok()
}

/// A statement on one line of the Output tab.
fn one_line(sql: &str) -> String {
    let joined = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > 400 { format!("{}…", joined.chars().take(400).collect::<String>()) } else { joined }
}

/// The handle between the text and the results.
pub(super) fn sql_handle(cx: &App) -> impl IntoElement {
    div()
        .id("sql-split-handle")
        .test_support()
        .flex_none()
        .h(px(5.))
        .w_full()
        .cursor_row_resize()
        .border_t_1()
        .border_color(cx.theme().border)
        .hover(|this| this.bg(cx.theme().drag_border.opacity(0.4)))
        .on_drag(SqlSplitDrag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                set_text_share(TEXT_SHARE);
                window.refresh();
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{ColumnInfo, ColumnSource, Driver, Origin, changes_schema, edit_target, is_sql, login_refused, only_reads, pick, rows_said, title_of, used_database};

    #[test]
    fn what_a_statement_means_for_the_panel() {
        assert!(changes_schema("ALTER TABLE t ADD c INT"));
        assert!(!changes_schema("update t set a = 1"));
        assert_eq!(used_database("USE `shop`").as_deref(), Some("shop"));
        assert_eq!(used_database("use a b"), None);
        assert!(login_refused("MySQL: ERROR 1045 (28000): Access denied for user"));
        assert!(!login_refused("connection refused"));
        assert_eq!((rows_said(1), rows_said(2)), ("1 row".to_string(), "2 rows".to_string()));
    }

    use crate::db::{schema::Schema, value::Kind};

    #[test]
    fn pages_keep_their_order_and_a_sort_goes_to_the_server() {
        let origin = |base: &str, sort, tie_break: Vec<usize>, offset| Origin { base: base.into(), sort, tie_break, offset, ..Default::default() };
        assert_eq!(origin("select * from a join b", None, vec![0], 10000).statement(), "select * from a join b ORDER BY 1 LIMIT 10000 OFFSET 10000", "no subquery: a join can have two columns of a name");
        assert_eq!(origin("select * from t order by x", None, vec![0], 10000).statement(), "select * from t order by x LIMIT 10000 OFFSET 10000", "its own order stays");
        assert_eq!(origin("select * from t", Some((2, true)), vec![0], 0).statement(), "select * from t ORDER BY 3 DESC, 1");
        assert_eq!(origin("select * from t order by x", Some((1, false)), vec![], 0).statement(), "SELECT * FROM (select * from t order by x) AS ideae_q ORDER BY 2");
        assert!(origin("select * from t", None, vec![], 10000).pages_may_shift());
        assert!(!origin("select * from t order by x", None, vec![], 10000).pages_may_shift());
    }

    fn column(name: &str, table: Option<(&str, &str)>) -> ColumnInfo {
        ColumnInfo { name: name.into(), kind: Kind::Text, source: table.map(|(table, column)| ColumnSource { database: Some("shop".into()), table: table.into(), column: column.into() }) }
    }

    #[test]
    fn a_cell_is_changed_only_by_what_the_server_says_it_reads() {
        let row = |t: &str, c: &str, key: &str| (t.to_string(), "BASE TABLE".to_string(), c.to_string(), "int".to_string(), "NO".to_string(), key.to_string());
        let schema = Schema::from_rows("shop".into(), [row("orders", "id", "PRI"), row("orders", "user_id", ""), row("orders", "status", "")]);
        // `SELECT user_id AS id, status`: the alias is not the key.
        assert_eq!(edit_target(&schema, &[column("id", Some(("orders", "user_id"))), column("status", Some(("orders", "status")))], 1), None);
        // `SELECT id, status AS note`: it is `status` that changes.
        let target = edit_target(&schema, &[column("id", Some(("orders", "id"))), column("note", Some(("orders", "status")))], 1).unwrap();
        assert_eq!((target.column.as_str(), target.key), ("status", vec![("id".to_string(), 0)]));
        // An expression, and a table of another database.
        assert_eq!(edit_target(&schema, &[column("id", Some(("orders", "id"))), column("x", None)], 1), None);
        let other = ColumnInfo { name: "status".into(), kind: Kind::Text, source: Some(ColumnSource { database: Some("other".into()), table: "orders".into(), column: "status".into() }) };
        assert_eq!(edit_target(&schema, &[column("id", Some(("orders", "id"))), other], 1), None);
    }

    #[test]
    fn a_filter_or_a_page_wraps_the_statement_whole() {
        let origin = |filter: &str, offset| Origin { base: "select * from t limit 5;".into(), filter: filter.into(), offset, ..Default::default() };
        assert_eq!(origin("", 0).statement(), "select * from t limit 5");
        assert_eq!(origin("a = 1", 0).statement(), "SELECT * FROM (select * from t limit 5) AS ideae_q WHERE a = 1");
        assert!(origin("", 10000).statement().ends_with("LIMIT 10000 OFFSET 10000"));
    }

    #[test]
    fn only_what_only_reads_is_run_again_for_an_export() {
        assert!(only_reads("SELECT * FROM t"));
        assert!(only_reads("show tables"));
        assert!(!only_reads("UPDATE t SET a = 1"));
        assert!(!only_reads("select * from t for update"));
        assert!(!only_reads("select * into outfile '/tmp/x' from t"));
        assert!(!only_reads("with x as (select 1) delete from t"));
    }

    #[test]
    fn a_run_takes_the_selection_else_the_statement_at_the_cursor() {
        let text = "select 1;\nselect 2;\nselect 3;";
        let at_cursor = pick(Driver::MySql, text, 12..12, 12);
        assert_eq!(at_cursor.len(), 1);
        assert_eq!(at_cursor[0].1, "select 2");
        assert_eq!(&text[at_cursor[0].0.clone()], "select 2");
        let selected = pick(Driver::MySql, text, 10..text.len(), 0);
        assert_eq!(selected.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), ["select 2", "select 3"]);
        assert_eq!(&text[selected[1].0.clone()], "select 3");
    }

    #[test]
    fn a_result_is_named_after_what_it_reads() {
        assert_eq!(title_of("select *\nfrom `orders` where 1"), "orders");
        assert_eq!(title_of("update users set a = 1"), "update");
    }

    #[test]
    fn only_sql_files_run() {
        assert!(is_sql(std::path::Path::new("a/b.SQL")));
        assert!(!is_sql(std::path::Path::new("a/b.rs")));
    }
}
