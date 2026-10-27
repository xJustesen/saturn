//! Loading a notebook and saving it without changes must be byte-stable
//! (so that saving from saturn produces clean git diffs).

use std::path::Path;

fn roundtrip(path: &Path) {
    let original = std::fs::read_to_string(path).unwrap();
    let nb = saturn::notebook::io::load(path).unwrap();
    let json = saturn::notebook::io::to_json(&nb).unwrap();
    if json != original {
        // Show the first differing line to make failures readable.
        for (i, (a, b)) in original.lines().zip(json.lines()).enumerate() {
            if a != b {
                panic!("{}: first difference at line {}:\n  file: {a}\n  ours: {b}", path.display(), i + 1);
            }
        }
        panic!("{}: length differs ({} vs {})", path.display(), original.len(), json.len());
    }
}

/// The sample notebook carries plotly JSON whose floats Python prints with
/// more digits than the shortest round-trip form, so compare semantically.
#[test]
fn sample_notebook_is_semantically_stable() {
    let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.ipynb"));
    let original: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let nb = saturn::notebook::io::load(path).unwrap();
    let ours: serde_json::Value = serde_json::from_str(&saturn::notebook::io::to_json(&nb).unwrap()).unwrap();
    assert_eq!(original, ours);
}

#[test]
fn jupyterlab_notebook_is_byte_stable() {
    roundtrip(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jupyterlab.ipynb")));
}

#[test]
fn legacy_notebook_loads_and_gets_ids() {
    let nb = saturn::notebook::io::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/legacy44.ipynb"
    )))
    .unwrap();
    assert_eq!(nb.cells.len(), 2);
    assert!(!nb.cells[0].id.as_str().is_empty());
    let json = saturn::notebook::io::to_json(&nb).unwrap();
    assert!(json.contains("\"nbformat_minor\": 5"));
}
