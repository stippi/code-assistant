use gpui_kit::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

/// The app's own icons, falling back to gpui-kit's component icons
/// (`icons/chevron-down.svg` and the like), which components such as the
/// code editor's fold buttons load by their built-in names.
#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*"]
#[exclude = "*.DS_Store"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match Self::get(path) {
            Some(f) => Ok(Some(f.data)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths: Vec<SharedString> = Self::iter()
            .filter(|p| p.starts_with(path))
            .map(Into::into)
            .collect();
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_own_and_component_icons() {
        assert!(Assets.load("icons/chevron_down.svg").unwrap().is_some());
        assert!(Assets.load("icons/chevron-down.svg").unwrap().is_some());
        assert!(Assets.load("icons/does-not-exist.svg").is_err());
    }

    #[test]
    fn lists_both_sources() {
        let paths = Assets.list("icons/chevron").unwrap();
        assert!(paths.iter().any(|p| p.as_ref() == "icons/chevron_down.svg"));
        assert!(paths.iter().any(|p| p.as_ref() == "icons/chevron-down.svg"));
    }
}
