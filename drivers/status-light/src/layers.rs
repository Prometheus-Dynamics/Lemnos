//! The layers a light has, and which one it shows.
//!
//! Each layer holds one look at a priority. The highest priority among the layers with a look is
//! shown, and clearing it reveals the next. On equal priorities the higher layer wins.

use crate::wire::Spec;

/// Layers on a light. The contract reports this in its layout.
pub const LAYERS: usize = 4;

/// The layers, each empty or holding a look at a priority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Layers {
    slots: [Option<(u8, Spec)>; LAYERS],
}

impl Layers {
    /// Shows `spec` on `layer` at `priority`, replacing what the layer had.
    pub fn set(&mut self, layer: usize, priority: u8, spec: Spec) {
        self.slots[layer] = Some((priority, spec));
    }

    /// Empties `layer`.
    pub fn clear(&mut self, layer: usize) {
        self.slots[layer] = None;
    }

    /// Empties every layer.
    pub fn clear_all(&mut self) {
        self.slots = [None; LAYERS];
    }

    /// The look shown: the highest priority, then the higher layer. `None` when every layer is
    /// empty.
    pub fn winner(&self) -> Option<Spec> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(layer, slot)| slot.map(|(priority, spec)| ((priority, layer), spec)))
            .max_by_key(|(key, _)| *key)
            .map(|(_, spec)| spec)
    }
}
