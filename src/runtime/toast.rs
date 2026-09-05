//! The toast overlay, shared by the menu loop and the streaming loop so the two stay
//! pixel-identical.
use anyhow::Result;

use crate::app::render::tile;
use crate::ui::render::{DrawCmd, Rect, TileSink};
use crate::ui::text::{Fonts, TextCache};

/// Distance from the top edge. Top-centre never overlaps the top-right stats panel or the
/// bottom log tail.
const TOAST_TOP_Y: i32 = 24;

/// One loop's toast tile, and the memo that keeps it from being rebuilt every frame.
///
/// The tile is alpha-independent — the fade is applied at draw time via `DrawCmd`'s `alpha` —
/// so only a *text* change needs a re-raster and re-upload. Without the memo the identical tile
/// would be rasterized and uploaded on every one of the ~120 frames a single toast lives for.
#[derive(Default)]
pub(super) struct Toast {
    /// Last rendered `(text, width, height)`.
    uploaded: Option<(String, u32, u32)>,
}

impl Toast {
    /// Appends this frame's toast command, uploading a fresh tile only if the text changed.
    /// `frame` is `ui::widgets::Notification::frame()`'s output — `None` while no toast is up.
    pub fn draw(
        &mut self,
        sink: &mut dyn TileSink,
        (fonts, text_cache): (&Fonts, &mut TextCache),
        frame: &Option<(String, f32)>,
        display_w: i32,
        cmds: &mut Vec<DrawCmd>,
    ) -> Result<()> {
        let Some((text, alpha)) = frame else {
            return Ok(());
        };
        let (tw, th) = match &self.uploaded {
            Some((cached, w, h)) if cached == text => (*w, *h),
            _ => match self.upload(sink, fonts, text_cache, text) {
                Ok(dims) => dims,
                Err(e) => {
                    tracing::warn!("toast render failed: {e:#}");
                    return Ok(());
                }
            },
        };
        cmds.push(DrawCmd::Tex {
            tile: tile::NOTIFICATION,
            dst: Rect::new((display_w - tw as i32) / 2, TOAST_TOP_Y, tw, th),
            alpha: (alpha * 255.0) as u8,
        });
        Ok(())
    }

    fn upload(
        &mut self,
        sink: &mut dyn TileSink,
        fonts: &Fonts,
        text_cache: &mut TextCache,
        text: &str,
    ) -> Result<(u32, u32)> {
        let tile = crate::ui::rasterize(
            crate::ui::widgets::NotificationTile {
                font: fonts.value,
                text,
            },
            text_cache,
            fonts,
        )?;
        let dims = (tile.width(), tile.height());
        sink.upload(tile::NOTIFICATION, &tile, false)?;
        self.uploaded = Some((text.to_string(), dims.0, dims.1));
        Ok(dims)
    }
}
