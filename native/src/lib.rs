use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes};

pub mod dictionary;
pub mod ocr;
mod wayland_capture;
use wayland_capture::WaylandCapture;

fn crop_bgra_impl(
    frame: &[u8],
    full_width: usize,
    full_height: usize,
    crop_left: i64,
    crop_top: i64,
    crop_width: i64,
    crop_height: i64,
) -> Result<(Vec<u8>, usize, usize), &'static str> {
    if full_width == 0 || full_height == 0 {
        return Err("frame dimensions must be greater than zero");
    }

    let stride = full_width
        .checked_mul(4)
        .ok_or("frame dimensions are too large")?;
    let required_len = stride
        .checked_mul(full_height)
        .ok_or("frame dimensions are too large")?;
    if frame.len() < required_len {
        return Err("BGRA buffer is smaller than its declared dimensions");
    }

    // Use a wider signed type so adding user-provided coordinates cannot
    // overflow. These clamps intentionally match the original Python shim.
    let full_width = full_width as i128;
    let full_height = full_height as i128;
    let requested_right = crop_left as i128 + crop_width as i128;
    let requested_bottom = crop_top as i128 + crop_height as i128;

    let left = (crop_left as i128).clamp(0, full_width - 1);
    let top = (crop_top as i128).clamp(0, full_height - 1);
    let right = requested_right.min(full_width).max(left + 1);
    let bottom = requested_bottom.min(full_height).max(top + 1);

    let output_width = (right - left) as usize;
    let output_height = (bottom - top) as usize;
    let output_stride = output_width * 4;
    let mut output = vec![0; output_stride * output_height];

    let left_bytes = left as usize * 4;
    for output_y in 0..output_height {
        let source_y = top as usize + output_y;
        let source_start = source_y * stride + left_bytes;
        let source_end = source_start + output_stride;
        let destination_start = output_y * output_stride;
        output[destination_start..destination_start + output_stride]
            .copy_from_slice(&frame[source_start..source_end]);
    }

    Ok((output, output_width, output_height))
}

/// Crop a tightly packed BGRA frame using MeikiPop's existing edge semantics.
#[pyfunction]
fn crop_bgra<'py>(
    py: Python<'py>,
    frame: &Bound<'py, PyAny>,
    full_width: usize,
    full_height: usize,
    rect: (i64, i64, i64, i64),
) -> PyResult<(Bound<'py, PyByteArray>, usize, usize)> {
    let slice: &[u8] = if let Ok(bytes) = frame.cast::<PyBytes>() {
        bytes.as_bytes()
    } else if let Ok(bytearray) = frame.cast::<PyByteArray>() {
        unsafe { bytearray.as_bytes() }
    } else {
        return Err(PyValueError::new_err(
            "frame must be a bytes or bytearray object",
        ));
    };

    let (left, top, width, height) = rect;
    let (cropped, width, height) =
        crop_bgra_impl(slice, full_width, full_height, left, top, width, height)
            .map_err(PyValueError::new_err)?;

    Ok((PyByteArray::new(py, &cropped), width, height))
}

/// Minimal proof that a Rust extension can be called from MeikiPop's Python.
#[pyfunction]
fn backend_name() -> &'static str {
    "pyo3"
}

#[pymodule]
fn meikipop_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    let dictionary_module = PyModule::new(py, "dictionary")?;
    let deconjugator_module = PyModule::new(py, "deconjugator")?;
    let lookup_module = PyModule::new(py, "lookup")?;
    let ocr_module = PyModule::new(py, "ocr")?;
    let hit_scan_module = PyModule::new(py, "hit_scan")?;
    let providers_module = PyModule::new(py, "providers")?;
    let dummy_module = PyModule::new(py, "dummy")?;
    let meikiocr_module = PyModule::new(py, "meikiocr")?;
    let postprocessing_module = PyModule::new(py, "postprocessing")?;

    deconjugator_module.add(
        "MAX_DECONJ_ITERATIONS",
        dictionary::deconjugator::MAX_DECONJ_ITERATIONS,
    )?;
    deconjugator_module.add_class::<dictionary::deconjugator::Form>()?;
    deconjugator_module.add_class::<dictionary::deconjugator::Deconjugator>()?;
    dictionary::lookup::register_python(&lookup_module)?;
    dictionary_module.add_submodule(&deconjugator_module)?;
    dictionary_module.add_submodule(&lookup_module)?;
    module.add_submodule(&dictionary_module)?;

    ocr::hit_scan::register_python(&hit_scan_module)?;
    ocr_module.add_submodule(&hit_scan_module)?;
    ocr::providers::dummy::provider::register_python(&dummy_module)?;
    ocr::providers::meikiocr::ocr::register_python(&meikiocr_module)?;
    ocr::providers::meikiocr::provider::register_python(&meikiocr_module)?;
    ocr::providers::postprocessing::register_python(&postprocessing_module)?;
    providers_module.add_submodule(&dummy_module)?;
    providers_module.add_submodule(&meikiocr_module)?;
    providers_module.add_submodule(&postprocessing_module)?;
    ocr_module.add_submodule(&providers_module)?;
    module.add_submodule(&ocr_module)?;

    // PyModule::add_submodule exposes attributes, while import statements also
    // require the fully-qualified modules to be present in sys.modules.
    let modules = py.import("sys")?.getattr("modules")?;
    modules.set_item("meikipop_native.dictionary", &dictionary_module)?;
    modules.set_item(
        "meikipop_native.dictionary.deconjugator",
        &deconjugator_module,
    )?;
    modules.set_item("meikipop_native.dictionary.lookup", &lookup_module)?;
    modules.set_item("meikipop_native.ocr", &ocr_module)?;
    modules.set_item("meikipop_native.ocr.hit_scan", &hit_scan_module)?;
    modules.set_item("meikipop_native.ocr.providers", &providers_module)?;
    modules.set_item("meikipop_native.ocr.providers.dummy", &dummy_module)?;
    modules.set_item("meikipop_native.ocr.providers.meikiocr", &meikiocr_module)?;
    modules.set_item(
        "meikipop_native.ocr.providers.postprocessing",
        &postprocessing_module,
    )?;

    module.add_class::<WaylandCapture>()?;
    module.add_function(wrap_pyfunction!(backend_name, module)?)?;
    module.add_function(wrap_pyfunction!(crop_bgra, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::crop_bgra_impl;

    #[test]
    fn crops_rows_from_a_bgra_frame() {
        let frame: Vec<u8> = (0..24).collect();
        let (cropped, width, height) = crop_bgra_impl(&frame, 3, 2, 1, 0, 2, 2).unwrap();

        assert_eq!((width, height), (2, 2));
        assert_eq!(
            cropped,
            [4, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 20, 21, 22, 23]
        );
    }

    #[test]
    fn clamps_a_crop_to_the_frame() {
        let frame: Vec<u8> = (0..24).collect();
        let (cropped, width, height) = crop_bgra_impl(&frame, 3, 2, -2, -1, 3, 2).unwrap();

        assert_eq!((width, height), (1, 1));
        assert_eq!(cropped, [0, 1, 2, 3]);
    }

    #[test]
    fn rejects_an_incomplete_frame() {
        let error = crop_bgra_impl(&[0; 7], 2, 1, 0, 0, 1, 1).unwrap_err();
        assert_eq!(error, "BGRA buffer is smaller than its declared dimensions");
    }
}
