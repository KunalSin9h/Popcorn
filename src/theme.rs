//! "Lab" theme. Cool enamel frame, a glass working surface, graphite ink, and one loud signal
//! color (sodium yellow) kept for the moment a case needs you. Light and dark share the layout.
use gpui::{Hsla, Pixels, WindowAppearance, hsla, px, rgb};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Theme {
    pub bg: Hsla,      // glass: the working surface
    pub chrome: Hsla,  // enamel: window frame and title bar
    pub surface: Hsla, // inset wells, inactive tabs
    pub hover: Hsla,
    pub selected: Hsla,
    pub seam: Hsla,    // hairlines
    pub frame: Hsla,   // outer window border
    pub fg: Hsla,      // graphite ink; also the primary button
    pub muted: Hsla,
    pub faint: Hsla,
    pub signal: Hsla,  // sodium: a case is waiting on you
    pub ok: Hsla,      // contained, allowed
    pub bad: Hsla,     // refused, destructive
}

pub const UI_FONT: &str = "IBM Plex Sans";
pub const CODE_FONT: &str = "IBM Plex Mono";

static LIGHT: LazyLock<Theme> = LazyLock::new(|| Theme {
    bg: rgb(0xf6f7f4).into(),
    chrome: rgb(0xe3e6e1).into(),
    surface: rgb(0xeceee9).into(),
    hover: hsla(150. / 360., 0.1, 0.15, 0.05),
    selected: hsla(150. / 360., 0.1, 0.15, 0.09),
    seam: rgb(0xd3d8d1).into(),
    frame: hsla(0., 0., 0., 0.16),
    fg: rgb(0x1c2124).into(),
    muted: rgb(0x5e6762).into(),
    faint: rgb(0x98a19b).into(),
    signal: rgb(0xf0be2c).into(),
    ok: rgb(0x2f6b52).into(),
    bad: rgb(0xb23a2e).into(),
});

static DARK: LazyLock<Theme> = LazyLock::new(|| Theme {
    bg: rgb(0x20262a).into(),
    chrome: rgb(0x171b1e).into(),
    surface: rgb(0x272e32).into(),
    hover: hsla(0., 0., 1., 0.04),
    selected: hsla(0., 0., 1., 0.08),
    seam: rgb(0x323a3e).into(),
    frame: hsla(0., 0., 1., 0.09),
    fg: rgb(0xe4e8e3).into(),
    muted: rgb(0x9ca69f).into(),
    faint: rgb(0x66716b).into(),
    signal: rgb(0xf0be2c).into(),
    ok: rgb(0x6fbf98).into(),
    bad: rgb(0xe36a5c).into(),
});

static IS_DARK: AtomicBool = AtomicBool::new(false);

pub fn theme() -> &'static Theme {
    if IS_DARK.load(Ordering::Relaxed) { &DARK } else { &LIGHT }
}

pub fn set_dark(dark: bool) {
    IS_DARK.store(dark, Ordering::Relaxed);
}

/// User preference, saved as one word in the config file.
#[derive(Clone, Copy, PartialEq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    fn path() -> Option<PathBuf> {
        // ponytail: XDG-style path on every OS; use platform config dirs if Windows users complain.
        let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::home_dir().map(|h| h.join(".config")))?;
        Some(base.join("popcorn").join("appearance"))
    }

    pub fn load() -> Self {
        match Self::path().and_then(|p| std::fs::read_to_string(p).ok()).as_deref().map(str::trim) {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn save(self) {
        let Some(p) = Self::path() else { return };
        let word = match self { Self::System => "system", Self::Light => "light", Self::Dark => "dark" };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(p, word).ok();
    }

    pub fn is_dark(self, system: WindowAppearance) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => matches!(system, WindowAppearance::Dark | WindowAppearance::VibrantDark),
        }
    }
}

pub const WINDOW_RADIUS: Pixels = px(12.);
