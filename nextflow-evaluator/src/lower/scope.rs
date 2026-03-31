//! Variable scopes while lowering: blocks inside functions and closures, and closure captures.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use crate::lowered::{Place, Slot};

/// A declared variable.
struct Declared {
    name: Rc<str>,
    slot: Slot,
    /// Whether the legacy parser keeps the name in a binding rather than as a Groovy local:
    /// a process input, a take, or a name that a process or workflow assigns without `def`.
    binding: bool,
}

/// The variables of one function or closure being lowered.
struct Frame {
    closure: bool,
    /// Block scopes, innermost last.
    blocks: Vec<Vec<Declared>>,
    slot_count: usize,
    /// The enclosing places this closure captures, in the order of `Place::Captured`.
    captures: Vec<Place>,
}

/// The frames being lowered, innermost last.
#[derive(Default)]
pub(super) struct Scopes {
    frames: Vec<Frame>,
    /// The variables of the script, in the order of `Place::Module`.
    module: Vec<Rc<str>>,
}

/// What a finished frame needs at run time.
pub(super) struct FrameLayout {
    pub slot_count: usize,
    pub captures: Vec<Place>,
    /// The names declared in the outermost block, with their slots.
    pub names: Vec<(Rc<str>, Slot)>,
}

/// The kind of an earlier declaration of a name that is declared again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Earlier {
    Local,
    /// A name that the legacy parser keeps in a binding, which a later `def` hides.
    Binding,
}

impl Scopes {
    pub fn push_frame(&mut self, closure: bool) {
        self.frames.push(Frame {
            closure,
            blocks: vec![Vec::new()],
            slot_count: 0,
            captures: Vec::new(),
        });
    }

    pub fn pop_frame(&mut self) -> FrameLayout {
        let frame = self.frames.pop().expect("a frame is open");
        let names = frame.blocks[0]
            .iter()
            .map(|declared| (declared.name.clone(), declared.slot))
            .collect();
        FrameLayout {
            slot_count: frame.slot_count,
            captures: frame.captures,
            names,
        }
    }

    pub fn push_block(&mut self) {
        self.frame().blocks.push(Vec::new());
    }

    pub fn pop_block(&mut self) {
        self.frame().blocks.pop();
    }

    /// Declares a variable in the innermost block. Like Nextflow, a name that any enclosing
    /// block of any enclosing frame declares is an error; the result is then the first
    /// declaration, and the variable is declared anyway, so that later reads find it.
    pub fn declare(&mut self, name: &str, binding: bool) -> (Slot, Option<Earlier>) {
        let earlier = self
            .frames
            .iter()
            .flat_map(|frame| frame.blocks.iter().flatten())
            .find(|declared| &*declared.name == name)
            .map(|declared| {
                if declared.binding {
                    Earlier::Binding
                } else {
                    Earlier::Local
                }
            });
        let frame = self.frame();
        let slot = frame.slot_count;
        frame.slot_count += 1;
        let block = frame.blocks.last_mut().expect("a block is open");
        block.push(Declared {
            name: name.into(),
            slot,
            binding,
        });
        (slot, earlier)
    }

    /// Declares a variable in the outermost block of the current frame, as Nextflow does for a
    /// variable that a process or workflow section assigns without `def`.
    pub fn declare_in_frame(&mut self, name: &str) -> Slot {
        let frame = self.frame();
        let slot = frame.slot_count;
        frame.slot_count += 1;
        frame.blocks[0].push(Declared {
            name: name.into(),
            slot,
            binding: true,
        });
        slot
    }

    /// Whether the code being lowered is inside a closure.
    pub fn in_closure(&self) -> bool {
        self.frames.last().is_some_and(|frame| frame.closure)
    }

    /// Finds a variable, capturing it into every closure between its frame and the current one.
    /// A variable of the script needs no capture.
    pub fn resolve(&mut self, name: &str) -> Option<Place> {
        resolve_in(&mut self.frames, name).or_else(|| {
            let index = self
                .module
                .iter()
                .position(|declared| &**declared == name)?;
            Some(Place::Module(index))
        })
    }

    /// Declares a variable of the script, once per name.
    pub fn declare_module(&mut self, name: &str) {
        if !self.module.iter().any(|declared| &**declared == name) {
            self.module.push(name.into());
        }
    }

    /// The number of variables of the script.
    pub fn module_count(&self) -> usize {
        self.module.len()
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is open")
    }
}

fn resolve_in(frames: &mut [Frame], name: &str) -> Option<Place> {
    let (frame, outer) = frames.split_last_mut()?;
    let local = frame
        .blocks
        .iter()
        .rev()
        .flat_map(|block| block.iter().rev())
        .find(|declared| &*declared.name == name);
    if let Some(declared) = local {
        return Some(Place::Local(declared.slot));
    }
    if !frame.closure {
        return None;
    }
    let place = resolve_in(outer, name)?;
    let index = match frame
        .captures
        .iter()
        .position(|captured| *captured == place)
    {
        Some(index) => index,
        None => {
            frame.captures.push(place);
            frame.captures.len() - 1
        }
    };
    Some(Place::Captured(index))
}
