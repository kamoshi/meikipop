use crate::ocr::interface::{paragraph_from_python, BoundingBox, Paragraph};
use pyo3::prelude::*;

pub const FURIGANA_VERTICAL_WIDTH_THRESHOLD: f64 = 0.65;
pub const FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD: f64 = 0.65;

/// Creates a single BoundingBox that encompasses all provided boxes.
fn merge_bounding_boxes(boxes: &[BoundingBox]) -> BoundingBox {
    if boxes.is_empty() {
        return BoundingBox::new(0.0, 0.0, 0.0, 0.0);
    }

    #[rustfmt::skip]
    let min_x = boxes.iter().map(|b| b.center_x - b.width / 2.0).reduce(f64::min).expect("boxes was checked to be non-empty");
    #[rustfmt::skip]
    let max_x = boxes.iter().map(|b| b.center_x + b.width / 2.0).reduce(f64::max).expect("boxes was checked to be non-empty");
    #[rustfmt::skip]
    let min_y = boxes.iter().map(|b| b.center_y - b.height / 2.0).reduce(f64::min).expect("boxes was checked to be non-empty");
    #[rustfmt::skip]
    let max_y = boxes.iter().map(|b| b.center_y + b.height / 2.0).reduce(f64::max).expect("boxes was checked to be non-empty");

    let width = max_x - min_x;
    let height = max_y - min_y;
    let center_x = min_x + width / 2.0;
    let center_y = min_y + height / 2.0;

    BoundingBox {
        center_x,
        center_y,
        width,
        height,
    }
}

/// Determines if two lines are close enough to be considered part of the same paragraph.
/// This uses heuristics to be tolerant of small OCR inaccuracies.
fn are_lines_adjacent(line1: &Paragraph, line2: &Paragraph) -> bool {
    let (b1, b2) = (&line1.r#box, &line2.r#box);
    let is_vertical = line1.is_vertical;

    if is_vertical {
        // For vertical text (read R->L), lines should have significant y-overlap
        // and be close on the x-axis.
        let y_overlap = f64::max(
            0.0,
            f64::min(b1.center_y + b1.height / 2.0, b2.center_y + b2.height / 2.0)
                - f64::max(b1.center_y - b1.height / 2.0, b2.center_y - b2.height / 2.0),
        );
        let has_enough_overlap = y_overlap > (f64::min(b1.height, b2.height) * 0.5);

        // Check horizontal distance between line centers. Allow up to 1.9x the width of a line for spacing.
        let horizontal_distance_ok =
            f64::abs(b1.center_x - b2.center_x) < 1.9 * f64::max(b1.width, b2.width);

        has_enough_overlap && horizontal_distance_ok
    } else {
        // For horizontal text (read T->B), lines should have significant x-overlap
        // and be close on the y-axis.
        let x_overlap = f64::max(
            0.0,
            f64::min(b1.center_x + b1.width / 2.0, b2.center_x + b2.width / 2.0)
                - f64::max(b1.center_x - b1.width / 2.0, b2.center_x - b2.width / 2.0),
        );
        let has_enough_overlap = x_overlap > (f64::min(b1.width, b2.width) * 0.5);

        // Check vertical distance. Allow up to 1.9x the height for line spacing.
        let vertical_distance_ok =
            f64::abs(b1.center_y - b2.center_y) < 1.9 * f64::max(b1.height, b2.height);

        has_enough_overlap && vertical_distance_ok
    }
}

/// Merges a list of single-line Paragraphs into one cohesive Paragraph.
fn merge_lines_into_paragraph(mut lines: Vec<Paragraph>) -> Option<Paragraph> {
    if lines.is_empty() {
        return None;
    }

    let is_vertical = lines[0].is_vertical;

    if is_vertical {
        // Vertical text is read right-to-left
        lines.sort_by(|a, b| b.r#box.center_x.total_cmp(&a.r#box.center_x));
    } else {
        // Horizontal text is read top-to-bottom
        lines.sort_by(|a, b| a.r#box.center_y.total_cmp(&b.r#box.center_y));
    }

    let mut all_words = vec![];
    let mut full_text_parts = vec![];
    let mut all_boxes = vec![];

    for line in lines {
        all_words.extend(line.words);
        full_text_parts.push(line.full_text);
        all_boxes.push(line.r#box);
    }

    let full_text = full_text_parts.concat();
    let merged_box = merge_bounding_boxes(&all_boxes);

    Some(Paragraph {
        full_text,
        words: all_words,
        r#box: merged_box,
        is_vertical,
    })
}

fn median(values: &[f64]) -> f64 {
    assert!(
        !values.is_empty(),
        "cannot calculate the median of an empty slice"
    );

    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);

    let middle = sorted.len() / 2;

    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    }
}

/// Separates lines into main text and furigana based on their size.
///
/// Furigana is much smaller than the main text. This function calculates the
/// median size (width for vertical, height for horizontal) and classifies
/// any significantly smaller lines as furigana.
///
/// Returns:
///     A tuple containing two lists: (main_lines, furigana_lines).
fn classify_lines_by_size(lines: Vec<Paragraph>) -> (Vec<Paragraph>, Vec<Paragraph>) {
    let mut main_lines = vec![];
    let mut furigana_lines = vec![];

    let (vertical_lines, horizontal_lines): (Vec<_>, Vec<_>) = lines
        .into_iter()
        .partition(|paragraph| paragraph.is_vertical);

    if !vertical_lines.is_empty() {
        // For vertical text, furigana lines are much thinner (smaller width)
        let widths: Vec<_> = vertical_lines.iter().map(|p| p.r#box.width).collect();
        if widths.len() > 1 {
            let median_width = median(&widths);
            let threshold = median_width * FURIGANA_VERTICAL_WIDTH_THRESHOLD;
            for line in vertical_lines {
                if line.r#box.width < threshold {
                    furigana_lines.push(line);
                } else {
                    main_lines.push(line);
                }
            }
        } else {
            // If there's only one line, it's main text by definition
            main_lines.extend(vertical_lines);
        }
    }

    if !horizontal_lines.is_empty() {
        // For horizontal text, furigana lines are much shorter (smaller height)
        let heights: Vec<_> = horizontal_lines.iter().map(|p| p.r#box.height).collect();
        if heights.len() > 1 {
            let median_height = median(&heights);
            let threshold = median_height * FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD;
            for line in horizontal_lines {
                if line.r#box.height < threshold {
                    furigana_lines.push(line);
                } else {
                    main_lines.push(line);
                }
            }
        } else {
            // If there's only one line, it's main text by definition
            main_lines.extend(horizontal_lines);
        }
    }

    (main_lines, furigana_lines)
}

/// Takes a flat list of single-line Paragraphs and groups them into
/// multi-line Paragraphs based on proximity and orientation.
///
/// This version includes a preprocessing step to identify and separate
/// furigana, which is then excluded from the main paragraph grouping logic.
pub fn group_lines_into_paragraphs(lines: Vec<Paragraph>) -> Vec<Paragraph> {
    if lines.is_empty() {
        return vec![];
    }

    // Classify lines into main text and furigana
    let (main_lines, furigana_lines) = classify_lines_by_size(lines);
    // logger.debug(f"Identified and separated {len(furigana_lines)} furigana lines.")

    // Separate main lines by orientation for processing
    let (vertical_lines, horizontal_lines): (Vec<_>, Vec<_>) = main_lines
        .into_iter()
        .partition(|paragraph| paragraph.is_vertical);

    let mut processed_paragraphs = vec![];

    for mut line_set in [vertical_lines, horizontal_lines] {
        while !line_set.is_empty() {
            let mut current_group = vec![line_set.remove(0)];
            let mut i = 0;

            while i < line_set.len() {
                let line_to_check = &line_set[i];
                let is_adjacent_to_group = current_group
                    .iter()
                    .any(|grouped_line| are_lines_adjacent(grouped_line, line_to_check));

                if is_adjacent_to_group {
                    current_group.push(line_set.remove(i));
                    // Restart check from the beginning since the group has grown
                    i = 0;
                } else {
                    i += 1;
                }
            }

            if let Some(merged_para) = merge_lines_into_paragraph(current_group) {
                processed_paragraphs.push(merged_para);
            }
        }
    }

    // Add the isolated furigana lines back as their own separate paragraphs
    let mut final_paragraphs = processed_paragraphs;
    final_paragraphs.extend(furigana_lines);

    // logger.debug(f"Regrouped {len(lines)} raw OCR lines into {len(final_paragraphs)} paragraphs.")
    final_paragraphs
}

#[pyfunction(name = "group_lines_into_paragraphs")]
fn py_group_lines_into_paragraphs(
    py: Python<'_>,
    lines: &Bound<'_, PyAny>,
) -> PyResult<Vec<Py<PyAny>>> {
    let mut rust_lines = Vec::new();
    for line in lines.try_iter()? {
        rust_lines.push(paragraph_from_python(&line?)?);
    }

    let interface = py.import("meikipop.ocr.interface")?;
    let bounding_box_class = interface.getattr("BoundingBox")?;
    let word_class = interface.getattr("Word")?;
    let paragraph_class = interface.getattr("Paragraph")?;
    let mut results = Vec::new();

    for paragraph in group_lines_into_paragraphs(rust_lines) {
        let bounding_box = bounding_box_class.call1((
            paragraph.r#box.center_x,
            paragraph.r#box.center_y,
            paragraph.r#box.width,
            paragraph.r#box.height,
        ))?;

        let mut words = Vec::new();
        for word in paragraph.words {
            let word_box = bounding_box_class.call1((
                word.r#box.center_x,
                word.r#box.center_y,
                word.r#box.width,
                word.r#box.height,
            ))?;
            words.push(
                word_class
                    .call1((word.text, word.separator, word_box))?
                    .unbind(),
            );
        }

        results.push(
            paragraph_class
                .call1((
                    paragraph.full_text,
                    words,
                    bounding_box,
                    paragraph.is_vertical,
                ))?
                .unbind(),
        );
    }

    Ok(results)
}

pub fn register_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "FURIGANA_VERTICAL_WIDTH_THRESHOLD",
        FURIGANA_VERTICAL_WIDTH_THRESHOLD,
    )?;
    module.add(
        "FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD",
        FURIGANA_HORIZONTAL_HEIGHT_THRESHOLD,
    )?;
    module.add_function(wrap_pyfunction!(py_group_lines_into_paragraphs, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::interface::Word;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-10,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn merging_no_boxes_returns_a_zero_box() {
        assert_eq!(
            merge_bounding_boxes(&[]),
            BoundingBox::new(0.0, 0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn merging_boxes_encloses_all_boxes() {
        let boxes = [
            BoundingBox::new(0.2, 0.3, 0.2, 0.2),
            BoundingBox::new(0.5, 0.4, 0.4, 0.2),
        ];

        let result = merge_bounding_boxes(&boxes);

        assert_close(result.center_x, 0.4);
        assert_close(result.center_y, 0.35);
        assert_close(result.width, 0.6);
        assert_close(result.height, 0.3);
    }

    #[test]
    fn horizontally_oriented_nearby_lines_are_adjacent() {
        let first = Paragraph::new(BoundingBox::new(0.5, 0.20, 0.6, 0.1), false);
        let second = Paragraph::new(BoundingBox::new(0.5, 0.32, 0.6, 0.1), false);

        assert!(are_lines_adjacent(&first, &second));
    }

    #[test]
    fn horizontally_oriented_distant_lines_are_not_adjacent() {
        let first = Paragraph::new(BoundingBox::new(0.5, 0.20, 0.6, 0.1), false);
        let second = Paragraph::new(BoundingBox::new(0.5, 0.60, 0.6, 0.1), false);

        assert!(!are_lines_adjacent(&first, &second));
    }

    #[test]
    fn vertically_oriented_nearby_lines_are_adjacent() {
        let first = Paragraph::new(BoundingBox::new(0.70, 0.5, 0.1, 0.6), true);
        let second = Paragraph::new(BoundingBox::new(0.58, 0.5, 0.1, 0.6), true);

        assert!(are_lines_adjacent(&first, &second));
    }

    #[test]
    fn merging_no_lines_returns_none() {
        assert_eq!(merge_lines_into_paragraph(Vec::new()), None);
    }

    fn line(text: &str, r#box: BoundingBox, is_vertical: bool) -> Paragraph {
        Paragraph {
            full_text: text.into(),
            words: vec![Word {
                text: text.into(),
                separator: String::new(),
                r#box: r#box.clone(),
            }],
            r#box,
            is_vertical,
        }
    }

    #[test]
    fn merging_horizontal_lines_orders_them_top_to_bottom() {
        let bottom = line("下", BoundingBox::new(0.5, 0.6, 0.4, 0.1), false);
        let top = line("上", BoundingBox::new(0.5, 0.2, 0.4, 0.1), false);

        // Deliberately pass them in the wrong reading order.
        let result = merge_lines_into_paragraph(vec![bottom, top]).unwrap();

        assert_eq!(result.full_text, "上下");
        assert_eq!(result.words[0].text, "上");
        assert_eq!(result.words[1].text, "下");
        assert!(!result.is_vertical);

        assert_close(result.r#box.center_x, 0.5);
        assert_close(result.r#box.center_y, 0.4);
        assert_close(result.r#box.width, 0.4);
        assert_close(result.r#box.height, 0.5);
    }

    #[test]
    fn merging_vertical_lines_orders_them_right_to_left() {
        let left = line("左", BoundingBox::new(0.3, 0.5, 0.1, 0.4), true);
        let right = line("右", BoundingBox::new(0.7, 0.5, 0.1, 0.4), true);

        let result = merge_lines_into_paragraph(vec![left, right]).unwrap();

        assert_eq!(result.full_text, "右左");
        assert_eq!(result.words[0].text, "右");
        assert_eq!(result.words[1].text, "左");
        assert!(result.is_vertical);

        assert_close(result.r#box.center_x, 0.5);
        assert_close(result.r#box.center_y, 0.5);
        assert_close(result.r#box.width, 0.5);
        assert_close(result.r#box.height, 0.4);
    }

    #[test]
    fn median_returns_middle_value_for_an_odd_length() {
        assert_close(median(&[3.0, 1.0, 2.0]), 2.0);
    }

    #[test]
    fn median_averages_middle_values_for_an_even_length() {
        assert_close(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    #[should_panic(expected = "cannot calculate the median of an empty slice")]
    fn median_rejects_empty_input() {
        median(&[]);
    }

    #[test]
    fn classifies_thin_vertical_lines_as_furigana() {
        let normal1 = line("本文一", BoundingBox::new(0.7, 0.5, 0.4, 0.8), true);
        let furigana = line("ふり", BoundingBox::new(0.5, 0.5, 0.1, 0.8), true);
        let normal2 = line("本文二", BoundingBox::new(0.3, 0.5, 0.4, 0.8), true);

        let (main, furigana) = classify_lines_by_size(vec![normal1, furigana, normal2]);

        assert_eq!(main.len(), 2);
        assert_eq!(furigana.len(), 1);
        assert_eq!(furigana[0].full_text, "ふり");
    }

    #[test]
    fn grouping_connects_transitively_adjacent_lines() {
        let first = line("一", BoundingBox::new(0.5, 0.10, 0.4, 0.1), false);
        let second = line("二", BoundingBox::new(0.5, 0.27, 0.4, 0.1), false);
        let third = line("三", BoundingBox::new(0.5, 0.44, 0.4, 0.1), false);

        assert!(!are_lines_adjacent(&first, &third));

        let result = group_lines_into_paragraphs(vec![third, first, second]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].full_text, "一二三");
        assert_eq!(result[0].words.len(), 3);
    }

    #[test]
    fn grouping_keeps_distant_lines_separate() {
        let first = line("上", BoundingBox::new(0.5, 0.1, 0.4, 0.1), false);
        let second = line("下", BoundingBox::new(0.5, 0.8, 0.4, 0.1), false);

        let result = group_lines_into_paragraphs(vec![first, second]);

        assert_eq!(result.len(), 2);
    }
}
