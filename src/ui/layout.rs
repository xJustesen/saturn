//! Per-cell height cache with prefix sums for row-based virtual scrolling.

#[derive(Debug, Clone, Default)]
pub struct CellLayout {
    pub source_rows: u16,
    /// Height of each rendered output block (in order), after collapsing.
    pub outputs: Vec<u16>,
    /// Rows used for the "N outputs hidden" line when collapsed (0 or 1).
    pub collapsed_rows: u16,
    /// Total rows including the trailing gap row.
    pub total: u16,
}

impl CellLayout {
    pub fn outputs_start(&self) -> u16 {
        self.source_rows
    }
}

#[derive(Default)]
pub struct LayoutCache {
    width: u16,
    heights: Vec<Option<CellLayout>>,
    prefix: Vec<usize>,
    prefix_dirty: bool,
}

impl LayoutCache {
    pub fn set_width(&mut self, width: u16) {
        if self.width != width {
            self.width = width;
            self.invalidate_all();
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn invalidate_all(&mut self) {
        self.heights.fill(None);
        self.prefix_dirty = true;
    }

    pub fn invalidate_cell(&mut self, idx: usize) {
        if let Some(h) = self.heights.get_mut(idx) {
            *h = None;
        }
        self.prefix_dirty = true;
    }

    pub fn inserted(&mut self, idx: usize) {
        let idx = idx.min(self.heights.len());
        self.heights.insert(idx, None);
        self.prefix_dirty = true;
    }

    pub fn removed(&mut self, idx: usize) {
        if idx < self.heights.len() {
            self.heights.remove(idx);
        }
        self.prefix_dirty = true;
    }

    pub fn moved(&mut self, from: usize, to: usize) {
        if from < self.heights.len() && to < self.heights.len() {
            let h = self.heights.remove(from);
            self.heights.insert(to, h);
            self.prefix_dirty = true;
        }
    }

    /// Make sure every cell has a measured layout; `measure(idx)` is called
    /// for the missing ones.
    pub fn ensure(&mut self, n: usize, mut measure: impl FnMut(usize) -> CellLayout) {
        if self.heights.len() != n {
            self.heights.resize(n, None);
            self.prefix_dirty = true;
        }
        for i in 0..n {
            if self.heights[i].is_none() {
                self.heights[i] = Some(measure(i));
                self.prefix_dirty = true;
            }
        }
        if self.prefix_dirty {
            self.prefix.clear();
            self.prefix.reserve(n + 1);
            let mut acc = 0usize;
            self.prefix.push(0);
            for h in &self.heights {
                acc += h.as_ref().map(|l| l.total as usize).unwrap_or(0);
                self.prefix.push(acc);
            }
            self.prefix_dirty = false;
        }
    }

    pub fn is_measured(&self, idx: usize) -> bool {
        matches!(self.heights.get(idx), Some(Some(_)))
    }

    pub fn layout(&self, idx: usize) -> Option<&CellLayout> {
        self.heights.get(idx).and_then(|h| h.as_ref())
    }

    pub fn height(&self, idx: usize) -> usize {
        self.layout(idx).map(|l| l.total as usize).unwrap_or(0)
    }

    pub fn row_of(&self, idx: usize) -> usize {
        self.prefix.get(idx).copied().unwrap_or(0)
    }

    pub fn total_rows(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// Index of the cell containing document row `row` (or the last cell).
    pub fn cell_at_row(&self, row: usize) -> usize {
        if self.prefix.len() < 2 {
            return 0;
        }
        // prefix[i] <= row < prefix[i+1]
        match self.prefix.binary_search(&row) {
            Ok(i) => i.min(self.prefix.len() - 2),
            Err(i) => i.saturating_sub(1).min(self.prefix.len() - 2),
        }
    }
}
