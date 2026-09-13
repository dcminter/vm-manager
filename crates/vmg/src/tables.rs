//! Column views over the model's tables, updated in place.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use crate::model::{NodeId, Table, TableRow, Tone};
use crate::prefs::Settings;

mod imp {
    #[allow(
        clippy::wildcard_imports,
        reason = "the subclass needs the parent scope"
    )]
    use super::*;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct RowObject {
        pub row: RefCell<Option<TableRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowObject {
        const NAME: &'static str = "VmgTableRow";
        type Type = super::RowObject;
    }

    impl ObjectImpl for RowObject {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("updated").build()])
        }
    }
}

glib::wrapper! {
    pub struct RowObject(ObjectSubclass<imp::RowObject>);
}

impl RowObject {
    fn new(row: &TableRow) -> Self {
        let object: Self = glib::Object::new();
        object.imp().row.replace(Some(row.clone()));
        object
    }

    fn key(&self) -> String {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .map(|row| row.node.key())
            .unwrap_or_default()
    }

    fn node(&self) -> Option<NodeId> {
        self.imp().row.borrow().as_ref().map(|row| row.node.clone())
    }

    fn cell(&self, index: usize) -> Option<crate::model::Cell> {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .and_then(|row| row.cells.get(index).cloned())
    }

    fn replace(&self, row: &TableRow) {
        let changed = self.imp().row.borrow().as_ref() != Some(row);
        if changed {
            self.imp().row.replace(Some(row.clone()));
            self.emit_by_name::<()>("updated", &[]);
        }
    }
}

/// Told a table id, a column title and the width dragged to.
pub type WidthChanged = Rc<dyn Fn(&str, &str, i32)>;

/// What the window does when a table is used.
pub struct Handlers {
    pub activate: Rc<dyn Fn(NodeId)>,
    pub secondary: Rc<dyn Fn(NodeId, f64, f64, gtk::Widget)>,
    pub checked_changed: Rc<dyn Fn()>,
    pub width_changed: WidthChanged,
}

pub struct TableView {
    pub view: gtk::ColumnView,
    store: gtk::gio::ListStore,
    selection: gtk::SingleSelection,
    checked: Rc<RefCell<HashSet<String>>>,
    id: &'static str,
}

impl TableView {
    pub fn new(table: &Table, settings: &Settings, handlers: &Handlers) -> Self {
        let store = gtk::gio::ListStore::new::<RowObject>();
        let view = gtk::ColumnView::builder()
            .show_row_separators(true)
            .single_click_activate(false)
            .build();
        view.add_css_class("data-table");
        let sorted = gtk::SortListModel::new(Some(store.clone()), view.sorter());
        let selection = gtk::SingleSelection::builder()
            .model(&sorted)
            .autoselect(false)
            .can_unselect(true)
            .build();
        view.set_model(Some(&selection));
        let checked: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
        view.append_column(&check_column(&checked, handlers.checked_changed.clone()));
        for (index, column) in table.columns.iter().enumerate() {
            let built = data_column(index, column.title, column.numeric, handlers);
            if let Some(width) = settings.column_width(table.id, column.title) {
                built.set_fixed_width(width);
            }
            let (id, title, changed) = (table.id, column.title, handlers.width_changed.clone());
            built.connect_fixed_width_notify(move |column| {
                changed(id, title, column.fixed_width());
            });
            view.append_column(&built);
        }
        let activate = handlers.activate.clone();
        view.connect_activate(glib::clone!(
            #[weak]
            selection,
            move |_, position| {
                if let Some(object) = selection.item(position).and_downcast::<RowObject>()
                    && let Some(node) = object.node()
                {
                    activate(node);
                }
            }
        ));
        let built = Self {
            view,
            store,
            selection,
            checked,
            id: table.id,
        };
        built.update(table);
        built
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Replaces rows in place when the same keys are listed in the same order.
    pub fn update(&self, table: &Table) {
        let same = self.store.n_items() as usize == table.rows.len()
            && table.rows.iter().enumerate().all(|(index, row)| {
                self.store
                    .item(index as u32)
                    .and_downcast::<RowObject>()
                    .is_some_and(|held| held.key() == row.node.key())
            });
        if same {
            for (index, row) in table.rows.iter().enumerate() {
                if let Some(held) = self.store.item(index as u32).and_downcast::<RowObject>() {
                    held.replace(row);
                }
            }
            if let Some(sorter) = self.view.sorter() {
                sorter.changed(gtk::SorterChange::Different);
            }
        } else {
            let selected = self.selected();
            let objects: Vec<RowObject> = table.rows.iter().map(RowObject::new).collect();
            self.store.splice(0, self.store.n_items(), &objects);
            if let Some(node) = selected {
                self.select(&node);
            }
        }
        let keys: HashSet<String> = table.rows.iter().map(|row| row.node.key()).collect();
        self.checked.borrow_mut().retain(|key| keys.contains(key));
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selection
            .selected_item()
            .and_downcast::<RowObject>()
            .and_then(|object| object.node())
    }

    pub fn select(&self, node: &NodeId) {
        let key = node.key();
        for position in 0..self.selection.n_items() {
            if self
                .selection
                .item(position)
                .and_downcast::<RowObject>()
                .is_some_and(|object| object.key() == key)
            {
                self.selection.set_selected(position);
                return;
            }
        }
    }

    pub fn checked(&self) -> Vec<NodeId> {
        let checked = self.checked.borrow();
        (0..self.selection.n_items())
            .filter_map(|position| self.selection.item(position).and_downcast::<RowObject>())
            .filter(|object| checked.contains(&object.key()))
            .filter_map(|object| object.node())
            .collect()
    }

    pub fn count(&self) -> usize {
        self.store.n_items() as usize
    }

    pub fn set_all_checked(&self, on: bool) {
        let mut checked = self.checked.borrow_mut();
        checked.clear();
        if on {
            for position in 0..self.store.n_items() {
                if let Some(object) = self.store.item(position).and_downcast::<RowObject>() {
                    checked.insert(object.key());
                }
            }
        }
        drop(checked);
        self.redraw();
    }

    /// Makes every visible checkbox read the set again.
    fn redraw(&self) {
        for position in 0..self.store.n_items() {
            if let Some(object) = self.store.item(position).and_downcast::<RowObject>() {
                object.emit_by_name::<()>("updated", &[]);
            }
        }
    }
}

/// Signal handlers connected on bind, keyed by the list item, released on unbind.
type Bindings = Rc<RefCell<HashMap<usize, Vec<(glib::Object, glib::SignalHandlerId)>>>>;

fn item_key(item: &gtk::ListItem) -> usize {
    item.as_ptr() as usize
}

fn release(bindings: &Bindings, item: &gtk::ListItem) {
    if let Some(held) = bindings.borrow_mut().remove(&item_key(item)) {
        for (object, handler) in held {
            object.disconnect(handler);
        }
    }
}

fn bound_row(values: &[glib::Value]) -> Option<RowObject> {
    values
        .first()
        .and_then(|value| value.get::<RowObject>().ok())
}

fn check_column(
    checked: &Rc<RefCell<HashSet<String>>>,
    changed: Rc<dyn Fn()>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let bindings: Bindings = Rc::new(RefCell::new(HashMap::new()));
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let check = gtk::CheckButton::new();
        check.update_property(&[gtk::accessible::Property::Label("Select row")]);
        item.set_child(Some(&check));
    });
    let checked_for_bind = checked.clone();
    let bindings_for_bind = bindings.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(check), Some(object)) = (
            item.child().and_downcast::<gtk::CheckButton>(),
            item.item().and_downcast::<RowObject>(),
        ) else {
            return;
        };
        let key = object.key();
        let active = checked_for_bind.borrow().contains(&key);
        check.set_active(active);
        let (checked, changed) = (checked_for_bind.clone(), changed.clone());
        let toggled = check.connect_toggled(move |check| {
            if check.is_active() {
                checked.borrow_mut().insert(key.clone());
            } else {
                checked.borrow_mut().remove(&key);
            }
            changed();
        });
        let checked = checked_for_bind.clone();
        let updated = object.connect_local(
            "updated",
            false,
            glib::clone!(
                #[weak]
                check,
                #[upgrade_or]
                None,
                move |values| {
                    if let Some(object) = bound_row(values) {
                        // Read first, since setting the button toggles the set.
                        let active = checked.borrow().contains(&object.key());
                        check.set_active(active);
                    }
                    None
                }
            ),
        );
        bindings_for_bind.borrow_mut().insert(
            item_key(item),
            vec![(check.upcast(), toggled), (object.upcast(), updated)],
        );
    });
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            release(&bindings, item);
        }
    });
    gtk::ColumnViewColumn::builder()
        .factory(&factory)
        .fixed_width(36)
        .build()
}

fn data_column(
    index: usize,
    title: &'static str,
    numeric: bool,
    handlers: &Handlers,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let bindings: Bindings = Rc::new(RefCell::new(HashMap::new()));
    let secondary = handlers.secondary.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .xalign(if numeric { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let child: gtk::Widget = if index == 0 {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.add_css_class("table-cell");
            row.append(&gtk::Image::new());
            row.append(&label);
            row.upcast()
        } else {
            label.upcast()
        };
        let gesture = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_SECONDARY)
            .build();
        let secondary = secondary.clone();
        gesture.connect_pressed(glib::clone!(
            #[weak]
            item,
            move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let (Some(object), Some(widget)) =
                    (item.item().and_downcast::<RowObject>(), item.child())
                    && let Some(node) = object.node()
                {
                    secondary(node, x, y, widget);
                }
            }
        ));
        child.add_controller(gesture);
        item.set_child(Some(&child));
    });
    let bindings_for_bind = bindings.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(child), Some(object)) = (item.child(), item.item().and_downcast::<RowObject>())
        else {
            return;
        };
        draw_cell(&child, &object, index);
        let handler = object.connect_local(
            "updated",
            false,
            glib::clone!(
                #[weak]
                child,
                #[upgrade_or]
                None,
                move |values| {
                    if let Some(object) = bound_row(values) {
                        draw_cell(&child, &object, index);
                    }
                    None
                }
            ),
        );
        bindings_for_bind
            .borrow_mut()
            .insert(item_key(item), vec![(object.upcast(), handler)]);
    });
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            release(&bindings, item);
        }
    });
    let sorter = gtk::CustomSorter::new(move |left, right| {
        let key = |object: &glib::Object| {
            object
                .downcast_ref::<RowObject>()
                .and_then(|object| object.cell(index))
                .map(|cell| cell.sort)
        };
        key(left).cmp(&key(right)).into()
    });
    gtk::ColumnViewColumn::builder()
        .title(title)
        .factory(&factory)
        .sorter(&sorter)
        .resizable(true)
        .expand(!numeric)
        .build()
}

fn draw_cell(child: &gtk::Widget, object: &RowObject, index: usize) {
    let Some(cell) = object.cell(index) else {
        return;
    };
    if let Some(label) = child.downcast_ref::<gtk::Label>() {
        label.set_text(&cell.text);
        return;
    }
    let Some(row) = child.downcast_ref::<gtk::Box>() else {
        return;
    };
    let mut next = row.first_child();
    while let Some(widget) = next {
        if let Some(image) = widget.downcast_ref::<gtk::Image>() {
            match cell.icon {
                Some((name, tone)) => {
                    image.set_icon_name(Some(name));
                    for held in [
                        Tone::Neutral,
                        Tone::Good,
                        Tone::Warn,
                        Tone::Bad,
                        Tone::Machines,
                        Tone::Images,
                    ] {
                        image.remove_css_class(held.class());
                    }
                    image.add_css_class(tone.class());
                    image.set_visible(true);
                }
                None => image.set_visible(false),
            }
        } else if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            label.set_text(&cell.text);
        }
        next = widget.next_sibling();
    }
}

/// Numbers before text, so a mixed column still orders.
#[cfg(test)]
mod tests {
    use crate::model::Sort;

    #[test]
    fn sort_keys_order_numbers_numerically_and_text_by_case_folding() {
        assert!(Sort::Number(9) < Sort::Number(10));
        assert!(Sort::Number(1) < Sort::Text("a".to_owned()));
        let lower = crate::model::Cell::text("Zed");
        assert_eq!(lower.sort, Sort::Text("zed".to_owned()));
    }
}
