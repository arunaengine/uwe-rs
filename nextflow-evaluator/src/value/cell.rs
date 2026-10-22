//! Collection cells preserve readonly wrappers and backed list slices.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use super::{List, ListKind, Map, Value};

#[derive(Debug)]
pub struct ListCell {
    value: RefCell<List>,
    parent: Option<Rc<ListCell>>,
    bounds: Option<Cell<(usize, usize)>>,
    pub readonly: bool,
    materialized: Cell<bool>,
}

impl ListCell {
    pub fn new(value: List) -> Self {
        let materialized = !value
            .range
            .as_ref()
            .is_some_and(|range| range.count.is_some());
        Self {
            value: RefCell::new(value),
            parent: None,
            bounds: None,
            readonly: false,
            materialized: Cell::new(materialized),
        }
    }

    pub fn readonly(source: &Rc<Self>, copy: bool) -> Self {
        Self {
            value: RefCell::new(if copy {
                let mut value = source.borrow().clone();
                value.default = None;
                value
            } else {
                List::new(ListKind::ArrayList, Vec::new())
            }),
            parent: (!copy).then(|| source.clone()),
            bounds: None,
            readonly: true,
            materialized: Cell::new(true),
        }
    }

    pub fn slice(source: &Rc<Self>, from: usize, to: usize) -> Self {
        Self {
            value: RefCell::new(List::new(ListKind::ArrayList, Vec::new())),
            parent: Some(source.clone()),
            bounds: Some(Cell::new((from, to))),
            readonly: source.readonly
                || matches!(source.peek().kind, ListKind::Tuple | ListKind::Range),
            materialized: Cell::new(true),
        }
    }

    pub fn borrow(&self) -> Ref<'_, List> {
        if let (Some(parent), None) = (&self.parent, &self.bounds) {
            return parent.borrow();
        }
        if !self.materialized.get() {
            let range = self.value.borrow().range.clone().expect("lazy range");
            self.value.borrow_mut().items = (0..range.count.unwrap_or(0))
                .filter_map(|index| range.item(index))
                .collect();
            self.materialized.set(true);
        }
        self.refresh();
        self.peek()
    }

    pub fn peek(&self) -> Ref<'_, List> {
        match (&self.parent, &self.bounds) {
            (Some(parent), None) => parent.peek(),
            _ => self.value.borrow(),
        }
    }

    pub fn len(&self) -> usize {
        if let Some(parent) = &self.parent {
            return match &self.bounds {
                Some(bounds) => {
                    let (from, to) = bounds.get();
                    to.min(parent.len()).saturating_sub(from)
                }
                None => parent.len(),
            };
        }
        let list = self.peek();
        match &list.range {
            Some(range) if range.count.is_some() => range.count.unwrap_or(0),
            _ => list.items.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn item(&self, index: usize) -> Option<Value> {
        if let Some(parent) = &self.parent {
            return match &self.bounds {
                Some(bounds) => {
                    let (from, to) = bounds.get();
                    from.checked_add(index)
                        .filter(|index| *index < to)
                        .and_then(|index| parent.item(index))
                }
                None => parent.item(index),
            };
        }
        let list = self.peek();
        match &list.range {
            Some(range) if range.count.is_some() => range.item(index),
            _ => list.items.get(index).cloned(),
        }
    }

    pub fn borrow_mut(&self) -> ListMut<'_> {
        match (&self.parent, &self.bounds) {
            (Some(parent), None) => parent.borrow_mut(),
            _ => {
                self.refresh();
                ListMut {
                    value: self.value.borrow_mut(),
                    owner: self,
                }
            }
        }
    }

    pub fn into_inner(self) -> List {
        self.value.into_inner()
    }

    fn refresh(&self) {
        if let (Some(parent), Some(bounds)) = (&self.parent, &self.bounds)
            && let Ok(mut value) = self.value.try_borrow_mut()
        {
            let (from, to) = bounds.get();
            if parent
                .peek()
                .range
                .as_ref()
                .is_some_and(|range| range.count.is_some())
            {
                value.items = (from.min(parent.len())..to.min(parent.len()))
                    .filter_map(|index| parent.item(index))
                    .collect();
            } else {
                let parent = parent.borrow();
                value.items =
                    parent.items[from.min(parent.items.len())..to.min(parent.items.len())].to_vec();
            }
        }
    }
}

pub struct ListMut<'a> {
    value: RefMut<'a, List>,
    owner: &'a ListCell,
}

impl Deref for ListMut<'_> {
    type Target = List;
    fn deref(&self) -> &List {
        &self.value
    }
}

impl DerefMut for ListMut<'_> {
    fn deref_mut(&mut self) -> &mut List {
        &mut self.value
    }
}

impl Drop for ListMut<'_> {
    fn drop(&mut self) {
        if let (Some(parent), Some(bounds)) = (&self.owner.parent, &self.owner.bounds) {
            let mut parent = parent.borrow_mut();
            let (from, to) = bounds.get();
            let from = from.min(parent.items.len());
            let to = to.min(parent.items.len());
            parent
                .items
                .splice(from..to, self.value.items.iter().cloned());
            bounds.set((from, from + self.value.items.len()));
        }
    }
}

#[derive(Debug)]
pub struct MapCell {
    value: RefCell<Map>,
    parent: Option<Rc<MapCell>>,
    pub readonly: bool,
}

impl MapCell {
    pub fn new(value: Map) -> Self {
        Self {
            value: RefCell::new(value),
            parent: None,
            readonly: false,
        }
    }

    pub fn readonly(source: &Rc<Self>, copy: bool) -> Self {
        let source_value = source.borrow();
        let value = Map {
            kind: source_value.kind,
            entries: if copy {
                source_value.entries.clone()
            } else {
                Default::default()
            },
            default: if copy {
                None
            } else {
                source_value.default.clone()
            },
        };
        Self {
            value: RefCell::new(value),
            parent: (!copy).then(|| source.clone()),
            readonly: true,
        }
    }

    pub fn borrow(&self) -> Ref<'_, Map> {
        match &self.parent {
            Some(parent) => parent.borrow(),
            None => self.value.borrow(),
        }
    }

    pub fn borrow_mut(&self) -> RefMut<'_, Map> {
        match &self.parent {
            Some(parent) => parent.borrow_mut(),
            None => self.value.borrow_mut(),
        }
    }

    pub fn into_inner(self) -> Map {
        self.value.into_inner()
    }
}
