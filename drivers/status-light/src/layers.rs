//! The layers a light has, and which one it shows.
//!
//! Each layer holds one look at a priority, and the caller that showed it. The highest priority
//! among the layers with a look is shown, and clearing it reveals the next. On equal priorities the
//! higher layer wins. When a caller goes, the looks it showed are dropped and the rest stand.

use lemnos_core::CallerId;

use crate::wire::Spec;

/// Layers on a light. The contract reports this in its layout.
pub const LAYERS: usize = 4;

/// A look on a layer: its priority, the look, and the caller that showed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent {
    priority: u8,
    spec: Spec,
    owner: CallerId,
}

/// The layers, each empty or holding an intent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Layers {
    slots: [Option<Intent>; LAYERS],
}

impl Layers {
    /// Shows `spec` on `layer` at `priority` for `owner`, replacing what the layer had.
    pub fn set(&mut self, layer: usize, priority: u8, spec: Spec, owner: CallerId) {
        self.slots[layer] = Some(Intent {
            priority,
            spec,
            owner,
        });
    }

    /// Empties `layer`.
    pub fn clear(&mut self, layer: usize) {
        self.slots[layer] = None;
    }

    /// Empties every layer.
    pub fn clear_all(&mut self) {
        self.slots = [None; LAYERS];
    }

    /// Empties every layer whose look `owner` showed. Returns whether any layer changed.
    pub fn drop_owner(&mut self, owner: CallerId) -> bool {
        let mut changed = false;
        for slot in &mut self.slots {
            if slot.is_some_and(|intent| intent.owner == owner) {
                *slot = None;
                changed = true;
            }
        }
        changed
    }

    /// The look shown: the highest priority, then the higher layer. `None` when every layer is
    /// empty.
    pub fn winner(&self) -> Option<Spec> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(layer, slot)| slot.map(|intent| ((intent.priority, layer), intent.spec)))
            .max_by_key(|(key, _)| *key)
            .map(|(_, spec)| spec)
    }
}
