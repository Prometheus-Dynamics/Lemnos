//! Easing curves in fixed point: progress and output are `0..=ONE`. No float code, so an FPU-less
//! target runs the same arithmetic as a desktop.

/// Progress or eased value 1.0.
pub const ONE: u32 = 1 << 16;

/// How a transition or an effect moves from start to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Easing {
    Linear,
    /// Slow start (quadratic).
    EaseIn,
    /// Slow end (quadratic).
    EaseOut,
    /// Slow start and end (cubic smoothstep).
    #[default]
    EaseInOut,
    /// Half a cosine: `(1 - cos(pi t)) / 2`.
    Sine,
}

impl Easing {
    /// Every curve, in wire order (see [`Easing::index`]).
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::EaseIn,
        Self::EaseOut,
        Self::EaseInOut,
        Self::Sine,
    ];

    /// The kebab-case name (`ease-in-out`). `ease` is accepted too.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "linear" => Some(Self::Linear),
            "ease-in" => Some(Self::EaseIn),
            "ease-out" => Some(Self::EaseOut),
            "ease-in-out" | "ease" => Some(Self::EaseInOut),
            "sine" => Some(Self::Sine),
            _ => None,
        }
    }

    /// The curve's position in [`Easing::ALL`], as a look's bytes carry it.
    pub const fn index(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::EaseIn => 1,
            Self::EaseOut => 2,
            Self::EaseInOut => 3,
            Self::Sine => 4,
        }
    }

    /// The curve at `index` in [`Easing::ALL`].
    pub fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(usize::from(index)).copied()
    }

    /// The eased value of progress `t` (`0..=ONE`).
    pub fn apply(self, t: u32) -> u32 {
        let t = t.min(ONE);
        match self {
            Self::Linear => t,
            Self::EaseIn => mul(t, t),
            Self::EaseOut => ONE - mul(ONE - t, ONE - t),
            Self::EaseInOut => {
                // 3t^2 - 2t^3
                let t2 = mul(t, t);
                let t3 = mul(t2, t);
                (3 * t2).saturating_sub(2 * t3).min(ONE)
            }
            Self::Sine => {
                // (1 - cos(pi t)) / 2 = sin^2(pi t / 2)
                let s = quarter_sin(t);
                mul(s, s)
            }
        }
    }
}

fn mul(a: u32, b: u32) -> u32 {
    ((u64::from(a) * u64::from(b)) >> 16) as u32
}

/// `sin(pi t / 2)` for `t` in `0..=ONE`, interpolated from a quarter-wave table.
pub(crate) fn quarter_sin(t: u32) -> u32 {
    // sin(k * pi / 32) * 65536 for k = 0..=16.
    const TABLE: [u32; 17] = [
        0, 6424, 12785, 19024, 25080, 30893, 36410, 41576, 46341, 50660, 54491, 57798, 60547,
        62714, 64277, 65220, 65536,
    ];
    let scaled = t.min(ONE) * 16;
    let index = (scaled >> 16) as usize;
    if index >= 16 {
        return ONE;
    }
    let frac = scaled & 0xffff;
    let (a, b) = (TABLE[index], TABLE[index + 1]);
    a + (((b - a) as u64 * u64::from(frac)) >> 16) as u32
}
