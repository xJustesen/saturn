//! Code run silently in a fresh kernel.

/// For Python kernels: make plotly emit PNGs itself when kaleido is around
/// (then the app does not need its sidecar), and keep matplotlib inline.
pub fn startup_code(language: &str) -> Option<String> {
    match language.to_ascii_lowercase().as_str() {
        "python" | "python3" => Some(
            r#"
def _saturn_setup():
    try:
        import plotly.io as _pio
        import kaleido  # noqa: F401
        _pio.renderers.default = "plotly_mimetype+png"
    except Exception:
        pass
    try:
        import matplotlib
        matplotlib.use("module://matplotlib_inline.backend_inline")
    except Exception:
        pass
_saturn_setup()
del _saturn_setup
"#
            .to_string(),
        ),
        _ => None,
    }
}
