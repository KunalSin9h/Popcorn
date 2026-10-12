//! UI icons (Lucide, ISC) and brand marks, embedded at build time and served to gpui as `icons/<path>`.
use gpui::{AssetSource, Result, SharedString};
use include_dir::{Dir, include_dir};
use std::borrow::Cow;

static ICONS: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/icons");

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.get_file(path.trim_start_matches("icons/")).map(|f| Cow::Borrowed(f.contents())))
    }
    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
