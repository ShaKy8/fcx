//! `Item`: the GObject stored in a pane's list model. It wraps an immutable [`Row`]
//! and carries the one piece of mutable UI state, `marked`, as a property so that
//! cell widgets can bind to it and restyle themselves when a mark toggles.

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use crate::row::Row;

mod imp {
    use std::cell::{Cell, OnceCell};

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Item)]
    pub struct Item {
        pub row: OnceCell<Row>,
        #[property(get, set)]
        pub marked: Cell<bool>,
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
}

/// The `Row` inside a list-model item.
pub fn row_of(obj: &glib::Object) -> &Row {
    obj.downcast_ref::<Item>()
        .expect("list items are FcItem")
        .row()
}
