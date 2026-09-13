//! The detail pane: an action strip, tables and groups of rows.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::model::{Action, Group, NodeId, Page};

/// What the window does when a page is used.
pub struct Handlers {
    pub action: Rc<dyn Fn(Action)>,
    pub navigate: Rc<dyn Fn(NodeId)>,
}

/// The identities a page was last rendered with, so values can be updated in place.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    actions: Vec<Action>,
    groups: Vec<GroupShape>,
}

/// A group title and its row labels and links.
type GroupShape = (String, Vec<(String, Option<NodeId>)>);

impl Shape {
    fn of(page: &Page) -> Self {
        Self {
            actions: page.actions.clone(),
            groups: page
                .groups
                .iter()
                .map(|group| {
                    (
                        group.title.clone(),
                        group
                            .rows
                            .iter()
                            .map(|row| (row.label.clone(), row.link.clone()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

pub struct Surface {
    pub root: gtk::Box,
    strip: gtk::FlowBox,
    /// Where the window places tables, before the groups.
    pub tables: gtk::Box,
    body: gtk::Box,
    shape: RefCell<Option<Shape>>,
    rows: RefCell<Vec<adw::ActionRow>>,
}

impl Surface {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let strip = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(6)
            .max_children_per_line(12)
            .build();
        root.append(&strip);
        let tables = gtk::Box::new(gtk::Orientation::Vertical, 12);
        tables.set_margin_start(12);
        tables.set_margin_end(12);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.set_margin_start(18);
        body.set_margin_end(18);
        body.set_margin_top(12);
        body.set_margin_bottom(18);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&tables);
        content.append(&body);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        root.append(&scroller);
        Self {
            root,
            strip,
            tables,
            body,
            shape: RefCell::new(None),
            rows: RefCell::new(Vec::new()),
        }
    }

    /// Rebuilds when the page's shape changed, else writes the values into place.
    pub fn render(&self, page: &Page, handlers: &Handlers) {
        let shape = Shape::of(page);
        if self.shape.borrow().as_ref() == Some(&shape) {
            let rows = self.rows.borrow();
            let values = page.groups.iter().flat_map(|group| &group.rows);
            for (widget, row) in rows.iter().zip(values) {
                if widget.subtitle().as_deref() != Some(row.value.as_str()) {
                    widget.set_subtitle(&row.value);
                }
            }
            return;
        }
        self.shape.replace(Some(shape));
        self.rebuild_strip(page, handlers);
        self.rebuild_body(&page.groups, handlers);
    }

    fn rebuild_strip(&self, page: &Page, handlers: &Handlers) {
        while let Some(child) = self.strip.first_child() {
            self.strip.remove(&child);
        }
        for action in &page.actions {
            let button = action_button(*action);
            let (act, handler) = (*action, handlers.action.clone());
            button.connect_clicked(move |_| handler(act));
            self.strip.insert(&button, -1);
        }
        self.strip.set_visible(!page.actions.is_empty());
    }

    fn rebuild_body(&self, groups: &[Group], handlers: &Handlers) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        let mut rows = Vec::new();
        for group in groups {
            let widget = adw::PreferencesGroup::builder().title(&group.title).build();
            for row in &group.rows {
                let built = adw::ActionRow::builder().use_markup(false).build();
                built.set_title(&row.label);
                built.set_subtitle(&row.value);
                built.add_css_class("property");
                match &row.link {
                    Some(target) => {
                        built.set_activatable(true);
                        built.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                        let (target, navigate) = (target.clone(), handlers.navigate.clone());
                        built.connect_activated(move |_| navigate(target.clone()));
                    }
                    None => built.set_subtitle_selectable(true),
                }
                widget.add(&built);
                rows.push(built);
            }
            self.body.append(&widget);
        }
        self.rows.replace(rows);
    }
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

pub fn action_button(action: Action) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(action.icon()));
    content.append(&gtk::Label::new(Some(action.label())));
    let button = gtk::Button::builder().child(&content).build();
    button.update_property(&[gtk::accessible::Property::Label(action.label())]);
    if action.destructive() {
        button.add_css_class("destructive-action");
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Row;

    fn page(actions: Vec<Action>, value: &str) -> Page {
        Page {
            node: NodeId::Host,
            title: "host".to_owned(),
            subtitle: String::new(),
            icon: "computer-symbolic",
            actions,
            groups: vec![Group {
                title: "Host".to_owned(),
                rows: vec![Row::new("Name", value)],
            }],
            tables: Vec::new(),
        }
    }

    #[test]
    fn a_shape_ignores_values_but_not_labels_or_actions() {
        let one = Shape::of(&page(vec![Action::Run], "a"));
        assert_eq!(one, Shape::of(&page(vec![Action::Run], "b")));
        assert_ne!(one, Shape::of(&page(vec![Action::Prune], "a")));
    }
}
