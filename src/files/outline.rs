//! Notebook previews: reads only each cell's type and source (serde skips
//! `outputs` without building them) and draws the cells as styled lines.

use edtui::syntect::highlighting::Theme as SynTheme;
use ratatui::style::Style;
use ratatui::text::Line;
use serde::Deserialize;

use crate::editor::language_extension;
use crate::files::preview::{
    MAX_LINE_CHARS, NB_MAX_LINES, Preview, UiColors, cut_chars, strip_plain_bg,
};
use crate::render::render_markdown;
use crate::ui::text::{clean_text, expand_tabs, highlight_with};

#[derive(Deserialize, Default)]
struct NbFile {
    #[serde(default)]
    cells: Vec<NbCell>,
    #[serde(default)]
    metadata: NbMeta,
    nbformat: Option<u32>,
}

#[derive(Deserialize, Default)]
struct NbCell {
    #[serde(default)]
    cell_type: String,
    #[serde(default)]
    source: Source,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Source {
    One(String),
    Many(Vec<String>),
}

impl Default for Source {
    fn default() -> Self {
        Source::One(String::new())
    }
}

impl Source {
    fn text(&self) -> String {
        match self {
            Source::One(s) => s.clone(),
            Source::Many(v) => v.concat(),
        }
    }
}

#[derive(Deserialize, Default)]
struct NbMeta {
    #[serde(default)]
    language_info: Option<LangInfo>,
    #[serde(default)]
    kernelspec: Option<KernelSpec>,
}

#[derive(Deserialize, Default)]
struct LangInfo {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct KernelSpec {
    #[serde(default)]
    language: Option<String>,
}

pub fn build(bytes: &[u8], width: u16, syn: &SynTheme, tab_width: usize, ui: UiColors) -> Preview {
    let nb: NbFile = match serde_json::from_slice(bytes) {
        Ok(nb) => nb,
        Err(e) => return Preview::Placeholder(format!("cannot preview: {e}")),
    };
    if nb.nbformat.unwrap_or(4) < 4 {
        return Preview::Placeholder("v3 notebook: no preview (saturn can open it)".into());
    }
    let lang = nb
        .metadata
        .language_info
        .and_then(|l| l.name)
        .or_else(|| nb.metadata.kernelspec.and_then(|k| k.language))
        .unwrap_or_else(|| "python".into());
    let ss = &**crate::syntax::SYNTAX_SET;
    let syntax = crate::syntax::by_extension(language_extension(&lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let muted = Style::default().fg(ui.muted);
    let code_bg = ui
        .input_bg
        .map_or(Style::default(), |c| Style::default().bg(c));

    let total = nb.cells.len();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut note = None;
    for (i, cell) in nb.cells.iter().enumerate() {
        let kind = match cell.cell_type.as_str() {
            "markdown" => "markdown",
            "raw" => "raw",
            _ => "code",
        };
        let mut cell_lines = vec![Line::styled(format!("── [{}] {kind} ──", i + 1), muted)];
        let source = cell.source.text();
        let source = clean_text(&source);
        match kind {
            "markdown" => cell_lines
                .extend(render_markdown(&source, width.saturating_sub(1).max(1), tab_width).lines),
            "raw" => cell_lines.extend(
                expand_tabs(&source, tab_width)
                    .split('\n')
                    .map(|l| Line::styled(cut_chars(l, MAX_LINE_CHARS).to_string(), muted)),
            ),
            _ => {
                let cut: Vec<&str> = source
                    .split('\n')
                    .map(|l| cut_chars(l, MAX_LINE_CHARS))
                    .collect();
                let mut code = highlight_with(&cut.join("\n"), syntax, syn, tab_width).lines;
                strip_plain_bg(&mut code, syn);
                cell_lines.extend(code.into_iter().map(|l| l.style(code_bg)));
            }
        }
        if lines.len() + cell_lines.len() > NB_MAX_LINES {
            let room = NB_MAX_LINES - lines.len();
            lines.extend(cell_lines.into_iter().take(room));
            note = Some(format!(
                "first {NB_MAX_LINES} lines (cells 1–{} of {total})",
                i + 1
            ));
            break;
        }
        lines.extend(cell_lines);
    }
    Preview::Doc {
        lines,
        line_numbers: false,
        note,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::theme::Theme;

    /// From an error output in `sample.ipynb`; in no cell source.
    const SAMPLE_PROBE: &str = "ZeroDivisionError";
    /// From a display_data output in `jupyterlab.ipynb`; in no cell source.
    const JUPYTERLAB_PROBE: &str = "<Figure>";

    fn ui() -> UiColors {
        UiColors {
            muted: Color::Gray,
            input_bg: Some(Color::Black),
            line_numbers: Color::DarkGray,
        }
    }

    fn doc(bytes: &[u8]) -> (Vec<String>, Option<String>) {
        let syn = Theme::load("monokai").unwrap().syntax;
        match build(bytes, 80, &syn, 4, ui()) {
            Preview::Doc {
                lines,
                line_numbers,
                note,
            } => {
                assert!(!line_numbers);
                (
                    lines
                        .iter()
                        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                        .collect(),
                    note,
                )
            }
            Preview::Placeholder(p) => panic!("placeholder: {p}"),
        }
    }

    fn placeholder(bytes: &[u8]) -> String {
        let syn = Theme::load("monokai").unwrap().syntax;
        match build(bytes, 80, &syn, 4, ui()) {
            Preview::Placeholder(p) => p,
            Preview::Doc { .. } => panic!("expected a placeholder"),
        }
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("tests/fixtures/{name}")).unwrap()
    }

    fn separators(lines: &[String]) -> usize {
        lines.iter().filter(|l| l.starts_with("── [")).count()
    }

    #[test]
    fn one_separator_per_cell() {
        for (name, cells) in [
            ("sample.ipynb", 10),
            ("jupyterlab.ipynb", 4),
            ("legacy44.ipynb", 2),
        ] {
            let (lines, _) = doc(&fixture(name));
            assert_eq!(separators(&lines), cells, "{name}");
        }
        let (lines, _) = doc(&fixture("jupyterlab.ipynb"));
        assert_eq!(lines[0], "── [1] markdown ──");
        assert!(lines.iter().any(|l| l == "── [3] raw ──"));
    }

    #[test]
    fn outputs_are_left_out() {
        let (lines, _) = doc(&fixture("sample.ipynb"));
        assert!(!lines.iter().any(|l| l.contains(SAMPLE_PROBE)));
        let (lines, _) = doc(&fixture("jupyterlab.ipynb"));
        assert!(!lines.iter().any(|l| l.contains(JUPYTERLAB_PROBE)));
        // legacy44's only output (`hi`) is also in its source: count lines.
        let (lines, _) = doc(&fixture("legacy44.ipynb"));
        assert_eq!(
            lines,
            vec![
                "── [1] markdown ──",
                "# old notebook",
                "── [2] code ──",
                "print('hi')"
            ]
        );
    }

    #[test]
    fn list_source_and_kernelspec_without_language() {
        let nb = br#"{"nbformat": 4, "nbformat_minor": 5,
            "metadata": {"kernelspec": {"name": "python3", "display_name": "Python 3"}},
            "cells": [{"cell_type": "code", "source": ["x = 1\n", "y = 2"], "outputs": [], "metadata": {}}]}"#;
        let (lines, note) = doc(nb);
        assert_eq!(lines, vec!["── [1] code ──", "x = 1", "y = 2"]);
        assert!(note.is_none());
    }

    #[test]
    fn empty_notebook_has_no_cells() {
        let (lines, _) =
            doc(br#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": []}"#);
        assert!(lines.is_empty());
    }

    #[test]
    fn invalid_json_and_v3() {
        assert!(placeholder(b"{not json").starts_with("cannot preview: "));
        assert_eq!(
            placeholder(br#"{"nbformat": 3, "worksheets": []}"#),
            "v3 notebook: no preview (saturn can open it)"
        );
    }

    #[test]
    fn long_notebook_stops_with_a_note() {
        let source: String = (0..300).map(|i| format!("x{i}\\n")).collect();
        let cell = format!(r#"{{"cell_type": "code", "source": "{source}", "outputs": []}}"#);
        let nb = format!(
            r#"{{"nbformat": 4, "metadata": {{}}, "cells": [{}]}}"#,
            vec![cell; 10].join(",")
        );
        let (lines, note) = doc(nb.as_bytes());
        assert_eq!(lines.len(), NB_MAX_LINES);
        assert_eq!(note.as_deref(), Some("first 2000 lines (cells 1–7 of 10)"));
    }

    #[test]
    #[ignore]
    fn big_output_parses_fast() {
        let blob = "A".repeat(20 * 1024 * 1024);
        let nb = format!(
            r#"{{"nbformat": 4, "metadata": {{}}, "cells": [{{"cell_type": "code", "source": "plot()",
               "outputs": [{{"output_type": "display_data", "data": {{"image/png": "{blob}"}}}}]}}]}}"#
        );
        let t = std::time::Instant::now();
        let (lines, _) = doc(nb.as_bytes());
        assert!(
            t.elapsed() < std::time::Duration::from_millis(300),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(lines.len(), 2);
    }
}
