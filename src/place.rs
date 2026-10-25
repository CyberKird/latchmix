//! Where the flyout goes relative to the taskbar. Pure geometry so every taskbar edge
//! can be unit tested without a second monitor or moving the real taskbar.
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Side {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

/// The taskbar is whichever edge the work area has been pulled in from.
pub fn side_of(monitor: RECT, work: RECT) -> Side {
    if work.top > monitor.top {
        Side::Top
    } else if work.left > monitor.left {
        Side::Left
    } else if work.right < monitor.right {
        Side::Right
    } else {
        Side::Bottom
    }
}

/// Top-left corner for a `w` x `h` flyout: in the corner nearest the tray, `gap` from the edges,
/// and never outside the work area.
pub fn place(work: RECT, side: Side, w: i32, h: i32, gap: i32) -> (i32, i32) {
    let (x, y) = match side {
        Side::Top => (work.right - w - gap, work.top + gap),
        Side::Left => (work.left + gap, work.bottom - h - gap),
        Side::Bottom | Side::Right => (work.right - w - gap, work.bottom - h - gap),
    };
    (x.min(work.right - w).max(work.left), y.min(work.bottom - h).max(work.top))
}

/// Direction the flyout slides in from (unit vector).
pub fn slide(side: Side) -> (f32, f32) {
    match side {
        Side::Bottom => (0.0, 1.0),
        Side::Top => (0.0, -1.0),
        Side::Left => (-1.0, 0.0),
        Side::Right => (1.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(l: i32, t: i32, rt: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rt, bottom: b }
    }
    const MON: RECT = r(0, 0, 1920, 1080);

    #[test]
    fn detects_each_taskbar_edge() {
        assert_eq!(side_of(MON, r(0, 0, 1920, 1040)), Side::Bottom);
        assert_eq!(side_of(MON, r(0, 40, 1920, 1080)), Side::Top);
        assert_eq!(side_of(MON, r(48, 0, 1920, 1080)), Side::Left);
        assert_eq!(side_of(MON, r(0, 0, 1872, 1080)), Side::Right);
        // A monitor that is not at the origin works the same way.
        assert_eq!(side_of(r(3840, 0, 5760, 1080), r(3840, 0, 5760, 1040)), Side::Bottom);
    }

    #[test]
    fn sits_in_the_corner_next_to_the_taskbar() {
        let work = r(0, 0, 1920, 1040);
        assert_eq!(place(work, Side::Bottom, 380, 300, 12), (1528, 728));
        assert_eq!(place(r(0, 40, 1920, 1080), Side::Top, 380, 300, 12), (1528, 52));
        assert_eq!(place(r(48, 0, 1920, 1080), Side::Left, 380, 300, 12), (60, 768));
        assert_eq!(place(r(0, 0, 1872, 1080), Side::Right, 380, 300, 12), (1480, 768));
    }

    #[test]
    fn never_leaves_the_work_area() {
        // Taller than the screen: pinned to the top edge instead of going off it.
        assert_eq!(place(r(0, 0, 1920, 1040), Side::Bottom, 380, 2000, 12), (1528, 0));
        // Narrower screen than the flyout.
        assert_eq!(place(r(100, 0, 300, 500), Side::Bottom, 380, 200, 12), (100, 288));
    }
}
