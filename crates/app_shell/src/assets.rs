//! What the apps draw with, taken from Krust's own assets: its UI, monospace and password fonts,
//! and the close glyph of the title bar's buttons. Only these are embedded, not the whole folder.

use std::borrow::Cow;

use anyhow::{anyhow, Result};
use krustui::AssetProvider;
use rust_embed::RustEmbed;

#[derive(Clone, Copy, RustEmbed)]
#[folder = "../../app/assets"]
#[include = "bundled/fonts/roboto/*.ttf"]
#[include = "bundled/fonts/hack/*.ttf"]
#[include = "bundled/fonts/password.ttf"]
#[include = "bundled/svg/linux/decorations/close.svg"]
pub struct Assets;

pub static ASSETS: Assets = Assets;

impl AssetProvider for Assets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        <Assets as RustEmbed>::get(path)
            .map(|file| file.data)
            .ok_or_else(|| anyhow!("no asset exists at path {path}"))
    }
}
