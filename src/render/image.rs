//! Decoded, protocol-encoded images keyed by content hash.
//!
//! Each image is encoded once (off the UI thread) at a fixed cell size as a
//! [`SlicedProtocol`], which can be drawn partially when scrolled.

use std::collections::HashMap;
use std::io::Cursor;

use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::{FilterType, FontSize, Resize};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;

/// Largest factor by which a raster image is enlarged to fill the popout.
pub const MAX_UPSCALE: u32 = 4;

/// Kitty placeholder diacritics only exist for 297 rows/columns.
pub const KITTY_MAX_CELLS: u16 = 297;

pub enum ImageState {
    Pending,
    Ready(SlicedProtocol),
    Failed(String),
}

pub struct ImageEntry {
    pub state: ImageState,
    pub cols: u16,
    pub rows: u16,
}

pub struct ImageStore {
    picker: Picker,
    entries: HashMap<u64, ImageEntry>,
    tx: UnboundedSender<AppEvent>,
}

impl ImageStore {
    pub fn new(picker: Picker, tx: UnboundedSender<AppEvent>) -> Self {
        ImageStore { picker, entries: HashMap::new(), tx }
    }

    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    pub fn protocol_type(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    /// Largest image the protocol can place, in cells. Kitty unicode
    /// placeholders address rows and columns with a table of 297 diacritics,
    /// and ratatui-image leaves cells beyond that undrawn.
    pub fn max_cells(&self) -> Option<u16> {
        match self.picker.protocol_type() {
            ProtocolType::Kitty => Some(KITTY_MAX_CELLS),
            _ => None,
        }
    }

    /// Clamp a `cols`×`rows` box to what the protocol can draw.
    pub fn clamp_box(&self, cols: u16, rows: u16) -> (u16, u16) {
        match self.max_cells() {
            Some(m) => (cols.min(m), rows.min(m)),
            None => (cols, rows),
        }
    }

    pub fn is_halfblocks(&self) -> bool {
        matches!(self.picker.protocol_type(), ProtocolType::Halfblocks)
    }

    pub fn get(&self, hash: u64) -> Option<&ImageEntry> {
        self.entries.get(&hash)
    }

    /// Register raw encoded image bytes (PNG/JPEG/GIF). Returns the cell size
    /// the image will occupy, kicking off decoding in the background.
    pub fn ensure_bytes(&mut self, hash: u64, bytes: Vec<u8>, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        self.ensure_fit(hash, bytes, max_cols, max_rows, false)
    }

    /// Like [`ensure_bytes`](Self::ensure_bytes), but stored under an explicit
    /// `key` (so the same image can exist at several sizes). With `upscale`,
    /// images smaller than the box are enlarged (up to [`MAX_UPSCALE`]×) to
    /// fill it, as in the output popout.
    pub fn ensure_fit(&mut self, key: u64, bytes: Vec<u8>, max_cols: u16, max_rows: u16, upscale: bool) -> Option<(u16, u16)> {
        if let Some(e) = self.entries.get(&key) {
            return Some((e.cols, e.rows));
        }
        let (px_w, px_h) = ::image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        let font = self.picker.font_size();
        let (max_cols, max_rows) = self.clamp_box(max_cols, max_rows);
        let (cols, rows) = if upscale {
            fit_cells(px_w.saturating_mul(MAX_UPSCALE), px_h.saturating_mul(MAX_UPSCALE), font, max_cols, max_rows)
        } else {
            fit_cells(px_w, px_h, font, max_cols, max_rows)
        };
        self.entries.insert(key, ImageEntry { state: ImageState::Pending, cols, rows });
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let resize = if upscale { Resize::Scale(Some(FilterType::CatmullRom)) } else { Resize::Fit(None) };
            let result = ::image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|img| {
                    SlicedProtocol::new_with_resize(&picker, img, Size { width: cols, height: rows }, resize)
                        .map_err(|e| e.to_string())
                });
            let _ = tx.send(AppEvent::ImageReady { hash: key, result });
        });
        Some((cols, rows))
    }

    /// Register one frame of a live figure, already rendered at the pixel size
    /// of a `cols`×`rows` box, so it is shown as is (no rescaling).
    pub fn ensure_frame(&mut self, key: u64, bytes: Vec<u8>, cols: u16, rows: u16) {
        self.entries.insert(key, ImageEntry { state: ImageState::Pending, cols, rows });
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = ::image::load_from_memory(&bytes).map_err(|e| e.to_string()).and_then(|img| {
                SlicedProtocol::new_with_resize(&picker, img, Size { width: cols, height: rows }, Resize::Fit(None))
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send(AppEvent::ImageReady { hash: key, result });
        });
    }

    /// Drop an entry (e.g. a popout-sized render that is no longer shown).
    pub fn remove(&mut self, key: u64) {
        self.entries.remove(&key);
    }

    pub fn set_result(&mut self, hash: u64, result: Result<SlicedProtocol, String>) {
        if let Some(e) = self.entries.get_mut(&hash) {
            match result {
                Ok(p) => {
                    let s = p.size();
                    if s.width > 0 && s.height > 0 {
                        e.cols = s.width;
                        e.rows = s.height;
                    }
                    e.state = ImageState::Ready(p);
                }
                Err(msg) => e.state = ImageState::Failed(msg),
            }
        }
    }
}

/// Fit an image of `px_w`×`px_h` pixels into at most `max_cols`×`max_rows`
/// cells, preserving aspect ratio.
pub fn fit_cells(px_w: u32, px_h: u32, font: FontSize, max_cols: u16, max_rows: u16) -> (u16, u16) {
    let fw = u32::from(font.width.max(1));
    let fh = u32::from(font.height.max(1));
    let mut cols = px_w.div_ceil(fw).max(1) as f64;
    let mut rows = px_h.div_ceil(fh).max(1) as f64;
    let max_cols = f64::from(max_cols.max(1));
    let max_rows = f64::from(max_rows.max(1));
    if rows > max_rows {
        cols = (cols * max_rows / rows).ceil();
        rows = max_rows;
    }
    if cols > max_cols {
        rows = (rows * max_cols / cols).ceil();
        cols = max_cols;
    }
    (cols.max(1.0) as u16, rows.max(1.0) as u16)
}
