//! What the assistant is told about the notebook: a cell's source and its
//! outputs as text for the model, with the figures as images.

use jupyter_protocol::{Media, MediaType};

use crate::ai::Image;
use crate::notebook::{Cell, CellKind, Notebook, Output, StreamName};
use crate::render::html;
use crate::ui::text::clean_text;

/// Longest text kept from one output: the head and the tail survive.
const MAX_OUTPUT_CHARS: usize = 4000;
/// Most figures sent with one cell.
const MAX_IMAGES: usize = 4;
/// Largest figure sent, in base64 characters (about 3.7 MB of image).
const MAX_IMAGE_B64: usize = 5_000_000;

/// A PNG of a plotly figure that has none in the notebook: the cached render.
pub type PlotlyPng<'a> = &'a dyn Fn(&serde_json::Value) -> Option<Vec<u8>>;

pub fn kind_name(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Code => "code",
        CellKind::Markdown => "markdown",
        CellKind::Raw => "raw",
    }
}

/// `text` cut to `MAX_OUTPUT_CHARS`, keeping the start and the end.
fn clip(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS * 3 / 4).collect();
    let tail: String = text.chars().skip(n - MAX_OUTPUT_CHARS / 4).collect();
    format!(
        "{head}\n… [{} characters left out] …\n{tail}",
        n - MAX_OUTPUT_CHARS
    )
}

/// The text a model can read from rich output, by preference.
fn media_text(media: &Media) -> Option<(&'static str, String)> {
    let find = |f: &dyn Fn(&MediaType) -> Option<String>| media.content.iter().find_map(f);
    if let Some(s) = find(&|m| {
        if let MediaType::Markdown(s) = m {
            Some(s.clone())
        } else {
            None
        }
    }) {
        return Some(("text/markdown", s));
    }
    if let Some(s) = find(&|m| {
        if let MediaType::Plain(s) = m {
            Some(s.clone())
        } else {
            None
        }
    }) {
        return Some(("text/plain", s));
    }
    if let Some(s) = find(&|m| {
        if let MediaType::Html(s) = m {
            Some(html::to_text(s))
        } else {
            None
        }
    }) {
        return Some(("text/html", s));
    }
    find(&|m| {
        if let MediaType::Latex(s) = m {
            Some(s.clone())
        } else {
            None
        }
    })
    .map(|s| ("text/latex", s))
}

fn media_image(media: &Media, plotly_png: PlotlyPng) -> Option<Image> {
    let b64 = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    for m in &media.content {
        match m {
            MediaType::Png(s) => {
                return Some(Image {
                    media_type: "image/png".into(),
                    data: b64(s),
                });
            }
            MediaType::Jpeg(s) => {
                return Some(Image {
                    media_type: "image/jpeg".into(),
                    data: b64(s),
                });
            }
            MediaType::Gif(s) => {
                return Some(Image {
                    media_type: "image/gif".into(),
                    data: b64(s),
                });
            }
            _ => {}
        }
    }
    media.content.iter().find_map(|m| match m {
        MediaType::Plotly(fig) => plotly_png(fig).map(|png| Image::png(&png)),
        _ => None,
    })
}

/// One cell for the model: its source and outputs as tagged text, and its
/// figures in the order the text mentions them. `number` counts from 1.
pub fn cell_context(cell: &Cell, number: usize, plotly_png: PlotlyPng) -> (String, Vec<Image>) {
    let mut out = format!(
        "<cell number=\"{number}\" id=\"{}\" kind=\"{}\"",
        cell.id,
        kind_name(cell.kind)
    );
    if let Some(n) = cell.execution_count {
        out.push_str(&format!(" execution_count=\"{n}\""));
    }
    out.push_str(&format!(">\n<source>\n{}\n</source>\n", cell.text()));
    let mut images = Vec::new();
    for o in &cell.outputs {
        match o {
            Output::Stream { name, text } => {
                let name = if *name == StreamName::Stderr {
                    "stderr"
                } else {
                    "stdout"
                };
                out.push_str(&format!(
                    "<output type=\"{name}\">\n{}\n</output>\n",
                    clip(&clean_text(text))
                ));
            }
            Output::Error {
                ename,
                evalue,
                traceback,
            } => {
                let tb = clean_text(&traceback.join("\n")).into_owned();
                out.push_str(&format!(
                    "<output type=\"error\">\n{ename}: {evalue}\n{}\n</output>\n",
                    clip(&tb)
                ));
            }
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                let image = media_image(data, plotly_png);
                let has_image = image.is_some();
                let text = media_text(data);
                match image {
                    Some(_) if images.len() >= MAX_IMAGES => {
                        out.push_str(
                            "<output type=\"figure\">[not attached: too many figures]</output>\n",
                        );
                    }
                    Some(img) if img.data.len() > MAX_IMAGE_B64 => {
                        out.push_str("<output type=\"figure\">[not attached: the image is too large]</output>\n");
                    }
                    Some(img) => {
                        images.push(img);
                        out.push_str(&format!(
                            "<output type=\"figure\">[attached as image {}]</output>\n",
                            images.len()
                        ));
                    }
                    None => {}
                }
                // A figure's text form is only its repr; skip it when the image went along.
                if let Some((mime, text)) = text
                    && !has_image
                {
                    out.push_str(&format!(
                        "<output type=\"{mime}\">\n{}\n</output>\n",
                        clip(&clean_text(&text))
                    ));
                }
            }
        }
    }
    out.push_str("</cell>");
    (out, images)
}

/// The preamble of a user turn about the cell at `idx`: where the user is,
/// and that cell in full.
pub fn focus_context(nb: &Notebook, idx: usize, plotly_png: PlotlyPng) -> (String, Vec<Image>) {
    let Some(cell) = nb.cells.get(idx) else {
        return (String::new(), Vec::new());
    };
    let (body, images) = cell_context(cell, idx + 1, plotly_png);
    let text = format!(
        "<saturn-context>\nNotebook {} ({}), {} cells. The user is looking at cell {}:\n{body}\n</saturn-context>",
        nb.title(),
        nb.language().unwrap_or("unknown language"),
        nb.cells.len(),
        idx + 1,
    );
    (text, images)
}

/// What every session is told about where it runs.
pub const SYSTEM_PROMPT: &str = "\
You are the assistant built into saturn, a terminal Jupyter notebook editor. The user talks to you from a narrow \
side panel next to their open notebook, so keep answers short and use plain markdown; avoid wide tables and long code \
blocks. Each message starts with a <saturn-context> block that shows the cell the user is looking at, with its \
outputs and figures; treat it as what \"this\", \"the plot\" or \"the error\" refer to. The open notebook belongs to \
saturn: never read or change its .ipynb file with file tools or NotebookEdit, because saturn holds it in memory and \
would overwrite your change. When the user wants a cell changed, show the new source in a fenced code block.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn none(_: &serde_json::Value) -> Option<Vec<u8>> {
        None
    }

    fn media(types: Vec<MediaType>) -> Media {
        Media { content: types }
    }

    #[test]
    fn source_streams_and_errors_become_tagged_text() {
        let mut cell = Cell::new(CellKind::Code, "print('hi')\n1/0");
        cell.execution_count = Some(7);
        cell.outputs.push(Output::Stream {
            name: StreamName::Stdout,
            text: "hi\n".into(),
        });
        cell.outputs.push(Output::Error {
            ename: "ZeroDivisionError".into(),
            evalue: "division by zero".into(),
            traceback: vec!["\u{1b}[31mTraceback\u{1b}[0m".into(), "  line 2".into()],
        });
        let (text, images) = cell_context(&cell, 3, &none);
        assert!(images.is_empty());
        assert!(text.starts_with("<cell number=\"3\" id=\""), "{text}");
        assert!(
            text.contains("kind=\"code\" execution_count=\"7\">"),
            "{text}"
        );
        assert!(
            text.contains("<source>\nprint('hi')\n1/0\n</source>"),
            "{text}"
        );
        assert!(
            text.contains("<output type=\"stdout\">\nhi\n\n</output>"),
            "{text}"
        );
        assert!(
            text.contains("ZeroDivisionError: division by zero\nTraceback\n  line 2"),
            "{text}"
        );
        assert!(!text.contains('\u{1b}'), "{text}");
    }

    #[test]
    fn figures_go_along_as_images_and_drop_their_repr() {
        let mut cell = Cell::new(CellKind::Code, "plot()");
        cell.outputs.push(Output::DisplayData {
            data: media(vec![
                MediaType::Plain("<Figure size 640x480>".into()),
                MediaType::Png("aGk=\n".into()),
            ]),
            metadata: Map::new(),
            display_id: None,
        });
        cell.outputs.push(Output::ExecuteResult {
            execution_count: 1,
            data: media(vec![
                MediaType::Html("<table><tr><td>1</td></tr></table>".into()),
                MediaType::Plain("   a\n0  1".into()),
            ]),
            metadata: Map::new(),
        });
        let (text, images) = cell_context(&cell, 1, &none);
        assert_eq!(
            images,
            vec![Image {
                media_type: "image/png".into(),
                data: "aGk=".into()
            }]
        );
        assert!(text.contains("[attached as image 1]"), "{text}");
        assert!(!text.contains("Figure size"), "{text}");
        assert!(
            text.contains("<output type=\"text/plain\">\n   a\n0  1\n</output>"),
            "{text}"
        );
    }

    #[test]
    fn plotly_figures_use_the_cached_render() {
        let mut cell = Cell::new(CellKind::Code, "fig");
        cell.outputs.push(Output::DisplayData {
            data: media(vec![MediaType::Plotly(serde_json::json!({"data": []}))]),
            metadata: Map::new(),
            display_id: None,
        });
        let png = |_: &serde_json::Value| Some(b"png".to_vec());
        let (_, images) = cell_context(&cell, 1, &png);
        assert_eq!(images, vec![Image::png(b"png")]);
        assert!(cell_context(&cell, 1, &none).1.is_empty());
    }

    #[test]
    fn long_output_keeps_its_head_and_tail() {
        let text = format!("start{}end", "x".repeat(3 * MAX_OUTPUT_CHARS));
        let cut = clip(&text);
        assert!(cut.starts_with("start") && cut.ends_with("end"));
        assert!(cut.contains("characters left out"));
        assert!(cut.chars().count() < MAX_OUTPUT_CHARS + 60);
    }

    #[test]
    fn the_focused_cell_is_introduced_with_the_notebook() {
        let mut nb = Notebook::empty();
        nb.cells = vec![
            Cell::new(CellKind::Markdown, "# Title"),
            Cell::new(CellKind::Code, "x = 1"),
        ];
        let (text, _) = focus_context(&nb, 1, &none);
        assert!(
            text.starts_with("<saturn-context>\nNotebook Untitled.ipynb"),
            "{text}"
        );
        assert!(
            text.contains("2 cells. The user is looking at cell 2:"),
            "{text}"
        );
        assert!(text.contains("<source>\nx = 1\n</source>"), "{text}");
        assert!(text.ends_with("</saturn-context>"), "{text}");
    }
}
