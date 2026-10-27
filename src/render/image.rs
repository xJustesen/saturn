//! Decoded, protocol-encoded images keyed by content hash.
//!
//! Each image is encoded once (off the UI thread) at a fixed cell size as a
//! [`SlicedProtocol`], which can be drawn partially when scrolled.

use std::collections::HashMap;
use std::io::Cursor;

use base64::Engine;
use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::{FontSize, Resize};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::AppEvent;

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

    pub fn is_halfblocks(&self) -> bool {
        matches!(self.picker.protocol_type(), ProtocolType::Halfblocks)
    }

    pub fn get(&self, hash: u64) -> Option<&ImageEntry> {
        self.entries.get(&hash)
    }

    pub fn ensure_b64(&mut self, hash: u64, b64: &str, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        if let Some(e) = self.entries.get(&hash) {
            return Some((e.cols, e.rows));
        }
        let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(cleaned.as_bytes())
            .ok()?;
        self.ensure_bytes(hash, bytes, max_cols, max_rows)
    }

    /// Register raw encoded image bytes (PNG/JPEG/GIF). Returns the cell size
    /// the image will occupy, kicking off decoding in the background.
    pub fn ensure_bytes(&mut self, hash: u64, bytes: Vec<u8>, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        if let Some(e) = self.entries.get(&hash) {
            return Some((e.cols, e.rows));
        }
        let (px_w, px_h) = ::image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        let (cols, rows) = fit_cells(px_w, px_h, self.picker.font_size(), max_cols, max_rows);
        self.entries.insert(hash, ImageEntry { state: ImageState::Pending, cols, rows });
        let picker = self.picker.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = ::image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|img| {
                    SlicedProtocol::new_with_resize(
                        &picker,
                        img,
                        Size { width: cols, height: rows },
                        Resize::Fit(None),
                    )
                    .map_err(|e| e.to_string())
                });
            let _ = tx.send(AppEvent::ImageReady { hash, result });
        });
        Some((cols, rows))
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
