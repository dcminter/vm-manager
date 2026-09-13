//! The sidebar's rows, as objects a list model can hold.

use std::cell::RefCell;

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use crate::model::{Node, NodeId};

mod imp {
    #[allow(
        clippy::wildcard_imports,
        reason = "the subclass needs the parent scope"
    )]
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::NodeObject)]
    pub struct NodeObject {
        #[property(get, set)]
        pub key: RefCell<String>,
        #[property(get, set)]
        pub label: RefCell<String>,
        #[property(get, set)]
        pub description: RefCell<String>,
        #[property(get, set)]
        pub icon: RefCell<String>,
        #[property(get, set)]
        pub tone: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NodeObject {
        const NAME: &'static str = "VmgNode";
        type Type = super::NodeObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for NodeObject {}
}

glib::wrapper! {
    pub struct NodeObject(ObjectSubclass<imp::NodeObject>);
}

impl NodeObject {
    pub fn new(node: &Node) -> Self {
        let object: Self = glib::Object::new();
        object.apply(node);
        object
    }

    pub fn id(&self) -> Option<NodeId> {
        NodeId::parse(&self.key())
    }

    /// Sets only what changed, so rows keep their place and focus.
    pub fn apply(&self, node: &Node) {
        let key = node.id.key();
        if self.key() != key {
            self.set_key(key);
        }
        if self.label() != node.label {
            self.set_label(node.label.clone());
        }
        if self.description() != node.description {
            self.set_description(node.description.clone());
        }
        if self.icon() != node.icon {
            self.set_icon(node.icon.to_owned());
        }
        if self.tone() != node.tone.class() {
            self.set_tone(node.tone.class().to_owned());
        }
    }
}

/// Writes nodes into a store, in place where the keys already match.
pub fn fill(store: &gtk::gio::ListStore, nodes: &[Node]) {
    let same = store.n_items() as usize == nodes.len()
        && nodes.iter().enumerate().all(|(index, node)| {
            store
                .item(index as u32)
                .and_downcast::<NodeObject>()
                .is_some_and(|held| held.key() == node.id.key())
        });
    if same {
        for (index, node) in nodes.iter().enumerate() {
            if let Some(held) = store.item(index as u32).and_downcast::<NodeObject>() {
                held.apply(node);
            }
        }
    } else {
        let objects: Vec<NodeObject> = nodes.iter().map(NodeObject::new).collect();
        store.splice(0, store.n_items(), &objects);
    }
}

/// The list item widget for a node, with an expander for its children.
pub fn factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let image = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        row.append(&image);
        row.append(&label);
        let expander = gtk::TreeExpander::builder().child(&row).build();
        item.set_child(Some(&expander));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(expander), Some(tree_row)) = (
            item.child().and_downcast::<gtk::TreeExpander>(),
            item.item().and_downcast::<gtk::TreeListRow>(),
        ) else {
            return;
        };
        expander.set_list_row(Some(&tree_row));
        let Some(object) = tree_row.item().and_downcast::<NodeObject>() else {
            return;
        };
        let Some(row) = expander.child().and_downcast::<gtk::Box>() else {
            return;
        };
        draw(&row, &object);
        for property in ["label", "description", "icon", "tone"] {
            object.connect_notify_local(
                Some(property),
                glib::clone!(
                    #[weak]
                    row,
                    move |object, _| draw(&row, object)
                ),
            );
        }
    });
    factory.connect_unbind(|_, item| {
        if let Some(expander) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::prelude::ListItemExt::child)
            .and_downcast::<gtk::TreeExpander>()
        {
            expander.set_list_row(None);
        }
    });
    factory
}

fn draw(row: &gtk::Box, object: &NodeObject) {
    let mut next = row.first_child();
    while let Some(widget) = next {
        if let Some(image) = widget.downcast_ref::<gtk::Image>() {
            image.set_icon_name(Some(&object.icon()));
            for class in image.css_classes() {
                if class.starts_with("tone-") {
                    image.remove_css_class(&class);
                }
            }
            image.add_css_class(&object.tone());
        } else if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            label.set_text(&object.label());
        }
        next = widget.next_sibling();
    }
    row.set_tooltip_text(Some(&object.description()));
}
