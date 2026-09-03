use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFString, CFType, CGRect};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowNumber,
};

use crate::platform::interface::CaptureGeometry;

#[derive(Clone, Debug)]
struct WindowSummary {
    id: u32,
    bounds: CaptureGeometry,
    alpha: f64,
}

/// One ordered WindowServer observation.
///
/// WindowServer exposes rectangular bounds and whole-window alpha, not a
/// per-pixel visibility region. Occlusion is therefore intentionally
/// conservative: any nontransparent window whose bounds contain the pointer
/// blocks a selected window below it.
#[derive(Clone, Debug)]
pub(crate) struct WindowListSnapshot {
    windows: Vec<WindowSummary>,
}

impl WindowListSnapshot {
    pub(crate) fn on_screen() -> Option<Self> {
        let options =
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
        let array = CGWindowListCopyWindowInfo(options, kCGNullWindowID)?;
        let windows = (0..array.count())
            .filter_map(|index| array_dictionary(&array, index))
            .filter_map(window_summary)
            .collect();
        Some(Self { windows })
    }

    pub(crate) fn target_is_frontmost_at_point(&self, window_id: u32, point: (i32, i32)) -> bool {
        self.frontmost_window_at_point(point) == Some(window_id)
    }

    fn frontmost_window_at_point(&self, point: (i32, i32)) -> Option<u32> {
        self.windows
            .iter()
            .find(|window| window.alpha > 0.0 && window.bounds.contains(point))
            .map(|window| window.id)
    }
}

pub(crate) fn query_window_geometry(window_id: u32) -> Option<CaptureGeometry> {
    let array = CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, window_id)?;
    (0..array.count())
        .filter_map(|index| array_dictionary(&array, index))
        .find(|dictionary| dictionary_window_id(dictionary) == Some(window_id))
        .and_then(geometry_from_window_dictionary)
}

fn window_summary(dictionary: &CFDictionary) -> Option<WindowSummary> {
    let id = dictionary_window_id(dictionary)?;
    let bounds = geometry_from_window_dictionary(dictionary)?;
    // SAFETY: CoreGraphics exports these process-lifetime CFString constants.
    let alpha = dictionary_number(dictionary, unsafe { kCGWindowAlpha })
        .and_then(CFNumber::as_f64)
        .unwrap_or(1.0);
    Some(WindowSummary { id, bounds, alpha })
}

fn dictionary_window_id(dictionary: &CFDictionary) -> Option<u32> {
    // SAFETY: CoreGraphics exports this process-lifetime CFString constant.
    dictionary_number(dictionary, unsafe { kCGWindowNumber })?
        .as_i64()?
        .try_into()
        .ok()
}

fn array_dictionary(array: &CFArray, index: isize) -> Option<&CFDictionary> {
    if index < 0 || index >= array.count() {
        return None;
    }
    // SAFETY: The index was bounds-checked and the array owns the returned value.
    let value = unsafe { array.value_at_index(index) };
    if value.is_null() {
        return None;
    }
    // SAFETY: CFArray values are CFType-compatible; downcast_ref validates the type ID.
    let value = unsafe { &*value.cast::<CFType>() };
    value.downcast_ref::<CFDictionary>()
}

fn dictionary_number<'a>(dictionary: &'a CFDictionary, key: &CFString) -> Option<&'a CFNumber> {
    dictionary_value(dictionary, key)?.downcast_ref::<CFNumber>()
}

fn dictionary_value<'a>(dictionary: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    let mut value = std::ptr::null();
    // SAFETY: Both pointers refer to live Core Foundation objects and `value` is writable.
    if !unsafe { dictionary.value_if_present(std::ptr::from_ref(key).cast(), &mut value) }
        || value.is_null()
    {
        return None;
    }
    // SAFETY: A successful CFDictionary lookup returns a borrowed CFType value.
    Some(unsafe { &*value.cast::<CFType>() })
}

fn geometry_from_window_dictionary(dictionary: &CFDictionary) -> Option<CaptureGeometry> {
    // SAFETY: CoreGraphics exports this process-lifetime CFString constant.
    let bounds_dictionary =
        dictionary_value(dictionary, unsafe { kCGWindowBounds })?.downcast_ref::<CFDictionary>()?;

    let mut rect = CGRect::default();
    // SAFETY: `bounds_dictionary` is a CGRect dictionary and `rect` is writable.
    if unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds_dictionary), &mut rect) }
        && rect.size.width > 0.0
        && rect.size.height > 0.0
    {
        return Some(CaptureGeometry {
            left: rect.origin.x.round() as i32,
            top: rect.origin.y.round() as i32,
            width: rect.size.width.round() as usize,
            height: rect.size.height.round() as usize,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{WindowListSnapshot, WindowSummary};
    use crate::platform::interface::CaptureGeometry;

    fn window(id: u32, left: i32, top: i32, width: usize, height: usize) -> WindowSummary {
        WindowSummary {
            id,
            bounds: CaptureGeometry {
                left,
                top,
                width,
                height,
            },
            alpha: 1.0,
        }
    }

    #[test]
    fn frontmost_window_uses_window_server_order() {
        let snapshot = WindowListSnapshot {
            windows: vec![window(20, 100, 100, 300, 300), window(10, 0, 0, 500, 500)],
        };

        assert!(snapshot.target_is_frontmost_at_point(20, (150, 150)));
        assert!(!snapshot.target_is_frontmost_at_point(10, (150, 150)));
        assert!(snapshot.target_is_frontmost_at_point(10, (50, 50)));
        assert!(!snapshot.target_is_frontmost_at_point(20, (50, 50)));
    }

    #[test]
    fn frontmost_window_ignores_transparent_and_noncontaining_windows() {
        let mut transparent = window(30, 0, 0, 500, 500);
        transparent.alpha = 0.0;
        let snapshot = WindowListSnapshot {
            windows: vec![
                transparent,
                window(20, 300, 300, 100, 100),
                window(10, 0, 0, 500, 500),
            ],
        };

        assert!(snapshot.target_is_frontmost_at_point(10, (150, 150)));
    }
}
