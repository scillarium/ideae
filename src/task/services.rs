use super::*;

// ---------------------------------------------------------------- services

use gpui_kit::component::notification::Notification;

impl TaskView {
    pub(super) fn service_ix(&self, name: &str, _: &App) -> Option<usize> {
        self.tabs.iter().position(|t| matches!(t.kind, TabKind::Terminal(_)) && t.service.as_deref() == Some(name))
    }

    /// The services running now, with their ports, for the task's row.
    pub fn running_services(&self, cx: &App) -> Vec<(String, Option<u16>)> {
        self.services
            .iter()
            .filter(|s| self.service_ix(&s.name, cx).is_some())
            .map(|s| (s.name.clone(), s.port(self.port_slot)))
            .collect()
    }

    /// The service's terminal tab, created when missing; returns its index.
    pub(super) fn service_tab(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        if let Some(ix) = self.service_ix(name, cx) {
            return Some(ix);
        }
        let service = self.services.iter().find(|s| s.name == name)?.clone();
        let slot = self.port_slot;
        let mut env = crate::services::task_env(&self.services, slot);
        env.extend(service.env(&self.services, slot));
        let command = service.expand(&service.command, &self.services, slot);
        let cwd = service.cwd.as_ref().map_or_else(|| self.root.clone(), |c| self.root.join(c));
        self.terminal_seq += 1;
        let seq = self.terminal_seq;
        let tab = Self::make_terminal(
            &self.root,
            self.hook_socket.as_ref(),
            &format!("▶ {name}"),
            seq,
            Some(command),
            env,
            Some((name.to_string(), cwd)),
            None,
            window,
            cx,
        );
        let at = self.tabs.iter().rposition(|t| matches!(t.kind, TabKind::Terminal(_))).map_or(0, |ix| ix + 1);
        self.tabs.insert(at, tab);
        Some(at)
    }

    pub(super) fn start_service(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.reload_services() {
            window.push_notification(Notification::error(e).title("Services"), cx);
            return;
        }
        let port = self.services.iter().find(|s| s.name == name).and_then(|s| s.port(self.port_slot));
        if self.service_ix(name, cx).is_none() {
            if let Some(owner) = port.and_then(crate::services::port_taken) {
                window.push_notification(
                    Notification::warning(format!("Port {} is in use by {owner}.", port.unwrap_or(0))).title(format!("{name} not started")),
                    cx,
                );
                return;
            }
        }
        if let Some(ix) = self.service_tab(name, window, cx) {
            self.activate(ix, window, cx);
        }
        cx.emit(TaskEvent::Changed);
    }

    /// Every service of the task, started behind what is in front. One
    /// whose port something listens on already is left: the agent may have
    /// started it itself. How many were started.
    pub fn start_services(&mut self, window: &mut Window, cx: &mut Context<Self>) -> usize {
        if self.reload_services().is_err() {
            return 0;
        }
        let waiting: Vec<String> = self
            .services
            .iter()
            .filter(|s| self.service_ix(&s.name, cx).is_none() && s.port(self.port_slot).and_then(crate::services::port_taken).is_none())
            .map(|s| s.name.clone())
            .collect();
        let started = waiting.into_iter().filter(|name| self.service_tab(name, window, cx).is_some()).count();
        cx.emit(TaskEvent::Changed);
        cx.notify();
        started
    }

    pub fn stop_services(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (name, _) in self.running_services(cx) {
            self.stop_service(&name, window, cx);
        }
    }

    /// Whether some service runs, and whether some does not yet.
    pub fn services_running_and_waiting(&self, cx: &App) -> (bool, bool) {
        self.services.iter().fold((false, false), |(running, waiting), s| {
            let on = self.service_ix(&s.name, cx).is_some();
            (running || on, waiting || !on)
        })
    }

    pub fn has_services(&self) -> bool {
        !self.services.is_empty()
    }

    /// The task's ports as its terminals have them, for what else runs in it.
    pub fn service_env(&self) -> Vec<(String, String)> {
        crate::services::task_env(&self.services, self.port_slot)
    }

    /// Where the task's site is: the first service that has an address.
    pub fn site(&self) -> Option<String> {
        self.services.iter().find_map(|s| s.url(&self.services, self.port_slot))
    }

    /// The page in front, when it is one of the task's own site: what
    /// follows its address, to show the same page of another variant.
    pub fn shown_page(&self, cx: &App) -> Option<String> {
        let TabKind::Browser(view) = &self.tabs.get(self.focused_ix()?)?.kind else { return None };
        let (url, site) = (view.read(cx).url(), self.site()?);
        url.strip_prefix(origin(&site)).map(str::to_string)
    }

    /// The task's site in a browser tab, at `page` of it when given: the
    /// tab that is on the site already, or a new one.
    pub fn show_site(&mut self, page: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(site) = self.site() else { return };
        let url = match page {
            Some(page) => format!("{}{page}", origin(&site)),
            None => site.clone(),
        };
        let open = self.tabs.iter().position(|t| match &t.kind {
            TabKind::Browser(view) => view.read(cx).url().starts_with(origin(&site)),
            _ => false,
        });
        match open {
            Some(ix) => {
                if let TabKind::Browser(view) = &self.tabs[ix].kind {
                    if view.read(cx).url() != url {
                        view.update(cx, |v, _| v.go(&url));
                    }
                }
                self.activate(ix, window, cx);
            }
            None => self.open_browser(&url, window, cx),
        }
    }

    pub(super) fn stop_service(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.service_ix(name, cx) else { return };
        if let TabKind::Terminal(view) = &self.tabs[ix].kind {
            TermView::end(&view.clone(), cx);
        }
        self.remove_tab(ix, window, cx);
        cx.emit(TaskEvent::Changed);
    }

    pub(super) fn open_service(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.services.iter().find(|s| s.name == name).and_then(|s| s.url(&self.services, self.port_slot));
        if let Some(url) = url {
            self.open_browser(&url, window, cx);
        }
    }

    pub(super) fn reload_services(&mut self) -> Result<(), String> {
        self.services = crate::services::load(&self.root)?;
        Ok(())
    }

    /// Writes a commented `.ideae.toml` to start from, and opens it.
    pub(super) fn create_services_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.root.join(crate::services::FILE);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, crate::services::TEMPLATE) {
                window.push_notification(Notification::error(e.to_string()).title("Services"), cx);
                return;
            }
        }
        self.open_file_as(path, 0, 0, OpenAs::Kept, window, cx);
    }

    /// Every few seconds while the Run panel is in view: the ports the
    /// task's processes listen on.
    pub(super) fn watch_listening(&mut self, cx: &mut Context<Self>) {
        let Some(socket) = self.hook_socket.clone() else { return };
        let root = self.root.display().to_string();
        self._listening_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let (root, socket) = (root.clone(), socket.clone());
                let ports = cx
                    .background_spawn(async move {
                        crate::services::listening(&crate::procs::task_processes(Some(&root), &socket))
                    })
                    .await;
                let shown = this.update(cx, |t, cx| {
                    if t.listening != ports {
                        t.listening = ports;
                        cx.notify();
                    }
                    t.right == RightPane::Run && t.visible
                });
                if !matches!(shown, Ok(true)) {
                    let _ = this.update(cx, |t, _| t._listening_poll = None);
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        }));
    }

    /// Rereads `.ideae.toml` and, without one, looks for what to suggest.
    pub(super) fn refresh_services(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let (resolved, found) = cx.background_spawn(async move { (crate::project_config::resolve(&root), crate::services::detect(&root)) }).await;
            let _ = this.update(cx, |this, cx| {
                match resolved.error {
                    None => {
                        this.services = resolved.config.service;
                        this.auto.configured = resolved.config.task;
                        this.services_error = None;
                    }
                    Some(e) => this.services_error = Some(e),
                }
                let known: HashSet<String> = this.services.iter().map(|s| s.name.clone()).collect();
                this.services_found = found.into_iter().filter(|f| !known.contains(&f.service.name)).collect();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn add_found_service(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(found) = self.services_found.get(ix).cloned() else { return };
        if let Err(e) = crate::services::add(&self.root, &found.service) {
            self.services_error = Some(e);
        }
        self.services_found.remove(ix);
        self.refresh_services(cx);
        cx.notify();
    }

    /// What the project seems to run and `.ideae.toml` lacks, each with Add.
    pub(super) fn render_found(&self, mut panel: Stateful<Div>, heading: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        if self.services_found.is_empty() {
            return panel;
        }
        let muted = cx.theme().muted_foreground;
        panel = panel.child(div().px_3().pt_2().text_xs().text_color(muted).child(heading));
        for (ix, found) in self.services_found.iter().enumerate() {
            let s = &found.service;
            panel = panel.child(
                h_flex()
                    .id(("found", ix))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .hover(|d| d.bg(cx.theme().accent))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(h_flex().gap_2().child(s.name.clone()).when_some(s.port, |row, p| {
                                row.child(div().text_xs().text_color(muted).child(format!(":{p}")))
                            }))
                            .child(div().text_xs().text_color(muted).truncate().child(format!("{} · {}", s.command, found.source))),
                    )
                    .child(
                        Button::new(("add-found", ix))
                            .xsmall()
                            .outline()
                            .label("Add")
                            .tooltip(format!("Write it to {} at the repository root", crate::services::FILE))
                            .on_click(cx.listener(move |this, _, _, cx| this.add_found_service(ix, cx))),
                    ),
            );
        }
        panel
    }

    /// Ports something of the task listens on that no service describes.
    pub(super) fn render_listening(&self, mut panel: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let described: Vec<u16> = self.services.iter().filter_map(|s| s.port(self.port_slot)).collect();
        let ports: Vec<_> = self.listening.iter().filter(|l| !described.contains(&l.port)).cloned().collect();
        if ports.is_empty() {
            return panel;
        }
        let muted = cx.theme().muted_foreground;
        panel = panel.child(div().px_3().pt_2().text_xs().text_color(muted).child("LISTENING"));
        for l in ports {
            let url = format!("http://localhost:{}", l.port);
            panel = panel.child(
                h_flex()
                    .id(("listening", l.port as usize))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .hover(|d| d.bg(cx.theme().accent))
                    .child(div().text_color(gpui_kit::green()).child("●"))
                    .child(div().child(format!(":{}", l.port)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(format!("{} {}", l.name, l.pid)))
                    .child(
                        Button::new(("listening-open", l.port as usize))
                            .xsmall()
                            .ghost()
                            .label("Open")
                            .tooltip(format!("Open {url} in a Web tab"))
                            .on_click(cx.listener(move |this, _, window, cx| this.open_browser(&url, window, cx))),
                    ),
            );
        }
        panel
    }

    pub(super) fn render_run_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut panel = v_flex().id("run-panel").flex_1().min_h_0().overflow_y_scroll().text_sm();
        panel = self.render_auto(panel, cx);
        if let Some(error) = &self.services_error {
            panel = panel.child(div().px_3().py_2().text_xs().text_color(gpui_kit::red()).child(error.clone()));
        }
        if self.services.is_empty() {
            panel = panel.child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .child(div().font_semibold().child("Services"))
                    .child(div().text_xs().text_color(muted).child(
                        "Dev servers and workers this task runs. Each task gets its own ports, \
                         so several branches run side by side; its terminals, the agent's too, \
                         get them as IDEAE_PORT_<NAME>.",
                    )),
            );
            panel = self.render_listening(panel, cx);
            panel = self.render_found(panel, "FOUND IN THE PROJECT", cx);
            return panel
                .child(
                    v_flex()
                        .gap_1()
                        .px_3()
                        .py_3()
                        .child(
                            Button::new("set-up-services")
                                .small()
                                .outline()
                                .label("Set up services…")
                                .tooltip("Project settings: services, and what runs for every new task")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(TaskEvent::Auto(AutoAsk::Settings)))),
                        )
                        .child(
                            Button::new("describe-services")
                                .small()
                                .ghost()
                                .label(format!("Describe by hand in {}…", crate::services::FILE))
                                .on_click(cx.listener(|this, _, window, cx| this.create_services_file(window, cx))),
                        )
                        .child(div().text_xs().text_color(muted).child(format!(
                            "{} goes to the repository root; commit it so every task has the same services.",
                            crate::services::FILE
                        ))),
                )
                .into_any_element();
        }
        let running: Vec<String> = self.running_services(cx).into_iter().map(|(n, _)| n).collect();
        panel = panel.child(div().px_3().pt_2().text_xs().text_color(muted).child("SERVICES"));
        for (ix, service) in self.services.iter().enumerate() {
            let name = service.name.clone();
            let on = running.contains(&name);
            let port = service.port(self.port_slot);
            let command = service.expand(&service.command, &self.services, self.port_slot);
            let action = |id: &'static str, label: &'static str, tip: String, f: fn(&mut TaskView, &str, &mut Window, &mut Context<TaskView>)| {
                let name = name.clone();
                Button::new((id, ix)).xsmall().ghost().label(label).tooltip(tip).on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    f(this, &name, window, cx);
                    cx.notify();
                }))
            };
            panel = panel.child(
                h_flex()
                    .id(("service", ix))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .hover(|d| d.bg(cx.theme().accent))
                    .child(div().text_color(if on { gpui_kit::green() } else { muted }).child(if on { "●" } else { "○" }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(h_flex().gap_2().child(name.clone()).when_some(port, |row, p| {
                                row.child(div().text_xs().text_color(muted).child(format!(":{p}")))
                            }))
                            .child(div().text_xs().text_color(muted).truncate().child(command)),
                    )
                    .when(on, |row| {
                        row.child(action("svc-logs", "Logs", "Its terminal".into(), |t, n, w, cx| {
                            if let Some(ix) = t.service_ix(n, cx) {
                                t.activate(ix, w, cx);
                            }
                        }))
                        .child(action("svc-restart", "↻", "Restart".into(), |t, n, w, cx| {
                            t.stop_service(n, w, cx);
                            t.start_service(n, w, cx);
                        }))
                        .child(action("svc-stop", "■", "Stop".into(), TaskView::stop_service))
                    })
                    .when(!on, |row| row.child(action("svc-start", "▶", "Start".into(), TaskView::start_service)))
                    .when(service.url.is_some(), |row| {
                        let url = service.url(&self.services, self.port_slot).unwrap_or_default();
                        row.child(action("svc-open", "Open", format!("Open {url} in a Web tab"), TaskView::open_service))
                    }),
            );
        }
        panel = self.render_listening(panel, cx);
        panel = self.render_found(panel, "ALSO IN THE PROJECT", cx);
        panel
            .child(
                h_flex().px_3().py_2().child(
                    Button::new("edit-services")
                        .xsmall()
                        .ghost()
                        .label(format!("Edit {}", crate::services::FILE))
                        .on_click(cx.listener(|this, _, window, cx| this.create_services_file(window, cx))),
                ),
            )
            .into_any_element()
    }
}
