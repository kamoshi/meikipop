use crate::ocr::interface::{paragraph_from_python, BoundingBox, Paragraph};
use pyo3::prelude::*;

fn is_in_box(point: (f64, f64), r#box: Option<&BoundingBox>) -> bool {
    let Some(r#box) = r#box else {
        return false;
    };

    let (px, py) = point;
    let (half_w, half_h) = (r#box.width / 2.0, r#box.height / 2.0);

    (r#box.center_x - half_w <= px && px <= r#box.center_x + half_w)
        && (r#box.center_y - half_h <= py && py <= r#box.center_y + half_h)
}

fn is_in_box_ex(
    point: (f64, f64),
    box_before: Option<&BoundingBox>,
    r#box: Option<&BoundingBox>,
    box_after: Option<&BoundingBox>,
    is_vertical_flag: bool,
) -> bool {
    let Some(r#box) = r#box else {
        return false;
    };

    let mut left = r#box.center_x - r#box.width / 2.0;
    let mut right = r#box.center_x + r#box.width / 2.0;
    let mut top = r#box.center_y - r#box.height / 2.0;
    let mut bottom = r#box.center_y + r#box.height / 2.0;

    if !is_vertical_flag && let Some(box_before) = box_before {
        left = f64::min(left, box_before.center_x + box_before.width / 2.0);
    }
    if !is_vertical_flag && let Some(box_after) = box_after {
        right = f64::max(right, box_after.center_x - box_after.width / 2.0);
    }
    if is_vertical_flag && let Some(box_before) = box_before {
        top = f64::min(top, box_before.center_y + box_before.height / 2.0);
    }
    if is_vertical_flag && let Some(box_after) = box_after {
        bottom = f64::max(bottom, box_after.center_y - box_after.height / 2.0);
    }

    let (px, py) = point;

    (left <= px && px <= right) && (top <= py && py <= bottom)
}

pub fn hit_scan(paragraphs: &[Paragraph], norm_x: f64, norm_y: f64) -> Option<String> {
    let mut hit_scan_result = None;
    let mut lookup_string = None;

    for para in paragraphs {
        if !is_in_box((norm_x, norm_y), Some(&para.r#box)) {
            continue;
        }

        let mut target_word = None;
        let words = &para.words;
        for (i, word) in words.iter().enumerate() {
            let box_before = if i > 0 {
                Some(&words[i - 1].r#box)
            } else {
                None
            };

            let box_after = if i < words.len() - 1 {
                Some(&words[i + 1].r#box)
            } else {
                None
            };

            if is_in_box_ex(
                (norm_x, norm_y),
                box_before,
                Some(&word.r#box),
                box_after,
                para.is_vertical,
            ) {
                target_word = Some(word);
                break;
            }
        }

        let Some(target_word) = target_word else {
            continue;
        };

        let mut char_offset = 0;

        if para.is_vertical {
            if target_word.r#box.height > 0.0 {
                let top_edge = target_word.r#box.center_y - (target_word.r#box.height / 2.0);
                let relative_y_in_box = norm_y - top_edge;
                let char_percent = (relative_y_in_box / target_word.r#box.height).clamp(0.0, 1.0);
                char_offset = (char_percent * target_word.text.chars().count() as f64) as usize;
            }
        } else {
            // Horizontal
            if target_word.r#box.width > 0.0 {
                let left_edge = target_word.r#box.center_x - (target_word.r#box.width / 2.0);
                let relative_x_in_box = norm_x - left_edge;
                let char_percent = (relative_x_in_box / target_word.r#box.width).clamp(0.0, 1.0);
                char_offset = (char_percent * target_word.text.chars().count() as f64) as usize;
            }
        }

        let char_count = target_word.text.chars().count();
        if char_count == 0 {
            continue;
        }
        char_offset = usize::min(char_offset, char_count - 1);

        let mut word_start_index = 0;
        for word in &para.words {
            if word == target_word {
                break;
            }
            word_start_index += word.text.chars().count()
        }

        let final_char_index = word_start_index + char_offset;
        let full_text = &para.full_text;

        if final_char_index >= full_text.chars().count() {
            continue;
        }

        let Some((byte_index, character)) = full_text.char_indices().nth(final_char_index) else {
            continue;
        };
        let lookup_string = full_text[byte_index..].to_owned();
        // this may be interesting for debugging, but only lookup_string is really relevant
        hit_scan_result = Some((full_text, final_char_index, character, lookup_string));
        break;
    }

    if let Some(hit_scan_result) = hit_scan_result {
        lookup_string = Some(hit_scan_result.3);
    }
    //    truncated_text = (text[:40] + '...') if len(text) > 40 else text
    //     config.user_log(f"  -> Looking up '{char}' at pos {char_pos} in text: \"{truncated_text}\"")
    // else:
    //     config.user_log("hit scan unsuccessful")

    lookup_string
}

#[pyfunction(name = "hit_scan")]
fn py_hit_scan(
    paragraphs: &Bound<'_, PyAny>,
    norm_x: f64,
    norm_y: f64,
) -> PyResult<Option<String>> {
    let mut rust_paragraphs = Vec::new();
    for paragraph in paragraphs.try_iter()? {
        rust_paragraphs.push(paragraph_from_python(&paragraph?)?);
    }

    Ok(hit_scan(&rust_paragraphs, norm_x, norm_y))
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(py_hit_scan, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::interface::Word;

    #[test]
    fn point_inside_box_is_detected() {
        let bounding_box = BoundingBox::new(0.5, 0.5, 0.4, 0.2);

        assert!(is_in_box((0.5, 0.5), Some(&bounding_box)));
        assert!(is_in_box((0.3, 0.5), Some(&bounding_box)));
        assert!(!is_in_box((0.2, 0.5), Some(&bounding_box)));
    }

    #[test]
    fn missing_box_never_contains_point() {
        assert!(!is_in_box((0.5, 0.5), None));
    }

    #[test]
    fn horizontal_extended_box_includes_gap_before_word() {
        let before = BoundingBox::new(0.2, 0.5, 0.2, 0.2);
        let current = BoundingBox::new(0.5, 0.5, 0.2, 0.2);

        // Current begins at 0.4; previous ends at 0.3.
        assert!(!is_in_box((0.35, 0.5), Some(&current)));

        assert!(is_in_box_ex(
            (0.35, 0.5),
            Some(&before),
            Some(&current),
            None,
            false,
        ));
    }

    #[test]
    fn vertical_extended_box_includes_gap_before_word() {
        let before = BoundingBox::new(0.5, 0.2, 0.2, 0.2);
        let current = BoundingBox::new(0.5, 0.5, 0.2, 0.2);

        assert!(!is_in_box((0.5, 0.35), Some(&current)));

        assert!(is_in_box_ex(
            (0.5, 0.35),
            Some(&before),
            Some(&current),
            None,
            true,
        ));
    }

    fn paragraph_with_word(text: &str, bounding_box: BoundingBox, is_vertical: bool) -> Paragraph {
        Paragraph {
            full_text: text.into(),
            words: vec![Word {
                text: text.into(),
                separator: String::new(),
                r#box: bounding_box.clone(),
            }],
            r#box: bounding_box,
            is_vertical,
        }
    }

    #[test]
    fn horizontal_scan_returns_japanese_suffix() {
        let paragraph = paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), Some("本語".into()),);
    }

    #[test]
    fn scan_outside_paragraph_returns_none() {
        let paragraph = paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.4, 0.2), false);

        assert_eq!(hit_scan(&[paragraph], 0.1, 0.1), None);
    }
}
