//! Colors. The default is **Slate**: a cool near-black ground, filled tiles,
//! sky for Reeve and the keys, violet for the drafter, and green, amber and
//! red for what's verified, what needs you, and what failed. **Ink** (warm
//! charcoal, the ledger's) and **Brass** (deep navy, the original) remain.
//! Everything degrades to 256, 16, or no color.

use ratatui::style::{Color, Modifier, Style};

/// How many colors the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit.
    TrueColor,
    /// xterm 256.
    Ansi256,
    /// The terminal's own 16.
    Ansi16,
    /// None (`NO_COLOR`).
    Mono,
}

impl ColorMode {
    /// From the `[ui] colors` setting and the environment. `NO_COLOR` wins.
    pub fn detect(setting: &str, env: impl Fn(&str) -> Option<String>) -> Self {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::Mono;
        }
        match setting {
            "truecolor" | "24bit" => return Self::TrueColor,
            "256" => return Self::Ansi256,
            "16" => return Self::Ansi16,
            _ => {}
        }
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        let term = env("TERM").unwrap_or_default();
        if term.contains("256") || term.contains("kitty") || term.contains("alacritty") {
            Self::Ansi256
        } else if term.is_empty() || term == "dumb" {
            Self::Mono
        } else {
            Self::Ansi16
        }
    }
}

/// Every color the UI uses, by role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// How colors are reduced.
    pub mode: ColorMode,
    /// Screen ground.
    pub bg: Color,
    /// Tile ground (lifted off the screen).
    pub panel: Color,
    /// Raised ground: the composer, a selected row, an approval.
    pub input: Color,
    /// Sunken ground: a detail card inside a tile.
    pub inset: Color,
    /// Resting borders.
    pub border: Color,
    /// The focused panel's border.
    pub border_hot: Color,
    /// Body text.
    pub fg: Color,
    /// Secondary text.
    pub dim: Color,
    /// Tertiary text, placeholders, empty bar cells.
    pub faint: Color,
    /// Reeve's own accent.
    pub brass: Color,
    /// Warm highlight.
    pub amber: Color,
    /// Third stop of the header gradient.
    pub copper: Color,
    /// Cool secondary.
    pub teal: Color,
    /// The drafter's color; the top of a chart's gradient.
    pub violet: Color,
    /// The user's speaker color.
    pub user: Color,
    /// Healthy / success.
    pub good: Color,
    /// Attention.
    pub warn: Color,
    /// Failure, T3, YOLO.
    pub bad: Color,
    /// Inline code.
    pub code: Color,
    /// Code block ground.
    pub code_bg: Color,
    /// Ground of an added diff row.
    pub add_bg: Color,
    /// Ground of a removed diff row.
    pub del_bg: Color,
}

impl Theme {
    /// Brass, at full color.
    pub fn brass() -> Self {
        Self {
            mode: ColorMode::TrueColor,
            bg: rgb(0x0e1320),
            panel: rgb(0x121929),
            input: rgb(0x172033),
            inset: rgb(0x0b101b),
            border: rgb(0x263149),
            border_hot: rgb(0xc9a14e),
            fg: rgb(0xdce1ea),
            dim: rgb(0x8290a8),
            faint: rgb(0x46516a),
            brass: rgb(0xd6ab52),
            amber: rgb(0xf2b94b),
            copper: rgb(0xdd7a50),
            teal: rgb(0x4cc3b1),
            violet: rgb(0xb49be0),
            user: rgb(0x8db8ff),
            good: rgb(0x7fd18b),
            warn: rgb(0xf0a04b),
            bad: rgb(0xec5f6b),
            code: rgb(0xe8c78a),
            code_bg: rgb(0x1a2336),
            add_bg: rgb(0x14302b),
            del_bg: rgb(0x3a1c25),
        }
    }

    /// Slate, at full color: the board's palette.
    pub fn slate() -> Self {
        Self {
            mode: ColorMode::TrueColor,
            bg: rgb(0x0b0e12),
            panel: rgb(0x131820),
            input: rgb(0x1a212b),
            inset: rgb(0x0f141a),
            border: rgb(0x222a35),
            border_hot: rgb(0x38bdf8),
            fg: rgb(0xe2e8f0),
            dim: rgb(0x8b98a9),
            faint: rgb(0x566273),
            brass: rgb(0x38bdf8),
            amber: rgb(0xfbbf24),
            copper: rgb(0xfb923c),
            teal: rgb(0x2dd4bf),
            violet: rgb(0xa78bfa),
            user: rgb(0xe2e8f0),
            good: rgb(0x4ade80),
            warn: rgb(0xfbbf24),
            bad: rgb(0xf87171),
            code: rgb(0x7dd3fc),
            code_bg: rgb(0x0f141a),
            add_bg: rgb(0x0f2a1a),
            del_bg: rgb(0x351a1a),
        }
    }

    /// Ink, at full color: the ledger's palette.
    pub fn ink() -> Self {
        Self {
            mode: ColorMode::TrueColor,
            bg: rgb(0x151412),
            panel: rgb(0x1b1916),
            input: rgb(0x221f1a),
            inset: rgb(0x11100e),
            border: rgb(0x2d2a25),
            border_hot: rgb(0x4a3d22),
            fg: rgb(0xe9e4d8),
            dim: rgb(0x8b8578),
            faint: rgb(0x4f4a42),
            brass: rgb(0xd9a441),
            amber: rgb(0xe6b457),
            copper: rgb(0xd98a5c),
            teal: rgb(0x7fc4b8),
            violet: rgb(0xb7a2cf),
            user: rgb(0x8fa7c4),
            good: rgb(0xa3ba7c),
            warn: rgb(0xd9a441),
            bad: rgb(0xe5553b),
            code: rgb(0xe0c89a),
            code_bg: rgb(0x1d1b18),
            add_bg: rgb(0x1f2a1a),
            del_bg: rgb(0x2e1b17),
        }
    }

    /// The terminal's own 16 colors.
    pub fn ansi16() -> Self {
        Self {
            mode: ColorMode::Ansi16,
            bg: Color::Reset,
            panel: Color::Reset,
            input: Color::Reset,
            inset: Color::Reset,
            border: Color::DarkGray,
            border_hot: Color::Yellow,
            fg: Color::Reset,
            dim: Color::Gray,
            faint: Color::DarkGray,
            brass: Color::Yellow,
            amber: Color::LightYellow,
            copper: Color::LightRed,
            teal: Color::Cyan,
            violet: Color::Magenta,
            user: Color::LightBlue,
            good: Color::Green,
            warn: Color::Yellow,
            bad: Color::Red,
            code: Color::Yellow,
            code_bg: Color::Reset,
            add_bg: Color::Reset,
            del_bg: Color::Reset,
        }
    }

    /// By name, reduced for the terminal.
    pub fn named(name: &str, mode: ColorMode) -> Self {
        // Custom themes from ~/.reeve/themes/ arrive with M6's theme import.
        match name {
            "brass" => Self::brass(),
            "ink" => Self::ink(),
            _ => Self::slate(),
        }
        .degrade(mode)
    }

    /// Reduce to what the terminal can show.
    pub fn degrade(self, mode: ColorMode) -> Self {
        match mode {
            ColorMode::TrueColor => self,
            ColorMode::Ansi256 => Self {
                mode,
                ..self.map(quantize_256)
            },
            ColorMode::Ansi16 => Self::ansi16(),
            ColorMode::Mono => Self {
                mode,
                ..self.map(|_| Color::Reset)
            },
        }
    }

    fn map(self, f: impl Fn(Color) -> Color) -> Self {
        Self {
            mode: self.mode,
            bg: f(self.bg),
            panel: f(self.panel),
            input: f(self.input),
            inset: f(self.inset),
            border: f(self.border),
            border_hot: f(self.border_hot),
            fg: f(self.fg),
            dim: f(self.dim),
            faint: f(self.faint),
            brass: f(self.brass),
            amber: f(self.amber),
            copper: f(self.copper),
            teal: f(self.teal),
            violet: f(self.violet),
            user: f(self.user),
            good: f(self.good),
            warn: f(self.warn),
            bad: f(self.bad),
            code: f(self.code),
            code_bg: f(self.code_bg),
            add_bg: f(self.add_bg),
            del_bg: f(self.del_bg),
        }
    }

    /// Blend two theme colors at `t` (0–1). Below truecolor, the nearer end.
    pub fn mix(&self, a: Color, b: Color, t: f32) -> Color {
        match (self.mode, a, b) {
            (ColorMode::TrueColor, Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
                let t = t.clamp(0.0, 1.0);
                let l =
                    |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
                Color::Rgb(l(r1, r2), l(g1, g2), l(b1, b2))
            }
            _ if t < 0.5 => a,
            _ => b,
        }
    }

    /// A color along a multi-stop gradient.
    pub fn gradient(&self, stops: &[Color], t: f32) -> Color {
        match stops.len() {
            0 => self.fg,
            1 => stops[0],
            n => {
                let t = t.clamp(0.0, 1.0) * (n - 1) as f32;
                let i = (t.floor() as usize).min(n - 2);
                self.mix(stops[i], stops[i + 1], t - i as f32)
            }
        }
    }

    /// Level color: teal when calm, amber when busy, red when full.
    pub fn level(&self, ratio: f64) -> Color {
        if ratio >= 0.9 {
            self.bad
        } else if ratio >= 0.75 {
            self.warn
        } else {
            self.teal
        }
    }

    /// A faint wash of `c` over the ground: a pill's or a highlight's
    /// background. Without truecolor, the ground itself.
    pub fn tint(&self, c: Color) -> Color {
        match self.mode {
            ColorMode::TrueColor => self.mix(self.bg, c, 0.16),
            _ => self.bg,
        }
    }

    /// `c` on its own tint: a pill.
    pub fn pill(&self, c: Color) -> Style {
        Style::default().fg(c).bg(self.tint(c))
    }

    /// A key, as the hints show it: accent on its tint, bold.
    pub fn key(&self) -> Style {
        self.pill(self.brass).add_modifier(Modifier::BOLD)
    }

    /// Whether tiles can be drawn as filled surfaces (else they get borders).
    pub fn filled(&self) -> bool {
        matches!(self.mode, ColorMode::TrueColor | ColorMode::Ansi256) && self.panel != self.bg
    }

    /// A risk tier's color: slate, teal, amber, red.
    pub fn tier(&self, tier: reeve_core::policy::Tier) -> Color {
        use reeve_core::policy::Tier;
        match tier {
            Tier::T0 => self.dim,
            Tier::T1 => self.teal,
            Tier::T2 => self.amber,
            Tier::T3 => self.bad,
        }
    }

    /// Plain text on the panel ground.
    pub fn text(&self) -> Style {
        Style::default().fg(self.fg)
    }

    /// Secondary text.
    pub fn muted(&self) -> Style {
        Style::default().fg(self.dim)
    }

    /// Tertiary text.
    pub fn ghost(&self) -> Style {
        Style::default().fg(self.faint)
    }

    /// Bold accent.
    pub fn accent(&self) -> Style {
        Style::default().fg(self.brass).add_modifier(Modifier::BOLD)
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Nearest color in the xterm 6×6×6 cube or gray ramp.
fn quantize_256(c: Color) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let cube = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let (cr, cg, cb) = (cube(r), cube(g), cube(b));
    let level = |i: u8| if i == 0 { 0 } else { 55 + 40 * i };
    let cube_rgb = (level(cr), level(cg), level(cb));
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let gray_i = if avg > 238 {
        23
    } else {
        avg.saturating_sub(3) / 10
    } as u8;
    let gray_v = 8 + 10 * gray_i;
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if dist((gray_v, gray_v, gray_v)) < dist(cube_rgb) {
        Color::Indexed(232 + gray_i)
    } else {
        Color::Indexed(16 + 36 * cr + 6 * cg + cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_wins() {
        let env = |k: &str| match k {
            "NO_COLOR" => Some("1".to_string()),
            "COLORTERM" => Some("truecolor".to_string()),
            _ => None,
        };
        assert_eq!(ColorMode::detect("truecolor", env), ColorMode::Mono);
    }

    #[test]
    fn detects_truecolor_and_falls_back() {
        let tc = |k: &str| (k == "COLORTERM").then(|| "truecolor".to_string());
        assert_eq!(ColorMode::detect("auto", tc), ColorMode::TrueColor);
        let xterm = |k: &str| (k == "TERM").then(|| "xterm-256color".to_string());
        assert_eq!(ColorMode::detect("auto", xterm), ColorMode::Ansi256);
        assert_eq!(ColorMode::detect("16", tc), ColorMode::Ansi16);
    }

    #[test]
    fn degraded_themes_hold_no_rgb() {
        for mode in [ColorMode::Ansi256, ColorMode::Ansi16, ColorMode::Mono] {
            for t in [Theme::slate(), Theme::ink(), Theme::brass()] {
                let t = t.degrade(mode);
                assert!(!matches!(t.brass, Color::Rgb(..)), "{mode:?}");
                assert!(!matches!(t.tint(t.bad), Color::Rgb(..)), "{mode:?}");
            }
        }
    }

    #[test]
    fn slate_is_the_default_and_tiles_fill_only_with_color() {
        assert_eq!(Theme::named("", ColorMode::TrueColor), Theme::slate());
        assert_eq!(Theme::named("ink", ColorMode::TrueColor), Theme::ink());
        assert!(Theme::slate().filled());
        assert!(!Theme::slate().degrade(ColorMode::Ansi16).filled());
        assert!(!Theme::slate().degrade(ColorMode::Mono).filled());
    }

    #[test]
    fn gradients_hit_their_stops() {
        let t = Theme::brass();
        assert_eq!(t.gradient(&[t.brass, t.teal], 0.0), t.brass);
        assert_eq!(t.gradient(&[t.brass, t.teal], 1.0), t.teal);
    }
}
