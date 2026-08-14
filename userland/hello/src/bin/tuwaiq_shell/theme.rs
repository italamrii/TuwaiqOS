//! The TuwaiqOS palette.
//!
//! Charcoal, stone, and one copper. The neutrals are warmed toward the accent
//! rather than left as pure grey, so the whole surface reads as one rock face
//! instead of a UI kit with an orange added to it.
//!
//! Copper is reserved for a single meaning: the system acted. Focus, running,
//! on, unread. It is never a border for its own sake and never a gradient.
//! That restriction is what keeps one warm hue from looking like a theme swap.
//!
//! The light theme is the same escarpment at midday -- sand and limestone --
//! rather than an inversion of the dark one, which would have produced a
//! grey-blue office theme with no relationship to the identity.

pub type Rgb = (u8, u8, u8);

pub struct Theme {
    /// Deepest ground, behind everything
    pub void: Rgb,
    /// The panel's lower plain
    pub plain: Rgb,
    /// The panel's raised plateau, and every panel that rises from it
    pub plateau: Rgb,
    /// Window bodies
    pub chrome: Rgb,
    /// Hairlines and separators
    pub line: Rgb,
    /// Primary text
    pub ink: Rgb,
    /// Secondary text
    pub dim: Rgb,
    /// Disabled and placeholder text
    pub faint: Rgb,
    /// The accent
    pub copper: Rgb,
    /// Copper used as a wash behind an active band
    pub copper_wash: Rgb,
}

pub const DARK: Theme = Theme {
    void: (0x0A, 0x0C, 0x0F),
    plain: (0x14, 0x18, 0x1E),
    plateau: (0x1C, 0x22, 0x2B),
    chrome: (0x26, 0x2E, 0x39),
    line: (0x33, 0x3C, 0x49),
    ink: (0xE9, 0xE6, 0xE0),
    dim: (0x8E, 0x94, 0x9D),
    faint: (0x5C, 0x63, 0x6D),
    copper: (0xD9, 0x7A, 0x32),
    copper_wash: (0x3A, 0x2A, 0x1D),
};

pub const LIGHT: Theme = Theme {
    void: (0xE4, 0xDE, 0xD2),
    plain: (0xEF, 0xEA, 0xE1),
    plateau: (0xF7, 0xF4, 0xEE),
    chrome: (0xFB, 0xF9, 0xF5),
    line: (0xCF, 0xC6, 0xB6),
    ink: (0x19, 0x1B, 0x1F),
    dim: (0x5E, 0x63, 0x6B),
    faint: (0x8C, 0x91, 0x99),
    copper: (0xB0, 0x5D, 0x1E),
    copper_wash: (0xF0, 0xDF, 0xCA),
};
