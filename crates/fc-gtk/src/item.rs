//! `Item`: the GObject stored in a pane's list model. It wraps an immutable [`Row`]
//! and carries the mutable UI state as properties so that cell widgets can bind
//! to them and restyle themselves: `marked` for Commander marks, and
//! `computed-size` for folder sizes calculated on demand (−1 = not computed).

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use crate::row::Row;

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::Item)]
    pub struct Item {
        pub row: OnceCell<Row>,
        #[property(get, set)]
        pub marked: Cell<bool>,
        #[property(get, set)]
        pub computed_size: Cell<i64>,
        pub thumbnail: RefCell<Option<gtk::gdk::Texture>>,
    }

    impl Default for Item {
        fn default() -> Self {
            Item {
                row: OnceCell::new(),
                marked: Cell::new(false),
                computed_size: Cell::new(-1),
                thumbnail: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Item {
        const NAME: &'static str = "FcItem";
        type Type = super::Item;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Item {}
}

glib::wrapper! {
    pub struct Item(ObjectSubclass<imp::Item>);
}

impl Item {
    pub fn new(row: Row) -> Self {
        let item: Self = glib::Object::new();
        let _ = item.imp().row.set(row);
        item
    }

    pub fn row(&self) -> &Row {
        self.imp().row.get().expect("Item always holds a Row")
    }

    /// Cached image thumbnail for the thumbnails view.
    pub fn thumbnail(&self) -> Option<gtk::gdk::Texture> {
        self.imp().thumbnail.borrow().clone()
    }

    pub fn set_thumbnail(&self, texture: Option<gtk::gdk::Texture>) {
        *self.imp().thumbnail.borrow_mut() = texture;
    }

    /// Size to display: the computed folder size if known, else the entry's own.
    pub fn effective_size(&self) -> u64 {
        match self.computed_size() {
            n if n >= 0 => n as u64,
            _ => self.row().entry.size,
        }
    }
}

/// The `Row` inside a list-model item.
pub fn row_of(obj: &glib::Object) -> &Row {
    obj.downcast_ref::<Item>()
        .expect("list items are FcItem")
        .row()
}
