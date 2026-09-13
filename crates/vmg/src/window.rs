//! The window: a sidebar of machines and images, and tabs for what is selected.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use async_channel::{Receiver, Sender};
use gtk::glib;
use gtk::{CompositeTemplate, TemplateChild};
use vm_core::reports::Inspect;

use crate::application::RESOURCE_PATH;
use crate::detail;
use crate::model::{self, Action, CatalogueFilter, NodeId, Page, Snapshot, TableKind};
use crate::prefs::{Prefs, Settings};
use crate::tables::TableView;
use crate::terminals;
use crate::tree::{self, NodeObject};
use crate::worker::{self, Command, Update};

const REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// The stores behind the sidebar tree.
pub struct Stores {
    pub root: gtk::gio::ListStore,
    pub top: gtk::gio::ListStore,
    pub machines: gtk::gio::ListStore,
    pub images: gtk::gio::ListStore,
}

/// The tables on the host page and the toggles above them.
pub struct HostTables {
    pub machines: TableView,
    pub images: TableView,
    pub machines_summary: gtk::Label,
    pub images_summary: gtk::Label,
}

mod imp {
    #[allow(
        clippy::wildcard_imports,
        reason = "the subclass needs the parent scope"
    )]
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/com/paperstack/VmManager/ui/window.ui")]
    pub struct VmgWindow {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub sidebar_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub paned: TemplateChild<gtk::Paned>,
        #[template_child]
        pub sidebar: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub tree_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub tab_bar: TemplateChild<adw::TabBar>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub busy: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub progress_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub progress_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,

        pub snapshot: RefCell<Snapshot>,
        pub settings: RefCell<Settings>,
        pub prefs: OnceCell<Prefs>,
        pub updates: OnceCell<Sender<Update>>,
        pub jobs: RefCell<BTreeMap<u64, String>>,
        pub next_job: Cell<u64>,
        pub refreshing: Cell<bool>,
        pub stores: OnceCell<Stores>,
        pub selection: OnceCell<gtk::SingleSelection>,
        pub tabs: RefCell<HashMap<String, adw::TabPage>>,
        pub surfaces: RefCell<HashMap<String, detail::Surface>>,
        pub terminals: RefCell<HashMap<String, terminals::Tab>>,
        pub host_tables: RefCell<Option<HostTables>>,
        pub inspected: RefCell<HashMap<String, Inspect>>,
        pub inspecting: RefCell<HashSet<String>>,
        pub all_architectures: Cell<bool>,
        pub catalogue_filter: RefCell<CatalogueFilter>,
        pub catalogue_choices: RefCell<Vec<(CatalogueFilter, String)>>,
        pub catalogue_dropdown: RefCell<Option<gtk::DropDown>>,
        pub keep_running: Cell<bool>,
        pub indicator: RefCell<Option<crate::indicator::Handle>>,
        pub expanded: Cell<bool>,
        pub menu: RefCell<Option<gtk::Popover>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VmgWindow {
        const NAME: &'static str = "VmgWindow";
        type Type = super::VmgWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VmgWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for VmgWindow {}

    impl WindowImpl for VmgWindow {
        fn close_request(&self) -> glib::Propagation {
            self.obj().store_layout();
            if self.keep_running.get() {
                self.obj().set_visible(false);
                return glib::Propagation::Stop;
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for VmgWindow {}
    impl AdwApplicationWindowImpl for VmgWindow {}
}

glib::wrapper! {
    pub struct VmgWindow(ObjectSubclass<imp::VmgWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl VmgWindow {
    pub fn new(application: &adw::Application) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    /// A window of no application, for tests.
    #[cfg(test)]
    pub fn detached() -> Self {
        glib::Object::new()
    }

    fn setup(&self) {
        let imp = self.imp();
        #[cfg(test)]
        let prefs = Prefs::in_memory();
        #[cfg(not(test))]
        let prefs = Prefs::new();
        let settings = prefs.load();
        self.set_default_size(settings.window_width, settings.window_height);
        imp.paned.set_position(settings.sidebar_width);
        imp.settings.replace(settings);
        let _ = imp.prefs.set(prefs);
        let _ = RESOURCE_PATH;
        self.setup_tree();
        self.setup_actions();
        self.setup_tabs();
        let visible = imp.settings.borrow().sidebar_visible;
        self.set_sidebar_visible(visible);
    }

    /// Opens the channel, the indicator and the refresh timer.
    pub fn start(&self, want_indicator: bool) {
        let (sender, receiver) = async_channel::unbounded::<Update>();
        let _ = self.imp().updates.set(sender.clone());
        self.consume(receiver);
        if want_indicator {
            let updates = sender;
            let (done, handle) = async_channel::bounded::<Option<crate::indicator::Handle>>(1);
            std::thread::spawn(move || {
                let started = crate::indicator::start(updates.clone());
                let available = started.is_some();
                let _ = done.send_blocking(started);
                let _ = updates.send_blocking(Update::IndicatorAvailable(available));
            });
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = window)]
                self,
                async move {
                    if let Ok(started) = handle.recv().await {
                        window.imp().indicator.replace(started);
                    }
                }
            ));
        }
        self.refresh();
        glib::timeout_add_local(
            REFRESH,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    window.refresh();
                    glib::ControlFlow::Continue
                }
            ),
        );
    }

    fn consume(&self, updates: Receiver<Update>) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                while let Ok(update) = updates.recv().await {
                    window.apply(update);
                }
            }
        ));
    }

    fn apply(&self, update: Update) {
        match update {
            Update::Snapshot(snapshot) => self.apply_snapshot(*snapshot),
            Update::Progress {
                job,
                label,
                fraction,
            } => self.apply_progress(job, &label, fraction),
            Update::Finished {
                job,
                command,
                outcome,
            } => {
                self.end_job(job);
                self.apply_finished(&command, &outcome);
            }
            Update::Failed {
                job,
                command,
                error,
                ..
            } => {
                self.end_job(job);
                if matches!(*command, Command::Refresh) {
                    self.imp().refreshing.set(false);
                }
                self.toast(&format!("{}: {error}", command.subject()), true);
            }
            Update::IndicatorAvailable(available) => self.imp().keep_running.set(available),
            Update::ShowMachine(name) => {
                self.present();
                self.navigate(&NodeId::Machine(name));
            }
            Update::OpenRequested => self.present(),
            Update::QuitRequested => {
                if let Some(application) = self.application() {
                    application.quit();
                }
            }
        }
    }

    /// Asks for a listing unless one is already on its way.
    pub fn refresh(&self) {
        let imp = self.imp();
        if imp.refreshing.replace(true) {
            return;
        }
        if let Some(updates) = imp.updates.get() {
            worker::submit(0, Command::Refresh, updates.clone());
        }
    }

    /// Runs a command, tracking it while it runs.
    pub fn send(&self, command: Command) {
        let imp = self.imp();
        let job = imp.next_job.get() + 1;
        imp.next_job.set(job);
        imp.jobs.borrow_mut().insert(job, command.subject());
        self.show_jobs();
        if let Some(updates) = imp.updates.get() {
            worker::submit(job, command, updates.clone());
        }
    }

    fn end_job(&self, job: u64) {
        self.imp().jobs.borrow_mut().remove(&job);
        self.show_jobs();
    }

    fn show_jobs(&self) {
        let imp = self.imp();
        let jobs = imp.jobs.borrow();
        imp.busy.set_spinning(!jobs.is_empty());
        if let Some((_, label)) = jobs.iter().next_back() {
            imp.progress_label.set_text(label);
        }
        imp.progress_revealer.set_reveal_child(!jobs.is_empty());
        if jobs.is_empty() {
            imp.progress_bar.set_fraction(0.0);
        }
    }

    fn apply_progress(&self, job: u64, label: &str, fraction: Option<f64>) {
        let imp = self.imp();
        let subject = imp.jobs.borrow().get(&job).cloned().unwrap_or_default();
        imp.progress_label.set_text(&format!("{subject}: {label}"));
        match fraction {
            Some(fraction) => imp.progress_bar.set_fraction(fraction.clamp(0.0, 1.0)),
            None => imp.progress_bar.pulse(),
        }
    }

    pub fn toast(&self, message: &str, failed: bool) {
        let toast = adw::Toast::builder()
            .title(message)
            .use_markup(false)
            .timeout(if failed { 8 } else { 4 })
            .build();
        self.imp().toasts.add_toast(toast);
    }

    fn setup_tree(&self) {
        let imp = self.imp();
        let stores = Stores {
            root: gtk::gio::ListStore::new::<NodeObject>(),
            top: gtk::gio::ListStore::new::<NodeObject>(),
            machines: gtk::gio::ListStore::new::<NodeObject>(),
            images: gtk::gio::ListStore::new::<NodeObject>(),
        };
        let (top, machines, images) = (
            stores.top.clone(),
            stores.machines.clone(),
            stores.images.clone(),
        );
        let model = gtk::TreeListModel::new(stores.root.clone(), false, false, move |item| {
            let key = item.downcast_ref::<NodeObject>()?.key();
            let store = match key.as_str() {
                "host" => &top,
                "machines" => &machines,
                "images" => &images,
                _ => return None,
            };
            Some(store.clone().upcast())
        });
        let selection = gtk::SingleSelection::builder()
            .model(&model)
            .autoselect(false)
            .build();
        selection.connect_selected_item_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |selection| window.on_selected(selection)
        ));
        imp.tree_view.set_factory(Some(&tree::factory()));
        imp.tree_view.set_model(Some(&selection));
        // A click on the row already selected opens it again, without hover selecting.
        let click = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_PRIMARY)
            .build();
        click.connect_released(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _, _, _| {
                if let Some(selection) = window.imp().selection.get() {
                    window.on_selected(selection);
                }
            }
        ));
        imp.tree_view.add_controller(click);
        let _ = imp.selection.set(selection);
        let _ = imp.stores.set(stores);
    }

    fn setup_tabs(&self) {
        let imp = self.imp();
        imp.tab_bar.set_view(Some(&*imp.tab_view));
        imp.tab_view.connect_close_page(glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |view, page| {
                let host = window
                    .imp()
                    .tabs
                    .borrow()
                    .get(&Self::tab_key(&NodeId::Host))
                    .is_some_and(|held| held == page);
                if !host {
                    window.forget_tab(page);
                }
                view.close_page_finish(page, !host);
                glib::Propagation::Stop
            }
        ));
    }

    fn setup_actions(&self) {
        let sidebar = gtk::gio::SimpleAction::new_stateful(
            "show-sidebar",
            None,
            &self.imp().settings.borrow().sidebar_visible.to_variant(),
        );
        sidebar.connect_change_state(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |action, state| {
                let visible = state.and_then(glib::Variant::get::<bool>).unwrap_or(true);
                action.set_state(&visible.to_variant());
                window.set_sidebar_visible(visible);
            }
        ));
        self.add_action(&sidebar);
        for (name, action) in [
            ("run", Action::Run),
            ("import", Action::Import),
            ("update", Action::Update),
            ("prune", Action::Prune),
            ("configuration", Action::Configure),
        ] {
            let simple = gtk::gio::SimpleAction::new(name, None);
            simple.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.invoke(&NodeId::Host, action)
            ));
            self.add_action(&simple);
        }
        let refresh = gtk::gio::SimpleAction::new("refresh", None);
        refresh.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                window.imp().inspected.borrow_mut().clear();
                window.refresh();
            }
        ));
        self.add_action(&refresh);
    }

    fn set_sidebar_visible(&self, visible: bool) {
        let imp = self.imp();
        imp.sidebar.set_visible(visible);
        imp.sidebar_button.set_active(visible);
        imp.settings.borrow_mut().sidebar_visible = visible;
        if let Some(action) = self.lookup_action("show-sidebar")
            && let Some(action) = action.downcast_ref::<gtk::gio::SimpleAction>()
        {
            action.set_state(&visible.to_variant());
        }
    }

    /// Writes the window's shape to the preferences.
    pub fn store_layout(&self) {
        let imp = self.imp();
        let mut settings = imp.settings.borrow_mut();
        if imp.sidebar.get_visible() {
            settings.sidebar_width = imp.paned.position();
        }
        settings.window_width = self.width().max(600);
        settings.window_height = self.height().max(400);
        if let Some(prefs) = imp.prefs.get() {
            prefs.store(&settings);
        }
    }

    pub fn store_settings(&self) {
        let imp = self.imp();
        if let Some(prefs) = imp.prefs.get() {
            prefs.store(&imp.settings.borrow());
        }
    }

    pub fn apply_snapshot(&self, snapshot: Snapshot) {
        let imp = self.imp();
        imp.refreshing.set(false);
        if let Some(stores) = imp.stores.get() {
            tree::fill(&stores.root, &[model::host_node(&snapshot.host)]);
            tree::fill(
                &stores.top,
                &[
                    model::machines_node(&snapshot),
                    model::images_node(&snapshot),
                ],
            );
            tree::fill(&stores.machines, &model::machine_nodes(&snapshot));
            tree::fill(&stores.images, &model::image_nodes(&snapshot));
        }
        let running = snapshot
            .machines
            .iter()
            .filter(|row| matches!(row.state, vm_core::reports::State::Live(_)))
            .count();
        imp.window_title.set_subtitle(&format!(
            "{running} running of {} machines",
            snapshot.machines.len()
        ));
        if let Some(handle) = imp.indicator.borrow().as_ref() {
            crate::indicator::refresh(handle, crate::indicator::Model::of(&snapshot));
        }
        imp.snapshot.replace(snapshot);
        if !imp.expanded.replace(true) {
            self.expand_all();
            self.open_detail(&NodeId::Host);
        }
        self.offer_catalogues();
        self.render_tables();
        self.render_open_tabs();
    }

    /// Lists the catalogues in the images table's dropdown when the set changes.
    fn offer_catalogues(&self) {
        let imp = self.imp();
        let choices = CatalogueFilter::choices(&imp.snapshot.borrow().catalogues);
        if *imp.catalogue_choices.borrow() == choices {
            return;
        }
        let dropdown = imp.catalogue_dropdown.borrow().clone();
        if let Some(dropdown) = dropdown {
            let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
            let selected = choices
                .iter()
                .position(|(filter, _)| *filter == *imp.catalogue_filter.borrow())
                .unwrap_or(0);
            dropdown.set_model(Some(&gtk::StringList::new(&labels)));
            imp.catalogue_choices.replace(choices);
            dropdown.set_selected(selected as u32);
        } else {
            imp.catalogue_choices.replace(choices);
        }
    }

    fn expand_all(&self) {
        let Some(selection) = self.imp().selection.get() else {
            return;
        };
        let mut position = 0;
        while let Some(row) = selection.item(position).and_downcast::<gtk::TreeListRow>() {
            if row.depth() < 2 {
                row.set_expanded(true);
            }
            position += 1;
        }
    }

    fn on_selected(&self, selection: &gtk::SingleSelection) {
        let Some(node) = selection
            .selected_item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|row| row.item())
            .and_downcast::<NodeObject>()
            .and_then(|object| object.id())
        else {
            return;
        };
        self.open_detail(&node);
    }

    /// Selects a node in the sidebar, which opens it.
    pub fn navigate(&self, node: &NodeId) {
        let Some(selection) = self.imp().selection.get() else {
            return;
        };
        self.expand_all();
        let key = node.key();
        let mut position = 0;
        while let Some(row) = selection.item(position).and_downcast::<gtk::TreeListRow>() {
            if row
                .item()
                .and_downcast::<NodeObject>()
                .is_some_and(|object| object.key() == key)
            {
                if selection.selected() == position {
                    self.open_detail(node);
                } else {
                    selection.set_selected(position);
                }
                return;
            }
            position += 1;
        }
        self.open_detail(node);
    }

    /// The page a node shows now, or nothing when it is gone.
    fn page_for(&self, node: &NodeId) -> Option<Page> {
        let imp = self.imp();
        let snapshot = imp.snapshot.borrow();
        match node {
            NodeId::Host | NodeId::Machines | NodeId::Images => Some(model::host_page(&snapshot)),
            NodeId::Machine(name) => {
                let row = snapshot.machines.iter().find(|row| &row.name == name)?;
                Some(model::machine_page(
                    row,
                    snapshot.records.get(name),
                    vm_core::instance::now(),
                ))
            }
            NodeId::Image { name, tag } => {
                let summary = model::image_summaries(&snapshot)
                    .into_iter()
                    .find(|image| &image.name == name && &image.tag == tag)?;
                let reference = format!("{name}:{tag}");
                // The host's build leads; the others follow in the catalogue's order.
                let mut architectures = summary.architectures.clone();
                architectures.sort_by_key(|arch| *arch != snapshot.host.arch);
                drop(snapshot);
                let mut builds = Vec::new();
                for arch in architectures {
                    let key = format!("{reference}@{arch}");
                    let known = imp.inspected.borrow().get(&key).cloned();
                    match known {
                        Some(inspect) => builds.push(inspect),
                        None => {
                            if imp.inspecting.borrow_mut().insert(key) {
                                self.send(Command::Inspect {
                                    reference: reference.clone(),
                                    arch,
                                });
                            }
                        }
                    }
                }
                Some(model::image_page(&summary, &builds))
            }
        }
    }

    fn tab_key(node: &NodeId) -> String {
        let node = match node {
            NodeId::Machines | NodeId::Images => &NodeId::Host,
            other => other,
        };
        format!("detail:{}", node.key())
    }

    /// Opens or focuses a node's tab.
    pub fn open_detail(&self, node: &NodeId) {
        let imp = self.imp();
        let key = Self::tab_key(node);
        if let Some(page) = imp.tabs.borrow().get(&key) {
            imp.tab_view.set_selected_page(page);
            self.render_detail(node);
            return;
        }
        let Some(page) = self.page_for(node) else {
            return;
        };
        let surface = detail::Surface::new();
        let tab = imp.tab_view.append(&surface.root);
        tab.set_title(&page.title);
        tab.set_icon(Some(&gtk::gio::ThemedIcon::new(page.icon)));
        if matches!(node, NodeId::Host) {
            self.place_tables(&surface);
        }
        imp.tabs.borrow_mut().insert(key.clone(), tab.clone());
        imp.surfaces.borrow_mut().insert(key, surface);
        imp.tab_view.set_selected_page(&tab);
        self.render_detail(node);
    }

    pub fn render_detail(&self, node: &NodeId) {
        let imp = self.imp();
        let key = Self::tab_key(node);
        let Some(page) = self.page_for(node) else {
            let gone = imp.tabs.borrow().get(&key).cloned();
            if let Some(tab) = gone {
                imp.tab_view.close_page(&tab);
            }
            return;
        };
        let handlers = detail::Handlers {
            action: {
                let (window, node) = (self.clone(), page.node.clone());
                Rc::new(move |action| window.invoke(&node, action))
            },
            navigate: {
                let window = self.clone();
                Rc::new(move |target| window.navigate(&target))
            },
        };
        if let Some(tab) = imp.tabs.borrow().get(&key) {
            tab.set_title(&page.title);
            tab.set_tooltip(&page.subtitle);
            tab.set_icon(Some(&gtk::gio::ThemedIcon::new(page.icon)));
        }
        if let Some(surface) = imp.surfaces.borrow().get(&key) {
            surface.render(&page, &handlers);
        }
    }

    fn render_open_tabs(&self) {
        let keys: Vec<String> = self.imp().surfaces.borrow().keys().cloned().collect();
        for key in keys {
            if let Some(node) = key.strip_prefix("detail:").and_then(NodeId::parse) {
                self.render_detail(&node);
            }
        }
    }

    fn forget_tab(&self, page: &adw::TabPage) {
        let imp = self.imp();
        let key = imp
            .tabs
            .borrow()
            .iter()
            .find(|(_, held)| *held == page)
            .map(|(key, _)| key.clone());
        let Some(key) = key else {
            return;
        };
        imp.tabs.borrow_mut().remove(&key);
        imp.surfaces.borrow_mut().remove(&key);
        if let Some(terminal) = imp.terminals.borrow_mut().remove(&key) {
            terminal.close();
        }
    }

    /// Opens a terminal in a tab of its own, closing what it drives when the tab closes.
    pub fn open_terminal(&self, key: &str, title: &str, tab: terminals::Tab) {
        let imp = self.imp();
        if let Some(page) = imp.tabs.borrow().get(key) {
            imp.tab_view.set_selected_page(page);
            return;
        }
        let page = imp.tab_view.append(&tab.root);
        page.set_title(title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new(
            "utilities-terminal-symbolic",
        )));
        imp.tab_view.set_selected_page(&page);
        tab.terminal.grab_focus();
        imp.tabs.borrow_mut().insert(key.to_owned(), page);
        imp.terminals.borrow_mut().insert(key.to_owned(), tab);
    }

    /// Builds the host page's tables once, above its groups.
    fn place_tables(&self, surface: &detail::Surface) {
        let imp = self.imp();
        let settings = imp.settings.borrow().clone();
        let snapshot = imp.snapshot.borrow();
        let machines = model::machines_table(&snapshot, settings.show_stopped_machines, 0);
        let images = model::images_table(
            &snapshot,
            settings.show_remote_images,
            false,
            &CatalogueFilter::All,
        );
        drop(snapshot);
        let machines_view = TableView::new(&machines, &settings, &self.table_handlers());
        let images_view = TableView::new(&images, &settings, &self.table_handlers());
        let (machines_box, machines_summary) = self.table_section(
            "Machines",
            TableKind::Machines,
            &machines_view,
            ("Running", "All"),
            settings.show_stopped_machines,
        );
        let (images_box, images_summary) = self.table_section(
            "Images",
            TableKind::Images,
            &images_view,
            ("Held", "All"),
            settings.show_remote_images,
        );
        surface.tables.append(&machines_box);
        surface.tables.append(&images_box);
        imp.host_tables.replace(Some(HostTables {
            machines: machines_view,
            images: images_view,
            machines_summary,
            images_summary,
        }));
    }

    fn table_handlers(&self) -> crate::tables::Handlers {
        crate::tables::Handlers {
            activate: {
                let window = self.clone();
                Rc::new(move |node| window.navigate(&node))
            },
            secondary: {
                let window = self.clone();
                Rc::new(move |node, x, y, widget| window.show_context_menu(&node, x, y, &widget))
            },
            checked_changed: Rc::new(|| {}),
            width_changed: {
                let window = self.clone();
                Rc::new(move |table, column, width| {
                    let changed = window
                        .imp()
                        .settings
                        .borrow_mut()
                        .set_column_width(table, column, width);
                    if changed {
                        window.store_settings();
                    }
                })
            },
        }
    }

    fn table_section(
        &self,
        title: &str,
        kind: TableKind,
        view: &TableView,
        toggles: (&str, &str),
        show_all: bool,
    ) -> (gtk::Box, gtk::Label) {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading = gtk::Label::builder().label(title).xalign(0.0).build();
        heading.add_css_class("heading");
        header.append(&heading);
        let summary = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        summary.add_css_class("dim-label");
        header.append(&summary);
        let select_all = gtk::CheckButton::new();
        select_all.set_tooltip_text(Some("Select all"));
        select_all.update_property(&[gtk::accessible::Property::Label("Select all rows")]);
        header.append(&select_all);
        let cog = gtk::MenuButton::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Act on the selected rows")
            .build();
        cog.add_css_class("flat");
        cog.update_property(&[gtk::accessible::Property::Label("Bulk actions")]);
        cog.set_create_popup_func(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |cog| window.bulk_menu(cog, kind)
        ));
        header.append(&cog);
        if kind == TableKind::Images {
            let architectures = gtk::CheckButton::with_label("All architectures");
            architectures.connect_toggled(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |button| {
                    window.imp().all_architectures.set(button.is_active());
                    window.render_tables();
                }
            ));
            header.append(&architectures);
            let dropdown = gtk::DropDown::from_strings(&["All catalogues"]);
            dropdown.set_tooltip_text(Some("Which catalogues to list"));
            dropdown.connect_selected_notify(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |dropdown| {
                    let chosen = window
                        .imp()
                        .catalogue_choices
                        .borrow()
                        .get(dropdown.selected() as usize)
                        .map(|(filter, _)| filter.clone());
                    if let Some(filter) = chosen {
                        window.imp().catalogue_filter.replace(filter);
                        window.render_tables();
                    }
                }
            ));
            header.append(&dropdown);
            self.imp().catalogue_dropdown.replace(Some(dropdown));
        }
        let first = gtk::ToggleButton::with_label(toggles.0);
        let second = gtk::ToggleButton::with_label(toggles.1);
        second.set_group(Some(&first));
        if show_all {
            second.set_active(true);
        } else {
            first.set_active(true);
        }
        second.connect_toggled(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |button| window.set_filter(kind, button.is_active())
        ));
        let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        linked.add_css_class("linked");
        linked.append(&first);
        linked.append(&second);
        header.append(&linked);
        section.append(&header);
        let frame = gtk::Frame::builder().child(&view.view).build();
        frame.add_css_class("view");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&frame)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        section.append(&scroller);
        let id = view.id();
        select_all.connect_toggled(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |button| {
                if let Some(tables) = window.imp().host_tables.borrow().as_ref() {
                    let view = if id == "machines" {
                        &tables.machines
                    } else {
                        &tables.images
                    };
                    view.set_all_checked(button.is_active());
                }
            }
        ));
        (section, summary)
    }

    fn set_filter(&self, kind: TableKind, show_all: bool) {
        {
            let mut settings = self.imp().settings.borrow_mut();
            match kind {
                TableKind::Machines => settings.show_stopped_machines = show_all,
                TableKind::Images => settings.show_remote_images = show_all,
            }
        }
        self.store_settings();
        self.render_tables();
    }

    pub fn render_tables(&self) {
        let imp = self.imp();
        let tables = imp.host_tables.borrow();
        let Some(tables) = tables.as_ref() else {
            return;
        };
        let settings = imp.settings.borrow();
        let snapshot = imp.snapshot.borrow();
        let machines = model::machines_table(
            &snapshot,
            settings.show_stopped_machines,
            vm_core::instance::now(),
        );
        let images = model::images_table(
            &snapshot,
            settings.show_remote_images,
            imp.all_architectures.get(),
            &imp.catalogue_filter.borrow(),
        );
        tables.machines.update(&machines);
        tables.images.update(&images);
        tables
            .machines_summary
            .set_text(&format!("{} of {}", machines.rows.len(), machines.total));
        tables
            .images_summary
            .set_text(&format!("{} of {}", images.rows.len(), images.total));
    }

    /// The cog's menu, built when opened so it reflects what is checked.
    fn bulk_menu(&self, cog: &gtk::MenuButton, kind: TableKind) {
        let imp = self.imp();
        let checked = imp.host_tables.borrow().as_ref().map(|tables| match kind {
            TableKind::Machines => tables.machines.checked(),
            TableKind::Images => tables.images.checked(),
        });
        let checked = checked.unwrap_or_default();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if checked.is_empty() {
            let label = gtk::Label::new(Some("Nothing selected"));
            label.add_css_class("dim-label");
            label.set_margin_top(6);
            label.set_margin_bottom(6);
            label.set_margin_start(12);
            label.set_margin_end(12);
            list.append(&label);
        }
        for action in model::bulk_actions(kind) {
            let button =
                gtk::Button::with_label(&format!("{} ({})", action.label(), checked.len()));
            button.add_css_class("flat");
            button.set_sensitive(!checked.is_empty());
            let (window, nodes) = (self.clone(), checked.clone());
            button.connect_clicked(glib::clone!(
                #[weak]
                cog,
                move |_| {
                    cog.popdown();
                    window.invoke_many(&nodes, action);
                }
            ));
            list.append(&button);
        }
        let popover = gtk::Popover::builder().child(&list).build();
        cog.set_popover(Some(&popover));
    }

    /// A menu of a row's actions, at the pointer.
    fn show_context_menu(&self, node: &NodeId, x: f64, y: f64, widget: &gtk::Widget) {
        let Some(page) = self.page_for(node) else {
            return;
        };
        self.close_context_menu();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for action in page.actions {
            let button = gtk::Button::with_label(action.label());
            button.add_css_class("flat");
            let (window, node) = (self.clone(), node.clone());
            button.connect_clicked(move |button| {
                if let Some(popover) = button
                    .ancestor(gtk::Popover::static_type())
                    .and_downcast::<gtk::Popover>()
                {
                    popover.popdown();
                }
                window.invoke(&node, action);
            });
            list.append(&button);
        }
        // Parented to the table, which outlives the recycled cell that was clicked.
        let anchor = widget
            .ancestor(gtk::ColumnView::static_type())
            .unwrap_or_else(|| widget.clone());
        let point = widget
            .compute_point(&anchor, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        let popover = gtk::Popover::builder()
            .child(&list)
            .has_arrow(false)
            .position(gtk::PositionType::Right)
            .build();
        popover.set_parent(&anchor);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        popover.connect_closed(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |popover| {
                window.imp().menu.take();
                let popover = popover.clone();
                glib::idle_add_local_once(move || popover.unparent());
            }
        ));
        // Shown once the press that asked for it is over, or that press would close it.
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            popover,
            move || popover.popup()
        ));
        self.imp().menu.replace(Some(popover));
    }

    fn close_context_menu(&self) {
        if let Some(popover) = self.imp().menu.take() {
            popover.popdown();
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.imp().snapshot.borrow().clone()
    }

    pub fn remember_inspect(&self, inspect: Inspect) {
        let imp = self.imp();
        let key = format!("{}:{}@{}", inspect.name, inspect.tag, inspect.arch);
        imp.inspecting.borrow_mut().remove(&key);
        imp.inspected.borrow_mut().insert(key, inspect);
    }

    pub fn forget_inspections(&self) {
        let imp = self.imp();
        imp.inspected.borrow_mut().clear();
        imp.inspecting.borrow_mut().clear();
    }
}

#[cfg(all(test, feature = "live-gtk"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::model::Tone;
    use std::sync::Mutex;
    use vm_core::reports::{ImageRow, MachineRow, State};

    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn record_logging() {
        glib::log_set_writer_func(|level, fields| {
            let message = fields
                .iter()
                .find(|field| field.key() == "MESSAGE")
                .and_then(|field| field.value_str())
                .unwrap_or_default()
                .to_owned();
            if matches!(level, glib::LogLevel::Critical | glib::LogLevel::Warning) {
                LOGGED.lock().unwrap().push(message);
            }
            glib::LogWriterOutput::Handled
        });
    }

    fn settle() {
        let context = glib::MainContext::default();
        while context.iteration(false) {}
    }

    fn machine(name: &str, state: State) -> MachineRow {
        MachineRow {
            name: name.to_owned(),
            image: "debian:trixie".to_owned(),
            state,
            created: 0,
            ports: Vec::new(),
            pid: None,
            ssh_port: None,
            user: "dave".to_owned(),
            memory: Some(2048),
            memory_used: None,
            disk: None,
            disk_used: None,
        }
    }

    fn image(name: &str, tag: &str, held: bool) -> ImageRow {
        ImageRow {
            name: name.to_owned(),
            tag: tag.to_owned(),
            arch: "amd64".to_owned(),
            description: format!("{name} {tag}"),
            held,
            size: Some(1 << 20),
            catalogue: "project".to_owned(),
        }
    }

    fn snapshot(machines: &[(&str, State)]) -> Snapshot {
        Snapshot {
            host: crate::model::Host {
                name: "box".to_owned(),
                arch: "amd64".to_owned(),
                ..Default::default()
            },
            machines: machines
                .iter()
                .map(|(name, state)| machine(name, state.clone()))
                .collect(),
            records: BTreeMap::new(),
            images: vec![
                image("debian", "trixie", true),
                image("alpine", "3.22", false),
            ],
            catalogues: Vec::new(),
        }
    }

    fn tab_titles(window: &VmgWindow) -> Vec<String> {
        let view = &window.imp().tab_view;
        (0..view.n_pages())
            .map(|index| view.nth_page(index).title().to_string())
            .collect()
    }

    fn store_labels(store: &gtk::gio::ListStore) -> Vec<String> {
        (0..store.n_items())
            .filter_map(|index| store.item(index).and_downcast::<NodeObject>())
            .map(|node| node.label())
            .collect()
    }

    /// One test, since GTK must be driven from the thread that initialised it.
    #[test]
    fn the_window_behaves_as_designed() {
        adw::init().expect("these tests need a display");
        gtk::gio::resources_register_include!("vmg.gresource").expect("resources compile in");
        record_logging();
        let window = VmgWindow::detached();
        settle();

        a_snapshot_fills_the_sidebar_and_opens_the_host(&window);
        a_selection_opens_a_tab_that_a_refresh_keeps(&window);
        a_machine_that_goes_away_takes_its_tab(&window);
        the_tables_follow_their_filters(&window);
        select_all_ticks_every_visible_row(&window);
        a_terminal_tab_is_released_when_closed(&window);
        a_replayed_console_draws_no_answers_from_the_terminal();
        the_sidebar_starts_hidden_and_toggles(&window);

        let logged = LOGGED.lock().unwrap().clone();
        assert!(logged.is_empty(), "GTK complained: {logged:?}");
    }

    fn a_snapshot_fills_the_sidebar_and_opens_the_host(window: &VmgWindow) {
        window.apply_snapshot(snapshot(&[
            ("one", State::Live("running".to_owned())),
            ("two", State::Stopped),
        ]));
        settle();
        let stores = window.imp().stores.get().unwrap();
        assert_eq!(store_labels(&stores.root), ["box"]);
        assert_eq!(store_labels(&stores.top), ["Machines", "Images"]);
        assert_eq!(store_labels(&stores.machines), ["one", "two"]);
        assert_eq!(store_labels(&stores.images), ["debian:trixie"]);
        assert_eq!(tab_titles(window), ["box"]);
        let first = stores
            .machines
            .item(0)
            .and_downcast::<NodeObject>()
            .unwrap();
        assert_eq!(first.tone(), Tone::Good.class());
        assert_eq!(
            window.imp().window_title.subtitle().as_str(),
            "1 running of 2 machines"
        );
    }

    fn a_selection_opens_a_tab_that_a_refresh_keeps(window: &VmgWindow) {
        window.navigate(&NodeId::Machine("two".to_owned()));
        settle();
        assert_eq!(tab_titles(window), ["box", "two"]);
        let stores = window.imp().stores.get().unwrap();
        let before = stores.machines.item(1).unwrap();
        window.apply_snapshot(snapshot(&[
            ("one", State::Live("running".to_owned())),
            ("two", State::Live("paused".to_owned())),
        ]));
        settle();
        assert_eq!(tab_titles(window), ["box", "two"]);
        // The same object was updated in place rather than replaced.
        assert_eq!(stores.machines.item(1).unwrap(), before);
        let page = window.page_for(&NodeId::Machine("two".to_owned())).unwrap();
        assert_eq!(page.subtitle, "debian:trixie, paused");
        assert!(page.actions.contains(&Action::Resume));
    }

    fn a_machine_that_goes_away_takes_its_tab(window: &VmgWindow) {
        window.apply_snapshot(snapshot(&[("one", State::Live("running".to_owned()))]));
        settle();
        assert_eq!(tab_titles(window), ["box"]);
        assert_eq!(window.imp().surfaces.borrow().len(), 1);
    }

    fn the_tables_follow_their_filters(window: &VmgWindow) {
        window.apply_snapshot(snapshot(&[
            ("one", State::Live("running".to_owned())),
            ("two", State::Stopped),
        ]));
        settle();
        let count = |kind: TableKind| {
            let tables = window.imp().host_tables.borrow();
            let tables = tables.as_ref().unwrap();
            match kind {
                TableKind::Machines => tables.machines.count(),
                TableKind::Images => tables.images.count(),
            }
        };
        assert_eq!(count(TableKind::Machines), 2);
        assert_eq!(count(TableKind::Images), 2);
        window.set_filter(TableKind::Machines, false);
        window.set_filter(TableKind::Images, false);
        settle();
        assert_eq!(count(TableKind::Machines), 1);
        assert_eq!(count(TableKind::Images), 1);
        assert!(!window.imp().settings.borrow().show_stopped_machines);
        window.set_filter(TableKind::Machines, true);
        window.set_filter(TableKind::Images, true);
        settle();
        assert_eq!(count(TableKind::Machines), 2);
    }

    fn select_all_ticks_every_visible_row(window: &VmgWindow) {
        // Shown, so the rows have checkboxes bound to them.
        window.present();
        settle();
        let tables = window.imp().host_tables.borrow();
        let tables = tables.as_ref().unwrap();
        tables.machines.set_all_checked(true);
        settle();
        assert_eq!(tables.machines.checked().len(), 2);
        tables.machines.set_all_checked(false);
        settle();
        assert!(tables.machines.checked().is_empty());
    }

    fn a_replayed_console_draws_no_answers_from_the_terminal() {
        use vte::prelude::*;
        let terminal = vte::Terminal::new();
        let holder = gtk::Window::new();
        holder.set_child(Some(&terminal));
        holder.present();
        let answers = Rc::new(RefCell::new(String::new()));
        let heard = answers.clone();
        terminal.connect_commit(move |_, text, _| heard.borrow_mut().push_str(text));
        let wait = || {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while std::time::Instant::now() < until {
                glib::MainContext::default().iteration(false);
            }
        };
        terminals::replay(&terminal, b"localhost:~# \x1b[6n\x1b[32766;32766H\x1b[6n");
        wait();
        assert_eq!(answers.borrow().as_str(), "");
        terminal.feed(b"\x1b[6n");
        wait();
        assert!(answers.borrow().ends_with('R'), "{:?}", answers.borrow());
        holder.close();
    }

    fn a_terminal_tab_is_released_when_closed(window: &VmgWindow) {
        let marker = format!("vmg-test-{}", std::process::id());
        let tab = terminals::spawn(
            "sh",
            &[
                "-c".to_owned(),
                "while :; do sleep 1; done".to_owned(),
                marker.clone(),
            ],
        );
        window.open_terminal("shell:one", "one shell", tab);
        settle();
        let running = || {
            std::process::Command::new("pgrep")
                .args(["-f", &marker])
                .output()
                .is_ok_and(|output| !output.stdout.is_empty())
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !running() && std::time::Instant::now() < deadline {
            settle();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(running(), "the child should have started");
        let page = window
            .imp()
            .tabs
            .borrow()
            .get("shell:one")
            .cloned()
            .unwrap();
        window.imp().tab_view.close_page(&page);
        settle();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!running(), "closing the tab should end the child");
        let tab = terminals::logs("one");
        window.open_terminal("logs:one", "one log", tab);
        settle();
        assert_eq!(tab_titles(window), ["box", "one log"]);
        let page = window.imp().tabs.borrow().get("logs:one").cloned().unwrap();
        window.imp().tab_view.close_page(&page);
        settle();
        assert_eq!(tab_titles(window), ["box"]);
        assert!(window.imp().terminals.borrow().is_empty());
        // The host tab stays whatever asks to close it.
        let host = window
            .imp()
            .tabs
            .borrow()
            .get("detail:host")
            .cloned()
            .unwrap();
        window.imp().tab_view.close_page(&host);
        settle();
        assert_eq!(tab_titles(window), ["box"]);
    }

    fn the_sidebar_starts_hidden_and_toggles(window: &VmgWindow) {
        assert!(!window.imp().sidebar.get_visible());
        window.set_sidebar_visible(true);
        settle();
        assert!(window.imp().sidebar.get_visible());
        assert!(window.imp().settings.borrow().sidebar_visible);
        window.set_sidebar_visible(false);
        settle();
        assert!(!window.imp().sidebar.get_visible());
        assert!(!window.imp().settings.borrow().sidebar_visible);
    }
}
