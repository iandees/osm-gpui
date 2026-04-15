//! Modal dialog to add a user-defined custom imagery layer, plus the validation
//! helpers the dialog and its tests share.

use crate::custom_imagery_store::CustomImageryEntry;

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    NameEmpty,
    TemplateEmpty,
    TemplateMissingPlaceholder,
    TemplateYAndMinusY,
    MinZoomInvalid,
    MaxZoomInvalid,
    MinZoomAboveMax,
}

/// Validate raw form fields (already trimmed by the caller) and return a
/// normalised `CustomImageryEntry` on success.
pub fn validate(
    name: &str,
    url_template: &str,
    min_zoom_raw: &str,
    max_zoom_raw: &str,
) -> Result<CustomImageryEntry, ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError::NameEmpty);
    }
    let template = url_template.trim();
    if template.is_empty() {
        return Err(ValidationError::TemplateEmpty);
    }
    let has_z = template.contains("{z}");
    let has_x = template.contains("{x}");
    let has_y = template.contains("{y}");
    let has_minus_y = template.contains("{-y}");
    if !has_z || !has_x || (!has_y && !has_minus_y) {
        return Err(ValidationError::TemplateMissingPlaceholder);
    }
    if has_y && has_minus_y {
        return Err(ValidationError::TemplateYAndMinusY);
    }
    let min_zoom = parse_zoom(min_zoom_raw, 0).map_err(|_| ValidationError::MinZoomInvalid)?;
    let max_zoom = parse_zoom(max_zoom_raw, 19).map_err(|_| ValidationError::MaxZoomInvalid)?;
    if min_zoom > max_zoom {
        return Err(ValidationError::MinZoomAboveMax);
    }
    Ok(CustomImageryEntry {
        name: name.trim().to_string(),
        url_template: template.to_string(),
        min_zoom,
        max_zoom,
    })
}

fn parse_zoom(raw: &str, default_if_blank: u32) -> Result<u32, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(default_if_blank);
    }
    let v: u32 = s.parse().map_err(|_| ())?;
    if v > 24 {
        return Err(());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TMPL: &str = "https://tile.example.com/{z}/{x}/{y}.png";

    #[test]
    fn happy_path_defaults() {
        let e = validate("Example", TMPL, "", "").unwrap();
        assert_eq!(e.name, "Example");
        assert_eq!(e.url_template, TMPL);
        assert_eq!(e.min_zoom, 0);
        assert_eq!(e.max_zoom, 19);
    }

    #[test]
    fn happy_path_minus_y() {
        let e = validate(
            "Foo",
            "https://tile.example.com/{z}/{x}/{-y}.png",
            "4",
            "18",
        )
        .unwrap();
        assert_eq!(e.min_zoom, 4);
        assert_eq!(e.max_zoom, 18);
    }

    #[test]
    fn name_must_be_nonempty() {
        assert_eq!(validate("  ", TMPL, "", ""), Err(ValidationError::NameEmpty));
    }

    #[test]
    fn template_required() {
        assert_eq!(
            validate("Example", "  ", "", ""),
            Err(ValidationError::TemplateEmpty)
        );
    }

    #[test]
    fn template_missing_z_x_y() {
        assert_eq!(
            validate("Example", "https://example.com/a/b/c.png", "", ""),
            Err(ValidationError::TemplateMissingPlaceholder)
        );
    }

    #[test]
    fn template_cannot_contain_both_y_variants() {
        assert_eq!(
            validate(
                "Example",
                "https://example.com/{z}/{x}/{y}/{-y}.png",
                "",
                ""
            ),
            Err(ValidationError::TemplateYAndMinusY)
        );
    }

    #[test]
    fn min_above_max_rejected() {
        assert_eq!(
            validate("Example", TMPL, "15", "10"),
            Err(ValidationError::MinZoomAboveMax)
        );
    }

    #[test]
    fn out_of_range_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "25", ""),
            Err(ValidationError::MinZoomInvalid)
        );
        assert_eq!(
            validate("Example", TMPL, "", "99"),
            Err(ValidationError::MaxZoomInvalid)
        );
    }

    #[test]
    fn non_numeric_zoom_rejected() {
        assert_eq!(
            validate("Example", TMPL, "abc", ""),
            Err(ValidationError::MinZoomInvalid)
        );
    }
}
