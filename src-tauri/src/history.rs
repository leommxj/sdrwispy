use std::collections::VecDeque;

use serde::Serialize;

use crate::model::SpectrumFrame;

#[derive(Debug, Clone, Serialize)]
pub struct HistoryInfo {
    pub capture_epoch: u64,
    pub frame_ids: Vec<u64>,
    pub oldest_time_ns: Option<i64>,
    pub newest_time_ns: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameBatch {
    pub capture_epoch: u64,
    pub frames: Vec<SpectrumFrame>,
    pub latest: Option<SpectrumFrame>,
    pub history: HistoryInfo,
    // None means the cursor predates the bounded eviction ledger.
    pub missed_frames: Option<usize>,
    pub total_evicted: u64,
    pub more: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryFrame {
    pub frame: Option<SpectrumFrame>,
    pub expired: bool,
    pub history: HistoryInfo,
}

pub struct FrameHistory {
    capture_epoch: u64,
    capacity: usize,
    frames: VecDeque<SpectrumFrame>,
    // At most capacity inclusive runs of actual evictions; never count ID gaps.
    evicted: VecDeque<[u64; 2]>,
    discarded_through: Option<u64>,
    total_evicted: u64,
}

impl FrameHistory {
    pub fn new(capture_epoch: u64, capacity: usize) -> Self {
        Self {
            capture_epoch,
            capacity: capacity.max(1),
            frames: VecDeque::with_capacity(capacity.max(1)),
            evicted: VecDeque::new(),
            discarded_through: None,
            total_evicted: 0,
        }
    }

    pub fn reset(&mut self, capture_epoch: u64) {
        self.capture_epoch = capture_epoch;
        self.frames.clear();
        self.evicted.clear();
        self.discarded_through = None;
        self.total_evicted = 0;
    }

    fn evict_front(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.total_evicted += 1;
            if let Some(last) = self.evicted.back_mut()
                && last[1].checked_add(1) == Some(frame.frame_id)
            {
                last[1] = frame.frame_id;
            } else {
                if self.evicted.len() == self.capacity {
                    self.discarded_through = self.evicted.pop_front().map(|range| range[1]);
                }
                self.evicted.push_back([frame.frame_id, frame.frame_id]);
            }
        }
    }

    pub fn clear(&mut self) {
        while !self.frames.is_empty() {
            self.evict_front();
        }
    }

    pub fn push(&mut self, frame: SpectrumFrame) {
        if frame.capture_epoch != self.capture_epoch || frame.partial || frame.current_db.is_empty()
        {
            return;
        }
        // The worker emits complete reports in strictly increasing frame-ID order.
        if self
            .back()
            .is_some_and(|last| frame.frame_id <= last.frame_id)
            || self
                .evicted
                .back()
                .is_some_and(|last| frame.frame_id <= last[1])
        {
            return;
        }
        while self.frames.len() >= self.capacity {
            self.evict_front();
        }
        self.frames.push_back(frame);
    }

    pub fn back(&self) -> Option<&SpectrumFrame> {
        self.frames.back()
    }

    pub fn get(&self, index: usize) -> Option<&SpectrumFrame> {
        self.frames.get(index)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn info(&self) -> HistoryInfo {
        HistoryInfo {
            capture_epoch: self.capture_epoch,
            frame_ids: self.frames.iter().map(|frame| frame.frame_id).collect(),
            oldest_time_ns: self.frames.front().map(|frame| frame.sweep_start_time_ns),
            newest_time_ns: self.back().map(|frame| frame.sweep_end_time_ns),
        }
    }

    pub fn batch(
        &self,
        after_capture_epoch: Option<u64>,
        after_frame_id: Option<u64>,
        limit: Option<usize>,
        latest: Option<SpectrumFrame>,
    ) -> FrameBatch {
        let after = after_frame_id.filter(|_| after_capture_epoch == Some(self.capture_epoch));
        let missed_frames = after.map_or(Some(0), |id| {
            if self.discarded_through.is_some_and(|last| id < last) {
                return None;
            }
            Some(self.evicted.iter().fold(0usize, |total, &[first, last]| {
                let count = last.saturating_sub(id.max(first.saturating_sub(1)));
                total.saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
            }))
        });
        let mut unread = self
            .frames
            .iter()
            .filter(|frame| after.is_none_or(|id| frame.frame_id > id));
        let frames = unread
            .by_ref()
            .take(limit.unwrap_or(16).clamp(1, 32))
            .cloned()
            .collect();
        FrameBatch {
            capture_epoch: self.capture_epoch,
            frames,
            latest: latest.filter(|frame| frame.capture_epoch == self.capture_epoch),
            history: self.info(),
            missed_frames,
            total_evicted: self.total_evicted,
            more: unread.next().is_some(),
        }
    }

    pub fn by_id(
        &self,
        capture_epoch: u64,
        frame_id: u64,
        direction: i32,
    ) -> Result<HistoryFrame, String> {
        if !(-1..=1).contains(&direction) {
            return Err("history direction must be -1, 0, or 1".to_owned());
        }
        let anchor = (capture_epoch == self.capture_epoch)
            .then(|| {
                self.frames
                    .binary_search_by_key(&frame_id, |frame| frame.frame_id)
                    .ok()
            })
            .flatten();
        let frame = anchor
            .and_then(|index| match direction {
                -1 => index.checked_sub(1).and_then(|index| self.get(index)),
                0 => self.get(index),
                1 => self.get(index + 1),
                _ => unreachable!(),
            })
            .cloned();
        Ok(HistoryFrame {
            frame,
            expired: anchor.is_none(),
            history: self.info(),
        })
    }
}

#[cfg(test)]
pub(crate) fn test_frame(capture_epoch: u64, frame_id: u64) -> SpectrumFrame {
    SpectrumFrame {
        capture_epoch,
        frame_id,
        frame_revision: frame_id,
        partial: false,
        sweep_start_time_ns: frame_id as i64 * 10,
        sweep_end_time_ns: frame_id as i64 * 10 + 5,
        band_start_hz: 100.0,
        band_stop_hz: 101.0,
        bin_width_hz: 1.0,
        current_db: vec![-80.0],
        average_db: vec![-80.0],
        max_hold_db: vec![-80.0],
        valid_bin_ranges: vec![[0, 1]],
        history_valid_bin_ranges: vec![[0, 1]],
        dc_excluded_bin_ranges: vec![],
        invalid_bin_count: 0,
        estimated_hops: 1,
        completed_hops: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_preserves_order_and_limits_and_keeps_progress_separate() {
        let history = FrameHistory::new(0, 900);
        let value = serde_json::to_value(history.info()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"capture_epoch": 0, "frame_ids": [], "oldest_time_ns": null, "newest_time_ns": null})
        );
        assert!(history.batch(None, None, None, None).frames.is_empty());
        assert!(history.by_id(0, 1, 0).unwrap().expired);
        let mut history = FrameHistory::new(7, 900);
        for id in 1..=40 {
            history.push(test_frame(7, id));
        }
        let mut progress = test_frame(7, 41);
        progress.partial = true;
        history.push(progress.clone());
        let first = history.batch(None, None, None, Some(progress));
        assert_eq!(
            first.frames.iter().map(|f| f.frame_id).collect::<Vec<_>>(),
            (1..=16).collect::<Vec<_>>()
        );
        assert!(first.more);
        assert!(first.latest.unwrap().partial);
        assert_eq!(first.history.frame_ids.len(), 40);
        assert_eq!(history.batch(None, None, Some(100), None).frames.len(), 32);
        assert_eq!(history.batch(None, None, Some(0), None).frames.len(), 1);
        let rest = history.batch(Some(7), Some(16), Some(32), None);
        assert_eq!(rest.frames.len(), 24);
        assert!(!rest.more);
        let empty = history.batch(Some(7), Some(40), None, None);
        assert!(empty.frames.is_empty());
        assert!(!empty.more);
    }

    #[test]
    fn eviction_counts_only_unread_complete_frames_not_id_gaps() {
        let mut history = FrameHistory::new(7, 2);
        for id in [1, 3, 4, 8, 9] {
            history.push(test_frame(7, id));
        }
        assert_eq!(history.info().frame_ids, vec![8, 9]);
        for (cursor, missed) in [
            (0, 3),
            (1, 2),
            (2, 2),
            (3, 1),
            (4, 0),
            (8, 0),
            (u64::MAX, 0),
        ] {
            assert_eq!(
                history
                    .batch(Some(7), Some(cursor), None, None)
                    .missed_frames,
                Some(missed)
            );
        }
        let batch = history.batch(Some(7), Some(1), Some(1), None);
        assert_eq!(batch.frames[0].frame_id, 8);
        assert!(batch.more);
        assert_eq!(batch.history.oldest_time_ns, Some(80));
        assert_eq!(batch.history.newest_time_ns, Some(95));
        assert_eq!(
            history.batch(None, Some(1), None, None).missed_frames,
            Some(0)
        );
        history.clear();
        assert_eq!(
            history.batch(Some(7), Some(3), None, None).missed_frames,
            Some(3)
        );

        let mut history = FrameHistory::new(7, 2);
        for id in 1..=10_000 {
            history.push(test_frame(7, id * 3));
            assert!(history.evicted.len() <= 2);
        }
        assert_eq!(history.info().frame_ids, vec![29_997, 30_000]);
        let old = history.batch(Some(7), Some(3), None, None);
        assert_eq!(old.missed_frames, None);
        assert_eq!(old.total_evicted, 9_998);
        assert!(serde_json::to_value(old).unwrap()["missed_frames"].is_null());
        for (cursor, missed) in [(29_988, 2), (29_989, 2), (29_991, 1), (29_994, 0)] {
            assert_eq!(
                history
                    .batch(Some(7), Some(cursor), None, None)
                    .missed_frames,
                Some(missed)
            );
        }
        history.clear();
        let cleared = history.batch(Some(7), Some(29_994), None, None);
        assert_eq!(cleared.missed_frames, Some(2));
        assert_eq!(cleared.total_evicted, 10_000);
        history.push(test_frame(7, 30_000));
        assert!(history.info().frame_ids.is_empty());
    }

    #[test]
    fn epoch_change_resets_cursor_and_loss_accounting() {
        let mut history = FrameHistory::new(7, 2);
        for id in 1..=6 {
            history.push(test_frame(7, id));
        }
        history.reset(8);
        assert_eq!(history.total_evicted, 0);
        assert!(history.discarded_through.is_none());
        assert!(history.evicted.is_empty());
        for id in 1..=5 {
            history.push(test_frame(8, id));
        }
        history.push(test_frame(7, 100));
        for old_id in [0, 2, 100] {
            let batch = history.batch(Some(7), Some(old_id), None, None);
            assert_eq!(
                batch.frames.iter().map(|f| f.frame_id).collect::<Vec<_>>(),
                vec![4, 5]
            );
            assert_eq!(batch.missed_frames, Some(0));
        }
        assert_eq!(
            history.batch(Some(8), Some(1), None, None).missed_frames,
            Some(2)
        );
        assert!(history.by_id(7, 4, 0).unwrap().expired);
    }

    #[test]
    fn navigation_uses_identity_and_never_jumps_forward_after_eviction() {
        let mut history = FrameHistory::new(7, 3);
        for id in [1, 3, 6] {
            history.push(test_frame(7, id));
        }
        assert_eq!(history.by_id(7, 6, -1).unwrap().frame.unwrap().frame_id, 3);
        assert_eq!(history.by_id(7, 1, 1).unwrap().frame.unwrap().frame_id, 3);
        assert_eq!(history.by_id(7, 3, 0).unwrap().frame.unwrap().frame_id, 3);
        history.push(test_frame(7, 8));
        let prev = history.by_id(7, 3, -1).unwrap();
        assert!(prev.frame.is_none());
        assert!(!prev.expired);
        history.push(test_frame(7, 10));
        for direction in [-1, 0, 1] {
            let expired = history.by_id(7, 3, direction).unwrap();
            assert!(expired.frame.is_none());
            assert!(expired.expired);
        }
        assert!(history.by_id(7, 10, 1).unwrap().frame.is_none());
        assert!(history.by_id(7, 10, 2).is_err());
    }
}
