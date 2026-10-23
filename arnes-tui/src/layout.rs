//! The screen grid (docs/design.md): one column of reserved regions,
//! stacked, so no region can be drawn over another. Surfaces that come and
//! go (a picker, the approval question, notices) claim rows from the
//! conversation, which reflows above them.

/// A region in cells. Height 0 means "not shown at this size".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Region {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    #[must_use]
    pub fn rect(&self) -> egaku::Rect {
        egaku::Rect::new(f32::from(self.x), f32::from(self.y), f32::from(self.w), f32::from(self.h))
    }

    /// True when the two share a cell.
    #[must_use]
    pub const fn overlaps(&self, o: &Self) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.x < o.x + o.w
            && o.x < self.x + self.w
            && self.y < o.y + o.h
            && o.y < self.y + self.h
    }
}

/// What the surfaces want this frame, in rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Demand {
    /// Rows of prompt text (the box adds two for its border).
    pub prompt_rows: u16,
    /// Notice lane rows.
    pub notices: u16,
    /// Rows claimed by a picker or the approval question.
    pub claim: u16,
}

/// Every region, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regions {
    pub conversation: Region,
    pub claim: Region,
    pub notices: Region,
    pub activity: Region,
    pub prompt: Region,
    pub status: Region,
}

impl Regions {
    /// All regions, in stacking order.
    #[must_use]
    pub const fn all(&self) -> [Region; 6] {
        [self.conversation, self.claim, self.notices, self.activity, self.prompt, self.status]
    }
}

/// Lay out a `cols` x `rows` screen. Rows are handed out by priority, so a
/// tiny terminal keeps what matters: the prompt (at least its box), the
/// status line, the activity line, one conversation row, then notices, the
/// claimed surface, the prompt's extra rows, and the rest to the
/// conversation.
#[must_use]
pub fn layout(cols: u16, rows: u16, d: Demand) -> Regions {
    let mut left = rows;
    let mut take = |want: u16| {
        let got = want.min(left);
        left -= got;
        got
    };
    let prompt_min = take(3);
    let status = take(1);
    let activity = take(1);
    let conv_min = take(1);
    let notices = take(d.notices);
    let claim = take(d.claim);
    let prompt_extra = take((d.prompt_rows.max(1) + 2).saturating_sub(3));
    let conv = conv_min + left;
    let prompt = prompt_min + prompt_extra;

    let mut y = 0u16;
    let mut next = |h: u16| {
        let r = Region { x: 0, y, w: cols, h };
        y += h;
        r
    };
    Regions {
        conversation: next(conv),
        claim: next(claim),
        notices: next(notices),
        activity: next(activity),
        prompt: next(prompt),
        status: next(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_tile_the_screen_without_overlap_at_every_size() {
        for cols in [1u16, 10, 40, 80, 200] {
            for rows in 0u16..40 {
                for (n, c, p) in [(0, 0, 1), (3, 12, 1), (1, 8, 8), (5, 30, 20)] {
                    let r = layout(cols, rows, Demand { prompt_rows: p, notices: n, claim: c });
                    let all = r.all();
                    for (i, a) in all.iter().enumerate() {
                        assert!(a.y + a.h <= rows, "{cols}x{rows}: region {i} leaves the screen");
                        for b in &all[i + 1..] {
                            assert!(!a.overlaps(b), "{cols}x{rows}: {a:?} overlaps {b:?}");
                        }
                    }
                    let used: u16 = all.iter().map(|a| a.h).sum();
                    assert_eq!(used, rows, "{cols}x{rows}: every row belongs to a region");
                }
            }
        }
    }

    #[test]
    fn priorities_on_a_tiny_terminal() {
        let r = layout(40, 5, Demand { prompt_rows: 1, notices: 2, claim: 12 });
        assert_eq!((r.prompt.h, r.status.h, r.activity.h, r.conversation.h), (3, 1, 1, 0));
        let r = layout(40, 6, Demand { prompt_rows: 1, notices: 2, claim: 12 });
        assert_eq!((r.conversation.h, r.notices.h, r.claim.h), (1, 0, 0));
        let r = layout(40, 12, Demand { prompt_rows: 1, notices: 2, claim: 12 });
        assert_eq!((r.conversation.h, r.notices.h, r.claim.h), (1, 2, 4), "the claim takes what is left, the conversation keeps a row");
    }

    #[test]
    fn the_conversation_yields_to_notices() {
        let quiet = layout(80, 30, Demand { prompt_rows: 1, notices: 0, claim: 0 });
        let loud = layout(80, 30, Demand { prompt_rows: 1, notices: 2, claim: 0 });
        assert_eq!(loud.conversation.h + 2, quiet.conversation.h);
        assert_eq!(loud.prompt, quiet.prompt, "the prompt never moves for a notice");
    }
}
