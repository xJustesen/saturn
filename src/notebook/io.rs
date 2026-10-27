//! Loading and saving `.ipynb` files via the nbformat crate.

use std::path::Path;

use anyhow::{Context, Result, bail};

use super::Notebook;

pub fn load(path: &Path) -> Result<Notebook> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let v4 = match nbformat::parse_notebook(&json)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("cannot parse {}", path.display()))?
    {
        nbformat::Notebook::V4(nb) => nb,
        nbformat::Notebook::V4QuirksMode(q) => {
            tracing::warn!("{}: notebook had quirks {:?}; repaired", path.display(), q.quirks());
            q.repair()
        }
        nbformat::Notebook::Legacy(legacy) => nbformat::upgrade_legacy_notebook(legacy)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("upgrading legacy v4 notebook")?,
        nbformat::Notebook::V3(v3) => nbformat::upgrade_v3_notebook(v3)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("upgrading v3 notebook")?,
        _ => bail!("unsupported notebook format"),
    };
    Ok(Notebook::from_v4(v4, Some(path.to_path_buf())))
}

pub fn to_json(nb: &Notebook) -> Result<String> {
    let v4 = nb.to_v4();
    nbformat::serialize_notebook(&nbformat::Notebook::V4(v4)).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Atomic save: write to a temp file next to the target, then rename.
pub fn save(nb: &Notebook, path: &Path) -> Result<()> {
    let json = to_json(nb)?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.saturn-tmp"));
    std::fs::write(&tmp, json).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot rename to {}", path.display()))?;
    Ok(())
}
