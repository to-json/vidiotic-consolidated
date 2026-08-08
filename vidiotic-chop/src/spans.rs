//! In-memory span list: the set of trimmed ranges the user has marked,
//! possibly across more than one source video in a session — each span
//! carries its own source, so the list survives switching the open video.

use std::path::PathBuf;
use vidiotic_core::project::CropRect;

/// One marked span of a source video. `out_frame` is exclusive, matching
/// [`vidiotic_core::project::SpanProvenance`].
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub name: String,
    pub in_frame: u64,
    pub out_frame: u64,
    pub bpm: Option<f64>,
    /// Index into the app's `bank_names`.
    pub clip_bank: usize,
    /// The video this span's frame indices are relative to.
    pub source: PathBuf,
    /// Optional crop box rect (normalized coords in [0.0..1.0]).
    pub crop: Option<CropRect>,
}

/// Ordered spans plus the current selection, driving both the list panel and
/// preview seeking. Spans may come from more than one source video.
#[derive(Default)]
pub struct SpanList {
    pub spans: Vec<Span>,
    pub selected: Option<usize>,
}

impl SpanList {
    /// Append a new span `[in_frame, out_frame)` of `source` and select it.
    pub fn add(&mut self, source: PathBuf, in_frame: u64, out_frame: u64, crop: Option<CropRect>) {
        let n = self.spans.len();
        self.spans.push(Span {
            name: format!("span {}", n + 1),
            in_frame,
            out_frame: out_frame.max(in_frame + 1),
            bpm: None,
            clip_bank: 0,
            source,
            crop,
        });
        self.selected = Some(n);
    }

    /// Remove the span at `idx`, adjusting the selection.
    pub fn remove(&mut self, idx: usize) {
        if idx >= self.spans.len() {
            return;
        }
        self.spans.remove(idx);
        self.selected = match self.selected {
            Some(sel) if sel == idx => None,
            Some(sel) if sel > idx => Some(sel - 1),
            other => other,
        };
    }

    /// Select span `idx`, or clear the selection if out of range.
    pub fn select(&mut self, idx: usize) {
        self.selected = (idx < self.spans.len()).then_some(idx);
    }

    /// Swap span `idx` with its predecessor, keeping the selection on it.
    pub fn move_up(&mut self, idx: usize) {
        if idx == 0 || idx >= self.spans.len() {
            return;
        }
        self.spans.swap(idx, idx - 1);
        if self.selected == Some(idx) {
            self.selected = Some(idx - 1);
        }
    }

    /// Swap span `idx` with its successor, keeping the selection on it.
    pub fn move_down(&mut self, idx: usize) {
        if idx + 1 >= self.spans.len() {
            return;
        }
        self.spans.swap(idx, idx + 1);
        if self.selected == Some(idx) {
            self.selected = Some(idx + 1);
        }
    }
}
