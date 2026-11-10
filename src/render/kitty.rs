//! Live figure frames drawn with the Kitty graphics protocol directly.
//!
//! [`ratatui_image`] decodes an image, re-encodes it as (deflated) RGBA and
//! carries the transmission inside a buffer cell, which is fine for an image
//! shown once but far too slow for a stream of frames. Here a frame's PNG, as
//! it came from the browser, is handed to the terminal untouched (`f=100`,
//! the base64 is passed through) and the terminal scales it to a box of cells
//! through a virtual placement, shown with unicode placeholders.

use std::io::Write;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Image ids the frames rotate through. With three, the image on screen and
/// the one before it stay intact while the next is transmitted, so a frame is
/// never replaced while the terminal may still be drawing it.
const IDS: [u32; 3] = [0x5a_7e_01, 0x5a_7e_02, 0x5a_7e_03];

/// The character kitty reserves for image placeholders.
const PLACEHOLDER: char = '\u{10EEEE}';

/// Base64 characters per transmission chunk (the protocol's maximum).
const CHUNK: usize = 4096;

/// The frames of one live figure.
#[derive(Default)]
pub struct FrameStream {
    /// Frames transmitted so far.
    sent: u64,
}

impl FrameStream {
    /// Whether a frame has been transmitted (so there is something to place).
    pub fn has_frame(&self) -> bool {
        self.sent > 0
    }

    fn current(&self) -> u32 {
        IDS[((self.sent + IDS.len() as u64 - 1) % IDS.len() as u64) as usize]
    }

    /// Transmit a frame (base64 PNG) to be shown scaled to `cols`×`rows` cells.
    pub fn transmit(&mut self, out: &mut impl Write, png_base64: &str, cols: u16, rows: u16) -> std::io::Result<()> {
        let id = IDS[(self.sent % IDS.len() as u64) as usize];
        let mut seq = String::with_capacity(png_base64.len() + png_base64.len() / CHUNK * 16 + 96);
        let bytes = png_base64.as_bytes();
        let mut chunks = bytes.chunks(CHUNK).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = u8::from(chunks.peek().is_some());
            // q=2 on every chunk: the terminal must not answer any of them.
            seq.push_str("\x1b_Gq=2,");
            if first {
                // A virtual placement (U=1) of the whole image in a c×r box.
                seq.push_str(&format!("a=T,U=1,f=100,t=d,i={id},c={cols},r={rows},"));
                first = false;
            }
            seq.push_str(&format!("m={more};"));
            // Base64 is ASCII, so any split of it is valid UTF-8.
            seq.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            seq.push_str("\x1b\\");
        }
        out.write_all(seq.as_bytes())?;
        out.flush()?;
        self.sent += 1;
        Ok(())
    }

    /// Fill `area` with the placeholders that show the latest frame.
    pub fn place(&self, area: Rect, buf: &mut Buffer) {
        if !self.has_frame() {
            return;
        }
        let [_, r, g, b] = self.current().to_be_bytes();
        let fg = Color::Rgb(r, g, b);
        let max = DIACRITICS.len() as u16;
        let mut symbol = String::with_capacity(12);
        for y in 0..area.height.min(max) {
            for x in 0..area.width.min(max) {
                let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else { continue };
                // Every cell names its own row and column: the buffer diff may
                // write any subset of them.
                symbol.clear();
                symbol.push(PLACEHOLDER);
                symbol.push(DIACRITICS[y as usize]);
                symbol.push(DIACRITICS[x as usize]);
                cell.set_symbol(&symbol).set_fg(fg).set_diff_option(CellDiffOption::ForcedWidth(UNIT));
            }
        }
    }

    /// Free the frames held by the terminal.
    pub fn delete(&mut self, out: &mut impl Write) {
        if self.has_frame() {
            for id in IDS {
                let _ = write!(out, "\x1b_Gq=2,a=d,d=I,i={id}\x1b\\");
            }
            let _ = out.flush();
            self.sent = 0;
        }
    }
}

const UNIT: std::num::NonZeroU16 = std::num::NonZeroU16::new(1).unwrap();

/// Row/column diacritics of the placeholder scheme, from kitty's
/// `rowcolumn-diacritics.txt`.
#[rustfmt::skip]
static DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}', '\u{346}',
    '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}', '\u{35B}', '\u{363}',
    '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}', '\u{36A}', '\u{36B}', '\u{36C}',
    '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}', '\u{485}', '\u{486}', '\u{487}', '\u{592}',
    '\u{593}', '\u{594}', '\u{595}', '\u{597}', '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}',
    '\u{59F}', '\u{5A0}', '\u{5A1}', '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}',
    '\u{610}', '\u{611}', '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}',
    '\u{658}', '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}', '\u{6D7}', '\u{6D8}',
    '\u{6D9}', '\u{6DA}', '\u{6DB}', '\u{6DC}', '\u{6DF}', '\u{6E0}', '\u{6E1}', '\u{6E2}', '\u{6E4}',
    '\u{6E7}', '\u{6E8}', '\u{6EB}', '\u{6EC}', '\u{730}', '\u{732}', '\u{733}', '\u{735}', '\u{736}',
    '\u{73A}', '\u{73D}', '\u{73F}', '\u{740}', '\u{741}', '\u{743}', '\u{745}', '\u{747}', '\u{749}',
    '\u{74A}', '\u{7EB}', '\u{7EC}', '\u{7ED}', '\u{7EE}', '\u{7EF}', '\u{7F0}', '\u{7F1}', '\u{7F3}',
    '\u{816}', '\u{817}', '\u{818}', '\u{819}', '\u{81B}', '\u{81C}', '\u{81D}', '\u{81E}', '\u{81F}',
    '\u{820}', '\u{821}', '\u{822}', '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82A}',
    '\u{82B}', '\u{82C}', '\u{82D}', '\u{951}', '\u{953}', '\u{954}', '\u{F82}', '\u{F83}', '\u{F86}',
    '\u{F87}', '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}', '\u{1B6E}',
    '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}', '\u{1CD2}', '\u{1CDA}',
    '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}', '\u{1DC5}', '\u{1DC6}', '\u{1DC7}',
    '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}', '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}',
    '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}', '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}',
    '\u{1DDF}', '\u{1DE0}', '\u{1DE1}', '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}',
    '\u{20D0}', '\u{20D1}', '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}',
    '\u{20E7}', '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}', '\u{2DEB}',
    '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}', '\u{2DF3}', '\u{2DF4}',
    '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}', '\u{2DFB}', '\u{2DFC}', '\u{2DFD}',
    '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}', '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}',
    '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}', '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}',
    '\u{A8EB}', '\u{A8EC}', '\u{A8ED}', '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}',
    '\u{AAB3}', '\u{AAB7}', '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}',
    '\u{FE23}', '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}',
    '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}', '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmit_chunks_and_rotates_ids() {
        let mut s = FrameStream::default();
        let mut out = Vec::new();
        let data = "A".repeat(CHUNK * 2 + 10);
        s.transmit(&mut out, &data, 120, 40).unwrap();
        let text = String::from_utf8(out).unwrap();
        let parts: Vec<&str> = text.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with(&format!("\x1b_Gq=2,a=T,U=1,f=100,t=d,i={},c=120,r=40,m=1;", IDS[0])));
        assert!(parts[1].starts_with("\x1b_Gq=2,m=1;"));
        assert!(parts[2].starts_with("\x1b_Gq=2,m=0;"));
        let payload: usize = parts.iter().map(|p| p.split(';').nth(1).unwrap().len()).sum();
        assert_eq!(payload, data.len());
        assert_eq!(s.current(), IDS[0]);
        s.transmit(&mut Vec::new(), "AAAA", 1, 1).unwrap();
        assert_eq!(s.current(), IDS[1]);
    }

    #[test]
    fn place_addresses_every_cell() {
        let mut s = FrameStream::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        s.place(buf.area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        s.transmit(&mut Vec::new(), "AAAA", 4, 3).unwrap();
        s.place(buf.area, &mut buf);
        let want: String = [PLACEHOLDER, DIACRITICS[2], DIACRITICS[3]].iter().collect();
        assert_eq!(buf[(3, 2)].symbol(), want);
        assert_eq!(buf[(3, 2)].fg, Color::Rgb(0x5a, 0x7e, 0x01));
    }
}
