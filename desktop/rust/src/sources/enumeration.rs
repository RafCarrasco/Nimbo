//! Geometry and collection bookkeeping shared by the real Win32 enumerators.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
impl DesktopRect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Result<Self, &'static str> {
        if right <= left || bottom <= top {
            return Err("invalid desktop rectangle");
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }
    pub fn coordinates(self) -> [i32; 4] {
        [self.left, self.top, self.right, self.bottom]
    }
    pub fn covers(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }
}

pub struct NativeMonitor {
    pub handle: String,
    pub device_name: String,
    pub bounds: DesktopRect,
    pub work_area: DesktopRect,
    pub primary: bool,
}

#[cfg(any(windows, test))]
pub(super) struct Collector<T> {
    rows: Vec<T>,
    error: Option<&'static str>,
}
#[cfg(any(windows, test))]
impl<T> Collector<T> {
    pub(super) fn new() -> Self {
        Self {
            rows: Vec::new(),
            error: None,
        }
    }
    pub(super) fn push(&mut self, row: T) {
        self.rows.push(row);
    }
    pub(super) fn fail(&mut self, error: &'static str) {
        self.error = Some(error);
    }
    pub(super) fn finish(self, api_success: bool) -> Result<Vec<T>, &'static str> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if !api_success {
            return Err("native enumeration failed");
        }
        Ok(self.rows)
    }
}

#[cfg(any(windows, test))]
pub(super) fn wide_text(buffer: &[u16], count: Option<usize>) -> Result<String, &'static str> {
    let length = match count {
        Some(n) => n,
        None => buffer
            .iter()
            .position(|c| *c == 0)
            .ok_or("unterminated native text")?,
    };
    let text = buffer.get(..length).ok_or("native text exceeds buffer")?;
    Ok(String::from_utf16_lossy(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Additional characterization; these are not retroactive pre-implementation REDs.
    #[test]
    fn a_successful_empty_or_large_collection_is_not_a_failed_or_truncated_enumeration() {
        assert!(Collector::<u32>::new().finish(true).unwrap().is_empty());
        let mut rows = Collector::new();
        for n in 0..257 {
            rows.push(n);
        }
        assert_eq!(rows.finish(true).unwrap(), (0..257).collect::<Vec<_>>());
    }
    #[test]
    fn extreme_rectangles_and_invalid_utf16_do_not_overflow_or_read_past_buffers() {
        let all = DesktopRect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX).unwrap();
        assert!(all.covers(DesktopRect::new(-1, -1, 1, 1).unwrap()));
        assert_eq!(wide_text(&[0xd800, 0], None).unwrap(), "\u{fffd}");
        assert!(wide_text(&[65, 0], Some(usize::MAX)).is_err());
    }
    #[test]
    fn native_utf16_uses_only_the_reported_text_or_the_first_terminator() {
        let title = "Título 東京 🎮";
        let mut buffer: Vec<u16> = title.encode_utf16().collect();
        let count = buffer.len();
        buffer.extend([0, 88]);
        assert_eq!(wide_text(&buffer, Some(count)).unwrap(), title);
        assert_eq!(wide_text(&buffer, None).unwrap(), title);
        assert_eq!(wide_text(&buffer, Some(0)).unwrap(), "");
        assert!(wide_text(&buffer, Some(buffer.len() + 1)).is_err());
        assert!(wide_text(&[65, 66], None).is_err());
    }
    #[test]
    fn an_api_or_callback_error_never_turns_partial_enumeration_into_success() {
        let mut positive = Collector::new();
        positive.push(7);
        assert_eq!(positive.finish(true).unwrap(), vec![7]);
        let mut api_error = Collector::new();
        api_error.push(7);
        assert!(api_error.finish(false).is_err());
        let mut callback_error = Collector::new();
        callback_error.push(7);
        callback_error.fail("fixture callback failure");
        assert!(callback_error.finish(true).is_err());
    }
    #[test]
    fn monitor_coverage_is_inclusive_and_accepts_a_negative_virtual_origin() {
        let monitor = DesktopRect::new(-1920, -200, 0, 880).unwrap();
        assert!(monitor.covers(monitor));
        assert!(
            DesktopRect::new(-1921, -201, 1, 881)
                .unwrap()
                .covers(monitor)
        );
        for bounds in [
            [-1919, -200, 0, 880],
            [-1920, -199, 0, 880],
            [-1920, -200, -1, 880],
            [-1920, -200, 0, 879],
        ] {
            assert!(
                !DesktopRect::new(bounds[0], bounds[1], bounds[2], bounds[3])
                    .unwrap()
                    .covers(monitor)
            );
        }
    }
    #[test]
    fn geometry_accepts_negative_monitor_coordinates_but_not_invalid_extents() {
        assert_eq!(
            DesktopRect::new(-1920, -200, 0, 880).unwrap().coordinates(),
            [-1920, -200, 0, 880]
        );
        for args in [
            [0, 0, 0, 1080],
            [0, 0, 1920, 0],
            [1, 0, 0, 1080],
            [0, 1, 1920, 0],
        ] {
            assert!(DesktopRect::new(args[0], args[1], args[2], args[3]).is_err());
        }
    }
}
