//! LED intents from several owners, and which one a light shows.

use crate::animator::{Effect, Transition};
use crate::builtin;
use crate::easing::Easing;
use crate::look::{Block, LookName, LookSpec, MAX_FRAME_LEDS};
use lemnos_device::Rgbw;

/// Status a light can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Status {
    Ok,
    Warn,
    Error,
    Busy,
    #[default]
    Off,
}

impl Status {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Busy => "busy",
            Self::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            "busy" => Some(Self::Busy),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// The look that shows this status (`status.<name>`).
    pub const fn look_name(self) -> &'static str {
        match self {
            Self::Ok => "status.ok",
            Self::Warn => "status.warn",
            Self::Error => "status.error",
            Self::Busy => "status.busy",
            Self::Off => "status.off",
        }
    }
}

/// Which kind of intent, from lowest to highest precedence. A light shows
/// the highest layer anyone holds: `Locate` over `System` over `Alert` over
/// `Test` over `Status` over `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// Colours, frames, single LEDs, gauges and named looks from applications.
    App,
    /// Status from applications.
    Status,
    /// A selftest or diagnostic: any look, shown over every application's
    /// status so a test's frames are not hidden, but under the host's
    /// alerts, system states and locate. Hosts hold these on a lease, so
    /// a test that disappears falls back to the layers below.
    Test,
    /// Status from the host itself (a faulted sensor, a fan failsafe).
    Alert,
    /// System states: updating, booting, rebooting, a failed update.
    System,
    /// Find this board: shown over everything for a while.
    Locate,
}

/// A step of an update, each with its own colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Checking the image (progress unknown).
    Verifying,
    /// Writing the new slot.
    Writing,
    /// Written; waiting to restart into it.
    Staged,
    /// Restarting into the new version on trial.
    Applying,
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Verifying => "verifying",
            Self::Writing => "writing",
            Self::Staged => "staged",
            Self::Applying => "applying",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "verifying" => Some(Self::Verifying),
            "writing" | "staging" => Some(Self::Writing),
            "staged" => Some(Self::Staged),
            "applying" => Some(Self::Applying),
            _ => None,
        }
    }
}

/// Built-in animations for system states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemState {
    /// A progress fill (`progress` thousandths, `None`: unknown) over the
    /// phase's colour.
    Updating { progress: Option<u16>, phase: Phase },
    /// The start-up look: two comets.
    Booting,
    /// The reboot ember (a static look).
    Rebooting,
    /// A red pulse.
    UpdateFailed,
    /// A red pulse.
    RolledBack,
    /// The new version passed its trial boot: a green ripple, once (the
    /// host holds it for about 2.2 s, then releases the light).
    Confirmed,
}

impl SystemState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Updating { .. } => "updating",
            Self::Booting => "booting",
            Self::Rebooting => "rebooting",
            Self::UpdateFailed => "update-failed",
            Self::RolledBack => "rolled-back",
            Self::Confirmed => "confirmed",
        }
    }

    /// The look that shows this state: `system.verifying`,
    /// `system.writing` (an arc; `system.writing-unknown` while the amount is
    /// unknown), `system.staged`, `system.rebooting`, `system.booting`,
    /// `system.failed`, `system.rolled-back`, `system.confirmed`.
    pub const fn look_name(self) -> &'static str {
        match self {
            Self::Updating {
                phase: Phase::Staged,
                ..
            } => "system.staged",
            Self::Updating {
                progress: Some(_), ..
            } => "system.writing",
            Self::Updating {
                progress: None,
                phase: Phase::Verifying,
            } => "system.verifying",
            Self::Updating {
                progress: None,
                phase: Phase::Writing,
            } => "system.writing-unknown",
            Self::Updating {
                progress: None,
                phase: Phase::Applying,
            } => "system.rebooting",
            Self::Booting => "system.booting",
            Self::Rebooting => "system.rebooting",
            Self::UpdateFailed => "system.failed",
            Self::RolledBack => "system.rolled-back",
            Self::Confirmed => "system.confirmed",
        }
    }
}

/// The brightness of the reboot look: the ring holds it while power is cut,
/// so it is a static ember (12% of full, 31 of 255).
pub const EMBER: u8 = 31;

impl Layer {
    pub const fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Status => "status",
            Self::Test => "test",
            Self::Alert => "alert",
            Self::System => "system",
            Self::Locate => "locate",
        }
    }
}

/// The effect an intent asks for, before defaults fill in its timing.
/// `Chase` is a comet going round (the locate look's alternative to a
/// breathe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Solid,
    Blink,
    Breathe,
    Chase,
}

impl EffectKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Blink => "blink",
            Self::Breathe => "breathe",
            Self::Chase => "chase",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "solid" | "none" => Some(Self::Solid),
            "blink" => Some(Self::Blink),
            "breathe" | "pulse" => Some(Self::Breathe),
            "chase" | "spin" | "spinner" => Some(Self::Chase),
            _ => None,
        }
    }
}

/// How intents look unless they say otherwise; a board definition sets
/// them per light (the Raze: 250 ms ease-in-out fades, breathing status).
/// The built-in looks are made from these, so the board's keys still shape
/// them; a look file overrides a built-in entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defaults {
    pub fade_ms: u32,
    pub easing: Easing,
    /// The effect of `ok`, `busy` (and, unless overridden, `warn`) status.
    pub status_effect: EffectKind,
    /// The effect of `error` status.
    pub error_effect: EffectKind,
    pub breathe_period_ms: u32,
    /// Thousandths.
    pub breathe_depth: u16,
    pub blink_period_ms: u32,
    /// Thousandths of the period the light is on.
    pub blink_duty: u16,
    /// The ring-wide brightness: every look is scaled by it (0..=255).
    pub look_brightness: u8,
    pub ok: Rgbw,
    pub warn: Rgbw,
    pub error: Rgbw,
    pub busy: Rgbw,
    pub locate: Rgbw,
    pub locate_effect: EffectKind,
    /// What the light shows when nobody asks for anything.
    pub idle: Rgbw,
    /// Gauges: the fill and the unfilled part.
    pub progress: Rgbw,
    pub progress_background: Rgbw,
    /// Spinners (a chase, `Indeterminate`, an orbit without its own
    /// timing): one turn and the tail length in LEDs.
    pub spinner_period_ms: u32,
    pub spinner_tail: u8,
    /// The update's progress arc (blue) and its phases' colours: the
    /// verifying and writing (unknown amount) comets, the staged breathe.
    pub updating: Rgbw,
    pub verifying: Rgbw,
    pub verifying_period_ms: u32,
    /// Thousandths of an LED.
    pub verifying_tail: u16,
    /// Thousandths.
    pub verifying_base: u16,
    pub writing: Rgbw,
    pub staged: Rgbw,
    pub staged_period_ms: u32,
    /// Breathe depth, thousandths (the staged look breathes down to 55%).
    pub staged_depth: u16,
    /// The trial boot (and the start-up look): two comets.
    pub booting: Rgbw,
    pub booting_period_ms: u32,
    /// Thousandths of an LED.
    pub booting_tail: u16,
    /// Thousandths.
    pub booting_base: u16,
    /// The reboot ember (see [`EMBER`]).
    pub rebooting: Rgbw,
    /// A failed update or rollback: pulsed.
    pub failed: Rgbw,
    pub failed_period_ms: u32,
    /// Breathe depth, thousandths (the failed pulse breathes down to 10%).
    pub failed_depth: u16,
    /// The ripple after a confirmed trial boot.
    pub confirmed: Rgbw,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            fade_ms: 250,
            easing: Easing::EaseInOut,
            status_effect: EffectKind::Solid,
            error_effect: EffectKind::Blink,
            breathe_period_ms: 2_000,
            breathe_depth: 600,
            blink_period_ms: 1_000,
            blink_duty: 500,
            look_brightness: 128,
            ok: Rgbw::rgb(0x00ff00),
            warn: Rgbw::rgb(0xff8000),
            error: Rgbw::rgb(0xff0000),
            busy: Rgbw::rgb(0x0040ff),
            locate: Rgbw::rgb(0x00ffff),
            locate_effect: EffectKind::Breathe,
            idle: Rgbw::OFF,
            progress: Rgbw::rgb(0x00ff40),
            // A faint neutral white: the track of a gauge.
            progress_background: Rgbw::rgb(0x101012),
            spinner_period_ms: 1_200,
            spinner_tail: 5,
            updating: Rgbw::rgb(0x2f7bff),
            verifying: Rgbw::rgb(0x8a5cff),
            verifying_period_ms: 1_200,
            verifying_tail: 7_000,
            verifying_base: 50,
            writing: Rgbw::rgb(0x2f7bff),
            staged: Rgbw::rgb(0x2bd47d),
            staged_period_ms: 2_200,
            staged_depth: 450,
            booting: Rgbw::rgb(0xfff4e6),
            booting_period_ms: 1_800,
            booting_tail: 5_000,
            booting_base: 40,
            rebooting: Rgbw::rgb(0xff8000),
            failed: Rgbw::rgb(0xff3b3b),
            failed_period_ms: 2_400,
            failed_depth: 900,
            confirmed: Rgbw::rgb(0x2bd47d),
        }
    }
}

/// What an intent shows. Fixed size on purpose (no allocation), so an
/// inline look is as large as its layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Show<const N: usize> {
    Status(Status),
    Color(Rgbw),
    Frame {
        pixels: [Rgbw; N],
        len: usize,
    },
    /// A gauge at `fraction` thousandths; colours default to the board's.
    Progress {
        fraction: u16,
        color: Option<Rgbw>,
        background: Option<Rgbw>,
    },
    /// A spinner: progress of an unknown amount.
    Indeterminate {
        color: Option<Rgbw>,
    },
    /// One or two comets (`heads`, 1 or 2) with their own tail length
    /// (thousandths of an LED), floor brightness (thousandths) and period
    /// (the intent's `period_ms`, else the spinner's).
    Orbit {
        color: Option<Rgbw>,
        tail: Option<u16>,
        heads: u8,
        base: Option<u16>,
    },
    System(SystemState),
    /// The board's locate look.
    Locate,
    /// A named look: a built-in, or one from a look file, as the light's
    /// look table has it when the intent is resolved. `progress` (thousandths)
    /// fills the look's arcs that take an input.
    Look {
        name: LookName,
        progress: Option<u16>,
    },
    /// A look given in full. `progress` as for [`Show::Look`].
    Inline {
        spec: LookSpec,
        progress: Option<u16>,
    },
}

/// One owner's request for a light. Unset fields take the light's
/// [`Defaults`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent<const N: usize> {
    pub show: Show<N>,
    pub effect: Option<EffectKind>,
    pub period_ms: Option<u32>,
    /// Breathe depth or blink duty, thousandths.
    pub depth: Option<u16>,
    pub brightness: Option<u8>,
    pub fade_ms: Option<u32>,
    pub easing: Option<Easing>,
    /// A status from the host itself: shown in the [`Layer::Alert`] layer.
    pub alert: bool,
    /// A selftest or diagnostic look: shown in the [`Layer::Test`] layer
    /// (`alert` wins if both are set).
    pub test: bool,
}

/// Resolves a look name the light's table does not hold itself: the look
/// table (file looks) is asked first, then the built-ins.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<LookSpec>;

/// The look `name` is: from `lookup`, else a built-in made from `defaults`.
pub fn named_look(name: &str, defaults: &Defaults, lookup: Lookup<'_>) -> Option<LookSpec> {
    lookup(name).or_else(|| builtin::builtin(name, defaults))
}

impl<const N: usize> Intent<N> {
    pub const fn new(show: Show<N>) -> Self {
        Self {
            show,
            effect: None,
            period_ms: None,
            depth: None,
            brightness: None,
            fade_ms: None,
            easing: None,
            alert: false,
            test: false,
        }
    }

    /// The layer this intent belongs to.
    pub const fn layer(&self) -> Layer {
        if self.alert {
            return Layer::Alert;
        }
        if self.test {
            return Layer::Test;
        }
        match self.show {
            Show::Locate => Layer::Locate,
            Show::System(_) => Layer::System,
            Show::Status(_) => Layer::Status,
            Show::Color(_)
            | Show::Frame { .. }
            | Show::Progress { .. }
            | Show::Indeterminate { .. }
            | Show::Orbit { .. }
            | Show::Look { .. }
            | Show::Inline { .. } => Layer::App,
        }
    }

    /// The look and the fade into it, with `defaults` filling unset fields.
    /// Named looks are taken from `lookup` first (the look files), then the
    /// built-ins; an unknown name shows nothing.
    pub fn resolve(&self, defaults: &Defaults, lookup: Lookup<'_>) -> (LookSpec, Transition) {
        let (mut spec, progress) = match self.show {
            Show::Status(status) => named(status.look_name(), None, defaults, lookup),
            Show::Color(color) => (
                LookSpec::fill(color).with_brightness(builtin::FULL_FILL),
                None,
            ),
            Show::Frame { pixels, len } => (
                LookSpec::frame(&pixels[..len.min(N).min(MAX_FRAME_LEDS)]),
                None,
            ),
            Show::Progress {
                fraction,
                color,
                background,
            } => (
                LookSpec::progress(
                    fraction.min(1000),
                    color.unwrap_or(defaults.progress),
                    background.unwrap_or(defaults.progress_background),
                ),
                None,
            ),
            Show::Indeterminate { color } => (
                builtin::spinner(color.unwrap_or(defaults.progress), defaults),
                None,
            ),
            Show::Orbit {
                color,
                tail,
                heads,
                base,
            } => (
                builtin::comet(
                    color.unwrap_or(defaults.progress),
                    defaults.spinner_period_ms,
                    tail.unwrap_or(u16::from(defaults.spinner_tail) * 1000),
                    heads.clamp(1, 2),
                    base.unwrap_or(0),
                ),
                None,
            ),
            Show::System(state) => {
                let progress = match state {
                    SystemState::Updating { progress, .. } => progress,
                    _ => None,
                };
                named(state.look_name(), progress, defaults, lookup)
            }
            Show::Locate => named("system.locate", None, defaults, lookup),
            Show::Look { name, progress } => named(name.as_str(), progress, defaults, lookup),
            Show::Inline { spec, progress } => (spec, progress),
        };
        if let Some(progress) = progress {
            spec.fill_input(progress);
        } else {
            spec.fill_input(0);
        }
        if let Some(brightness) = self.brightness {
            spec.brightness = brightness;
        }
        if let Some(kind) = self.effect {
            self.apply_effect(&mut spec, kind, defaults);
        }
        self.apply_timing(&mut spec);
        spec.brightness = scale(spec.brightness, defaults.look_brightness).max(spec.floor);
        let transition = Transition::new(
            self.fade_ms.unwrap_or(defaults.fade_ms),
            self.easing.unwrap_or(defaults.easing),
        );
        (spec, transition)
    }

    /// Resolves with the built-in looks only.
    pub fn resolve_builtin(&self, defaults: &Defaults) -> (LookSpec, Transition) {
        self.resolve(defaults, &|_| None)
    }

    /// An effect the intent asks for replaces the look's envelope; a chase
    /// turns a still fill into a comet.
    fn apply_effect(&self, spec: &mut LookSpec, kind: EffectKind, defaults: &Defaults) {
        // A breathe keeps its own period and depth (a staged or failed look
        // has its own); else the board's.
        let base = match spec.envelope {
            Effect::Breathe {
                period_ms, depth, ..
            } => (period_ms, depth),
            _ => (defaults.breathe_period_ms, defaults.breathe_depth),
        };
        match kind {
            EffectKind::Solid => spec.envelope = Effect::Solid,
            EffectKind::Chase => {
                if !spec.is_moving()
                    && let Some(slot) = spec.layers.first_mut()
                    && let Some(layer) = slot
                    && let Block::Fill { color } = layer.block
                {
                    layer.block = builtin::comet_block(color, defaults);
                }
                spec.envelope = Effect::Solid;
            }
            EffectKind::Blink => {
                spec.envelope = Effect::Blink {
                    period_ms: self.period_ms.unwrap_or(defaults.blink_period_ms),
                    duty: self.depth.unwrap_or(defaults.blink_duty),
                };
            }
            EffectKind::Breathe => {
                spec.envelope = Effect::Breathe {
                    period_ms: self.period_ms.unwrap_or(base.0),
                    depth: self.depth.unwrap_or(base.1),
                    easing: self.easing.unwrap_or(defaults.easing),
                };
            }
        }
    }

    /// The request's timing: the period of comets and of the envelope, the
    /// envelope's depth (or blink duty) and easing.
    fn apply_timing(&self, spec: &mut LookSpec) {
        for slot in spec.layers.iter_mut().flatten() {
            if let Block::Comet { period_ms, .. } = &mut slot.block
                && let Some(period) = self.period_ms
            {
                *period_ms = period;
            }
        }
        match &mut spec.envelope {
            Effect::Solid => {}
            Effect::Blink { period_ms, duty } => {
                if let Some(period) = self.period_ms {
                    *period_ms = period;
                }
                if let Some(depth) = self.depth {
                    *duty = depth;
                }
            }
            Effect::Breathe {
                period_ms,
                depth,
                easing,
            } => {
                if let Some(period) = self.period_ms {
                    *period_ms = period;
                }
                if let Some(d) = self.depth {
                    *depth = d;
                }
                if let Some(e) = self.easing {
                    *easing = e;
                }
            }
        }
    }
}

/// A named look, with its arc's input.
fn named(
    name: &str,
    progress: Option<u16>,
    defaults: &Defaults,
    lookup: Lookup<'_>,
) -> (LookSpec, Option<u16>) {
    let spec = named_look(name, defaults, lookup).unwrap_or(LookSpec::OFF);
    (spec, progress)
}

/// `brightness` scaled by the ring-wide `ring` (255 is all of it).
fn scale(brightness: u8, ring: u8) -> u8 {
    ((u32::from(brightness) * u32::from(ring) + 127) / 255) as u8
}

#[derive(Debug, Clone, Copy)]
struct Held<const N: usize> {
    owner: u32,
    priority: u8,
    seq: u64,
    expires_ms: Option<u64>,
    intent: Intent<N>,
}

/// The intents a light's owners hold, at most `OWNERS` at once, and which one
/// wins: the highest [`Layer`], then the highest priority, then the most
/// recent. Each owner holds one intent per layer.
#[derive(Debug, Clone)]
pub struct Arbiter<const N: usize, const OWNERS: usize> {
    held: [Option<Held<N>>; OWNERS],
    seq: u64,
}

impl<const N: usize, const OWNERS: usize> Default for Arbiter<N, OWNERS> {
    fn default() -> Self {
        Self {
            held: [None; OWNERS],
            seq: 0,
        }
    }
}

/// The winning intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Winner<const N: usize> {
    pub owner: u32,
    pub layer: Layer,
    pub intent: Intent<N>,
}

impl<const N: usize, const OWNERS: usize> Arbiter<N, OWNERS> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds `intent` for `owner` (replacing its intent in the same layer),
    /// until `expires_ms` if set. `false` when every slot is taken.
    pub fn hold(
        &mut self,
        owner: u32,
        priority: u8,
        intent: Intent<N>,
        expires_ms: Option<u64>,
    ) -> bool {
        self.seq += 1;
        let layer = intent.layer();
        let held = Held {
            owner,
            priority,
            seq: self.seq,
            expires_ms,
            intent,
        };
        if let Some(slot) = self
            .held
            .iter_mut()
            .find(|s| s.is_some_and(|h| h.owner == owner && h.intent.layer() == layer))
        {
            *slot = Some(held);
            return true;
        }
        match self.held.iter_mut().find(|s| s.is_none()) {
            Some(slot) => {
                *slot = Some(held);
                true
            }
            None => false,
        }
    }

    /// Drops `owner`'s intent in `layer`, or all its intents (`None`).
    pub fn clear(&mut self, owner: u32, layer: Option<Layer>) {
        for slot in &mut self.held {
            if slot.is_some_and(|h| h.owner == owner && layer.is_none_or(|l| h.intent.layer() == l))
            {
                *slot = None;
            }
        }
    }

    /// Drops intents that expired by `now_ms`; returns whether any did.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let mut any = false;
        for slot in &mut self.held {
            if slot.is_some_and(|h| h.expires_ms.is_some_and(|at| at <= now_ms)) {
                *slot = None;
                any = true;
            }
        }
        any
    }

    /// The earliest expiry, if any intent has one.
    pub fn next_expiry(&self) -> Option<u64> {
        self.held
            .iter()
            .flatten()
            .filter_map(|h| h.expires_ms)
            .min()
    }

    /// The intent the light shows, if anyone holds one.
    pub fn winner(&self) -> Option<Winner<N>> {
        self.held
            .iter()
            .flatten()
            .max_by_key(|h| (h.intent.layer(), h.priority, h.seq))
            .map(|h| Winner {
                owner: h.owner,
                layer: h.intent.layer(),
                intent: h.intent,
            })
    }

    pub fn len(&self) -> usize {
        self.held.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
