//! Easing curves in fixed point: progress and output are `0..=ONE`.

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
    /// Half a cosine: `(1 - cos(πt)) / 2`.
    Sine,
    /// A CSS-style cubic Bézier through (0,0), (x1,y1), (x2,y2), (1,1), with
    /// control points in thousandths (`x` within 0..=1000).
    CubicBezier {
        x1: u16,
        y1: i16,
        x2: u16,
        y2: i16,
    },
}

impl Easing {
    /// The kebab-case name (`ease-in-out`); `cubic-bezier` for Bézier curves.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseIn => "ease-in",
            Self::EaseOut => "ease-out",
            Self::EaseInOut => "ease-in-out",
            Self::Sine => "sine",
            Self::CubicBezier { .. } => "cubic-bezier",
        }
    }

    /// Parses a name, or `cubic-bezier(x1, y1, x2, y2)` with decimal points
    /// (`cubic-bezier(0.25, 0.1, 0.25, 1.0)`).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "linear" => Some(Self::Linear),
            "ease-in" => Some(Self::EaseIn),
            "ease-out" => Some(Self::EaseOut),
            "ease-in-out" | "ease" => Some(Self::EaseInOut),
            "sine" => Some(Self::Sine),
            other => {
                let inner = other.strip_prefix("cubic-bezier(")?.strip_suffix(')')?;
                let mut parts = inner.split(',').map(|p| thousandths(p.trim()));
                let (x1, y1, x2, y2) = (
                    parts.next()??,
                    parts.next()??,
                    parts.next()??,
                    parts.next()??,
                );
                if parts.next().is_some() || !(0..=1000).contains(&x1) || !(0..=1000).contains(&x2)
                {
                    return None;
                }
                Some(Self::CubicBezier {
                    x1: x1 as u16,
                    y1: y1 as i16,
                    x2: x2 as u16,
                    y2: y2 as i16,
                })
            }
        }
    }

    /// The eased value of progress `t` (`0..=ONE`).
    pub fn apply(self, t: u32) -> u32 {
        let t = t.min(ONE);
        match self {
            Self::Linear => t,
            Self::EaseIn => mul(t, t),
            Self::EaseOut => ONE - mul(ONE - t, ONE - t),
            Self::EaseInOut => {
                // 3t² - 2t³
                let t2 = mul(t, t);
                let t3 = mul(t2, t);
                (3 * t2).saturating_sub(2 * t3).min(ONE)
            }
            Self::Sine => sine_ease(t),
            Self::CubicBezier { x1, y1, x2, y2 } => bezier(t, x1, y1, x2, y2),
        }
    }
}

fn mul(a: u32, b: u32) -> u32 {
    ((u64::from(a) * u64::from(b)) >> 16) as u32
}

/// "0.25" or "-0.5" as thousandths.
fn thousandths(text: &str) -> Option<i32> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let whole: i32 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut value = whole.checked_mul(1000)?;
    let mut scale = 100;
    for c in fraction.chars().take(3) {
        value += c.to_digit(10)? as i32 * scale;
        scale /= 10;
    }
    Some(if negative { -value } else { value })
}

/// `(1 - cos(πt)) / 2` from a quarter-wave table of `sin`, interpolated.
fn sine_ease(t: u32) -> u32 {
    // (1 - cos(πt)) / 2 = sin²(πt/2).
    let s = quarter_sin(t);
    mul(s, s)
}

/// `sin(πt/2)` for `t` in `0..=ONE`.
fn quarter_sin(t: u32) -> u32 {
    // sin(k·π/32) × 65536 for k = 0..=16.
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

/// The Bézier's y at the x equal to `t`, by bisection on the curve parameter.
fn bezier(t: u32, x1: u16, y1: i16, x2: u16, y2: i16) -> u32 {
    let to_q16 = |thousandths: i32| (i64::from(thousandths) << 16) / 1000;
    let (cx1, cx2) = (to_q16(i32::from(x1)), to_q16(i32::from(x2)));
    let (cy1, cy2) = (to_q16(i32::from(y1)), to_q16(i32::from(y2)));
    let one = i64::from(ONE);
    // B(s) = 3(1-s)²s·c1 + 3(1-s)s²·c2 + s³, all in Q16.
    let at = |s: i64, c1: i64, c2: i64| {
        let u = one - s;
        let a = (3 * ((((u * u) >> 16) * s) >> 16) * c1) >> 16;
        let b = (3 * ((((u * s) >> 16) * s) >> 16) * c2) >> 16;
        let c = (((s * s) >> 16) * s) >> 16;
        a + b + c
    };
    let target = i64::from(t);
    let (mut lo, mut hi) = (0i64, one);
    for _ in 0..20 {
        let mid = (lo + hi) / 2;
        if at(mid, cx1, cx2) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((lo + hi) / 2, cy1, cy2).clamp(0, one) as u32
}
