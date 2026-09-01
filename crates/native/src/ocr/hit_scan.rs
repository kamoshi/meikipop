use crate::ocr::interface::{BoundingBox, Paragraph};

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

#[cfg(test)]
fn normalize_mouse_position(
    mouse_pos: (i32, i32),
    scan_geometry: (i32, i32, usize, usize),
) -> Result<(f64, f64), String> {
    let (mouse_x, mouse_y) = mouse_pos;
    let (mouse_off_x, mouse_off_y, img_w, img_h) = scan_geometry;
    if img_w == 0 || img_h == 0 {
        return Err("cannot normalize mouse position against an empty scan geometry".to_owned());
    }
    let relative_x = mouse_x - mouse_off_x;
    let relative_y = mouse_y - mouse_off_y;
    let norm_x = relative_x as f64 / img_w as f64;
    let norm_y = relative_y as f64 / img_h as f64;
    Ok((norm_x, norm_y))
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
    fn normalizes_mouse_position_against_scan_geometry() {
        assert_eq!(
            normalize_mouse_position((150, 100), (100, 50, 200, 100)).unwrap(),
            (0.25, 0.5)
        );
    }

    #[test]
    fn rejects_an_empty_scan_geometry() {
        assert!(normalize_mouse_position((0, 0), (0, 0, 0, 100)).is_err());
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

    #[test]
    fn empty_paragraph_list_returns_none() {
        assert_eq!(hit_scan(&[], 0.5, 0.5), None);
    }

    #[test]
    fn vertical_scan_returns_japanese_suffix() {
        let paragraph = paragraph_with_word("縦書き", BoundingBox::new(0.5, 0.5, 0.2, 0.6), true);

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), Some("書き".into()),);
    }

    #[test]
    fn horizontal_scan_at_trailing_edge_returns_last_character() {
        let paragraph = paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);

        // The box extends from x=0.2 through x=0.8.
        assert_eq!(hit_scan(&[paragraph], 0.8, 0.5), Some("語".into()),);
    }

    #[test]
    fn vertical_scan_at_trailing_edge_returns_last_character() {
        let paragraph = paragraph_with_word("縦書き", BoundingBox::new(0.5, 0.5, 0.2, 0.6), true);

        // The box extends from y=0.2 through y=0.8.
        assert_eq!(hit_scan(&[paragraph], 0.5, 0.8), Some("き".into()),);
    }

    #[test]
    fn empty_selected_word_is_skipped() {
        let paragraph = paragraph_with_word("", BoundingBox::new(0.5, 0.5, 0.4, 0.2), false);

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), None);
    }

    #[test]
    fn paragraph_without_words_returns_none() {
        let paragraph = Paragraph {
            full_text: "日本語".into(),
            words: Vec::new(),
            r#box: BoundingBox::new(0.5, 0.5, 0.6, 0.2),
            is_vertical: false,
        };

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), None);
    }

    #[test]
    fn zero_width_word_selects_its_first_character() {
        let paragraph = paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.0, 0.2), false);

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), Some("日本語".into()),);
    }

    #[test]
    fn zero_height_vertical_word_selects_its_first_character() {
        let paragraph = paragraph_with_word("縦書き", BoundingBox::new(0.5, 0.5, 0.2, 0.0), true);

        assert_eq!(hit_scan(&[paragraph], 0.5, 0.5), Some("縦書き".into()),);
    }

    #[test]
    fn scan_skips_paragraphs_that_do_not_contain_point() {
        let outside = paragraph_with_word("外", BoundingBox::new(0.1, 0.1, 0.1, 0.1), false);
        let inside = paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);

        assert_eq!(hit_scan(&[outside, inside], 0.5, 0.5), Some("本語".into()),);
    }

    #[test]
    fn first_matching_paragraph_wins() {
        let first = paragraph_with_word("第一", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);
        let second = paragraph_with_word("第二", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);

        assert_eq!(hit_scan(&[first, second], 0.2, 0.5), Some("第一".into()),);
    }

    fn paragraph_with_multiple_words() -> Paragraph {
        let first_box = BoundingBox::new(0.25, 0.5, 0.3, 0.2);
        let second_box = BoundingBox::new(0.65, 0.5, 0.4, 0.2);

        Paragraph {
            full_text: "日本語です".into(),
            words: vec![
                Word {
                    text: "日本".into(),
                    separator: String::new(),
                    r#box: first_box,
                },
                Word {
                    text: "語です".into(),
                    separator: String::new(),
                    r#box: second_box,
                },
            ],
            r#box: BoundingBox::new(0.475, 0.5, 0.75, 0.2),
            is_vertical: false,
        }
    }

    #[test]
    fn scan_calculates_offset_of_second_word() {
        let paragraph = paragraph_with_multiple_words();

        // Near the beginning of the second word.
        assert_eq!(hit_scan(&[paragraph], 0.46, 0.5), Some("語です".into()),);
    }

    #[test]
    fn gap_between_words_is_assigned_to_preceding_word() {
        let paragraph = paragraph_with_multiple_words();

        // First word ends at x=0.4 and second begins at x=0.45.
        assert_eq!(hit_scan(&[paragraph], 0.425, 0.5), Some("本語です".into()),);
    }

    #[test]
    fn inconsistent_full_text_does_not_produce_invalid_slice() {
        let mut paragraph =
            paragraph_with_word("日本語", BoundingBox::new(0.5, 0.5, 0.6, 0.2), false);
        paragraph.full_text = "日".into();

        assert_eq!(hit_scan(&[paragraph], 0.8, 0.5), None);
    }
}
