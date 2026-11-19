//! The owners of the ring: one slot per layer, and what each slot shows.
//!
//! A layer is a slot. The caller that shows on it owns it, at a priority, and the slot holds one
//! look or one frame. A look sets every LED; a frame sets the LEDs it names. Each LED shows the
//! value of the owner with the highest `(priority, layer)` that set it. Where no owner set an LED,
//! the base look shows there: the highest look, or the board's default when there is none. Nothing
//! is blended.
//!
//! A frame's colours are scaled by the runtime brightness, as the looks are. Its colours are not
//! scaled by the board's look brightness, which is for looks alone.
//!
//! When a caller goes, the slots it owns are emptied and the rest stand. `clear` empties a slot only
//! when its caller owns it.

use alloc::vec;
use alloc::vec::Vec;

use lemnos_core::CallerId;

use crate::look::{Rgbw, scale_level};
use crate::wire::Spec;

/// Layers on a light. The contract reports this in its layout.
pub const LAYERS: usize = 4;

/// What a slot holds: a look, or a frame (whose LEDs are in [`Layers::frames`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    Look(Spec),
    Frame,
}

/// A slot's owner: its priority, what it holds, and the caller that showed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Intent {
    priority: u8,
    content: Content,
    owner: CallerId,
}

/// The highest look on the ring: its priority, its layer and the look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Top {
    pub priority: u8,
    pub layer: usize,
    pub spec: Spec,
}

/// The slots of a ring of `count` LEDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layers {
    slots: [Option<Intent>; LAYERS],
    /// Each layer's frame: `LAYERS` rows of `count` LEDs, row-major. `None` where the frame does not
    /// set the LED. Sized when the light starts, so nothing allocates after that.
    frames: Vec<Option<Rgbw>>,
    count: usize,
}

impl Layers {
    /// Empty slots for a ring of `count` LEDs.
    pub fn new(count: usize) -> Self {
        Self {
            slots: [None; LAYERS],
            frames: vec![None; LAYERS * count],
            count,
        }
    }

    /// Shows `spec` on `layer` at `priority` for `owner`, replacing what the layer had.
    pub fn set_look(&mut self, layer: usize, priority: u8, spec: Spec, owner: CallerId) {
        self.clear_row(layer);
        self.slots[layer] = Some(Intent {
            priority,
            content: Content::Look(spec),
            owner,
        });
    }

    /// Shows a frame on `layer` at `priority` for `owner`, replacing what the layer had. An LED past
    /// the ring is ignored.
    pub fn set_frame(
        &mut self,
        layer: usize,
        priority: u8,
        leds: impl IntoIterator<Item = (usize, Rgbw)>,
        owner: CallerId,
    ) {
        self.clear_row(layer);
        for (led, colour) in leds {
            if led < self.count {
                self.frames[layer * self.count + led] = Some(colour);
            }
        }
        self.slots[layer] = Some(Intent {
            priority,
            content: Content::Frame,
            owner,
        });
    }

    /// Empties `layer` when `owner` owns it. Returns whether it did.
    pub fn clear(&mut self, layer: usize, owner: CallerId) -> bool {
        if self.slots[layer].is_some_and(|intent| intent.owner == owner) {
            self.empty(layer);
            true
        } else {
            false
        }
    }

    /// Empties every layer that `owner` owns. Returns whether any layer changed.
    pub fn clear_all(&mut self, owner: CallerId) -> bool {
        self.drop_owner(owner)
    }

    /// Empties every layer whose owner is `owner`. Returns whether any layer changed.
    pub fn drop_owner(&mut self, owner: CallerId) -> bool {
        let mut changed = false;
        for layer in 0..LAYERS {
            if self.slots[layer].is_some_and(|intent| intent.owner == owner) {
                self.empty(layer);
                changed = true;
            }
        }
        changed
    }

    /// The highest look: by priority, then by layer. `None` when no layer holds a look.
    pub fn top(&self) -> Option<Top> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(layer, slot)| {
                let intent = (*slot)?;
                match intent.content {
                    Content::Look(spec) => Some(Top {
                        priority: intent.priority,
                        layer,
                        spec,
                    }),
                    Content::Frame => None,
                }
            })
            .max_by_key(|top| (top.priority, top.layer))
    }

    /// Composes the ring into `out`. The base is `base`, the look the animator renders, with its
    /// key `(priority, layer)`; `None` for the default look. Each LED takes the highest frame that
    /// sets it, when that frame's key beats the base's. Frame colours are scaled by `brightness`.
    /// Returns whether anything changed.
    pub fn compose(
        &self,
        base: &[Rgbw],
        base_key: Option<(u8, usize)>,
        brightness: u8,
        out: &mut [Rgbw],
    ) -> bool {
        let mut changed = false;
        for (i, value_out) in out.iter_mut().enumerate() {
            let mut key = base_key;
            let mut value = base[i];
            for layer in 0..LAYERS {
                let Some(intent) = self.slots[layer] else {
                    continue;
                };
                if intent.content != Content::Frame {
                    continue;
                }
                let Some(colour) = self.frames[layer * self.count + i] else {
                    continue;
                };
                let beats = Some((intent.priority, layer));
                if beats > key {
                    key = beats;
                    value = scale_rgbw(colour, brightness);
                }
            }
            if *value_out != value {
                *value_out = value;
                changed = true;
            }
        }
        changed
    }

    fn empty(&mut self, layer: usize) {
        self.slots[layer] = None;
        self.clear_row(layer);
    }

    fn clear_row(&mut self, layer: usize) {
        let row = layer * self.count..(layer + 1) * self.count;
        for led in &mut self.frames[row] {
            *led = None;
        }
    }
}

/// `colour` with each channel scaled by the ring's brightness (255 is all of it).
fn scale_rgbw(colour: Rgbw, brightness: u8) -> Rgbw {
    let s = |c: u8| scale_level(c, brightness);
    Rgbw::new(s(colour.r), s(colour.g), s(colour.b), s(colour.w))
}
