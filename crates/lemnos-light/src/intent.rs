//! LED intents from several owners, and which one a light shows.

use crate::animator::{Effect, Look, Transition};
use crate::easing::Easing;
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
}

/// Which kind of intent, from lowest to highest precedence. A light shows
/// the highest layer anyone holds: `Locate` over `System` over `Alert` over
/// `Test` over `Status` over `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// Colours, frames, single LEDs and gauges from applications.
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
    /// A spinner.
    Booting,
    /// A spinner.
    Rebooting,
    /// A red pulse.
    UpdateFailed,
    /// A red pulse.
    RolledBack,
}

impl SystemState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Updating { .. } => "updating",
            Self::Booting => "booting",
            Self::Rebooting => "rebooting",
            Self::UpdateFailed => "update-failed",
            Self::RolledBack => "rolled-back",
        }
    }
}

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
    pub brightness: u8,
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
    /// Spinners (booting, an unknown amount, a chase): one turn and the
    /// tail length in LEDs.
    pub spinner_period_ms: u32,
    pub spinner_tail: u8,
    /// The update fill and its phases' colours (shown dimmed under the
    /// fill, and as the spinner while the amount is unknown).
    pub updating: Rgbw,
    pub verifying: Rgbw,
    pub writing: Rgbw,
    pub staged: Rgbw,
    pub booting: Rgbw,
    pub rebooting: Rgbw,
    /// A failed update or rollback: pulsed.
    pub failed: Rgbw,
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
            brightness: 255,
            ok: Rgbw::rgb(0x00ff00),
            warn: Rgbw::rgb(0xff8000),
            error: Rgbw::rgb(0xff0000),
            busy: Rgbw::rgb(0x0040ff),
            locate: Rgbw::rgb(0x00ffff),
            locate_effect: EffectKind::Breathe,
            idle: Rgbw::OFF,
            progress: Rgbw::rgb(0x00ff40),
            progress_background: Rgbw::OFF,
            spinner_period_ms: 1_200,
            spinner_tail: 5,
            updating: Rgbw::rgb(0x0080ff),
            verifying: Rgbw::rgb(0x8000ff),
            writing: Rgbw::rgb(0x0080ff),
            staged: Rgbw::rgb(0x00ff80),
            booting: Rgbw::rgb(0xffffff),
            rebooting: Rgbw::rgb(0xff8000),
            failed: Rgbw::rgb(0xff0000),
        }
    }
}

/// What an intent shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    System(SystemState),
    /// The board's locate look.
    Locate,
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
            | Show::Indeterminate { .. } => Layer::App,
        }
    }

    /// The look and the fade into it, with `defaults` filling unset fields.
    pub fn resolve(&self, defaults: &Defaults) -> (Look<N>, Transition) {
        let (mut look, default_effect) = match self.show {
            Show::Status(status) => {
                let (color, effect) = match status {
                    Status::Ok => (defaults.ok, defaults.status_effect),
                    Status::Warn => (defaults.warn, defaults.status_effect),
                    Status::Error => (defaults.error, defaults.error_effect),
                    Status::Busy => (defaults.busy, defaults.status_effect),
                    Status::Off => (Rgbw::OFF, EffectKind::Solid),
                };
                (Look::fill(color), effect)
            }
            Show::Color(color) => (Look::fill(color), EffectKind::Solid),
            Show::Frame { pixels, len } => (Look::frame(&pixels[..len.min(N)]), EffectKind::Solid),
            Show::Progress {
                fraction,
                color,
                background,
            } => (
                Look::progress(
                    permille(fraction),
                    color.unwrap_or(defaults.progress),
                    background.unwrap_or(defaults.progress_background),
                ),
                EffectKind::Solid,
            ),
            Show::Indeterminate { color } => (
                self.spinner(color.unwrap_or(defaults.progress), defaults),
                EffectKind::Solid,
            ),
            Show::System(state) => match state {
                SystemState::Updating { progress, phase } => {
                    let phase_color = match phase {
                        Phase::Verifying => defaults.verifying,
                        Phase::Writing => defaults.writing,
                        Phase::Staged => defaults.staged,
                        Phase::Applying => defaults.rebooting,
                    };
                    match progress {
                        Some(progress) => (
                            Look::progress(
                                permille(progress),
                                defaults.updating,
                                phase_color.scaled(64),
                            ),
                            EffectKind::Solid,
                        ),
                        None => (self.spinner(phase_color, defaults), EffectKind::Solid),
                    }
                }
                SystemState::Booting => {
                    (self.spinner(defaults.booting, defaults), EffectKind::Solid)
                }
                SystemState::Rebooting => (
                    self.spinner(defaults.rebooting, defaults),
                    EffectKind::Solid,
                ),
                SystemState::UpdateFailed | SystemState::RolledBack => {
                    (Look::fill(defaults.failed), EffectKind::Breathe)
                }
            },
            Show::Locate if defaults.locate_effect == EffectKind::Chase => {
                (self.spinner(defaults.locate, defaults), EffectKind::Solid)
            }
            Show::Locate => (Look::fill(defaults.locate), defaults.locate_effect),
        };
        let effect = match self.effect.unwrap_or(default_effect) {
            EffectKind::Chase if !look.is_moving() => {
                let color = match look.pixels {
                    crate::animator::Pixels::Fill(color) => color,
                    _ => defaults.locate,
                };
                look = self.spinner(color, defaults);
                EffectKind::Solid
            }
            other => other,
        };
        look.effect = match effect {
            EffectKind::Solid | EffectKind::Chase => Effect::Solid,
            EffectKind::Blink => Effect::Blink {
                period_ms: self.period_ms.unwrap_or(defaults.blink_period_ms),
                duty: self.depth.unwrap_or(defaults.blink_duty),
            },
            EffectKind::Breathe => Effect::Breathe {
                period_ms: self.period_ms.unwrap_or(defaults.breathe_period_ms),
                depth: self.depth.unwrap_or(defaults.breathe_depth),
                easing: self.easing.unwrap_or(defaults.easing),
            },
        };
        look.brightness = self.brightness.unwrap_or(defaults.brightness);
        let transition = Transition::new(
            self.fade_ms.unwrap_or(defaults.fade_ms),
            self.easing.unwrap_or(defaults.easing),
        );
        (look, transition)
    }

    fn spinner(&self, color: Rgbw, defaults: &Defaults) -> Look<N> {
        Look::spinner(
            color,
            Rgbw::OFF,
            self.period_ms.unwrap_or(defaults.spinner_period_ms),
            defaults.spinner_tail,
        )
    }
}

/// Thousandths as `0..=ONE`.
fn permille(value: u16) -> u32 {
    (u32::from(value.min(1000)) << 16) / 1000
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
