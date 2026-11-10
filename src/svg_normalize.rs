//! Makes vendored preset icons square.
//!
//! GPUI renders an `svg()` at `element width / SVG width` scale and stretches
//! the resulting bitmap over the element box, so an icon whose viewBox is not
//! square (many Font Awesome glyphs are 448x512, 640x512, ...) would be drawn
//! squashed in a square slot. `examples/update_presets.rs` runs every
//! downloaded icon through [`square_viewbox`] so the vendored files already
//! carry a centred square viewBox; the shapes themselves are never edited.

/// The `(start, end, value)` of attribute `name` inside the opening-tag text
/// `tag`, where `start..end` covers the leading whitespace, the name and the
/// quoted value. `None` when the attribute is absent.
fn find_attr<'a>(tag: &'a str, name: &str) -> Option<(usize, usize, &'a str)> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(rel) = tag[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        // Must be a whole attribute name: preceded by whitespace and followed
        // by `=` (so `stroke-width` never matches `width`).
        if at == 0 || !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut i = at + name.len();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let quote = *bytes.get(i)?;
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        let value_start = i + 1;
        let value_len = tag[value_start..].find(quote as char)?;
        let value_end = value_start + value_len;
        let mut start = at;
        while start > 0 && bytes[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        return Some((start, value_end + 1, &tag[value_start..value_end]));
    }
    None
}

fn parse_length(value: &str) -> Option<f64> {
    let v = value.trim().trim_end_matches("px").trim();
    v.parse::<f64>().ok().filter(|n| *n > 0.0)
}

fn parse_view_box(value: &str) -> Option<[f64; 4]> {
    let nums: Vec<f64> = value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    match nums[..] {
        [x, y, w, h] if w > 0.0 && h > 0.0 => Some([x, y, w, h]),
        _ => None,
    }
}

/// The byte range of the root `<svg ...>` opening tag.
fn root_tag(svg: &str) -> Option<(usize, usize)> {
    let start = svg.find("<svg")?;
    let end = start + svg[start..].find('>')? + 1;
    Some((start, end))
}

/// The aspect ratio (width / height) a renderer would use for this SVG:
/// its `width`/`height` attributes when both are present, else its viewBox.
pub fn aspect_ratio(svg: &str) -> Option<f64> {
    let (s, e) = root_tag(svg)?;
    let tag = &svg[s..e];
    let width = find_attr(tag, "width").and_then(|(_, _, v)| parse_length(v));
    let height = find_attr(tag, "height").and_then(|(_, _, v)| parse_length(v));
    if let (Some(w), Some(h)) = (width, height) {
        return Some(w / h);
    }
    let vb = find_attr(tag, "viewBox").and_then(|(_, _, v)| parse_view_box(v))?;
    Some(vb[2] / vb[3])
}

/// Returns `svg` with a square, centred viewBox and no `width`/`height`
/// attributes when its viewBox (or width/height) is not already square;
/// returns it unchanged otherwise, or when it carries no usable dimensions.
pub fn square_viewbox(svg: &str) -> String {
    let Some((s, e)) = root_tag(svg) else {
        return svg.to_string();
    };
    let tag = &svg[s..e];
    let vb_attr = find_attr(tag, "viewBox");
    let width = find_attr(tag, "width");
    let height = find_attr(tag, "height");
    let vb = vb_attr.and_then(|(_, _, v)| parse_view_box(v)).or_else(|| {
        let w = parse_length(width?.2)?;
        let h = parse_length(height?.2)?;
        Some([0.0, 0.0, w, h])
    });
    let Some([x, y, w, h]) = vb else {
        return svg.to_string();
    };
    if (w - h).abs() < 1e-9 {
        return svg.to_string();
    }
    let side = w.max(h);
    let new_view_box = format!(
        " viewBox=\"{} {} {} {}\"",
        fmt_num(x - (side - w) / 2.0),
        fmt_num(y - (side - h) / 2.0),
        fmt_num(side),
        fmt_num(side)
    );

    // Drop the old viewBox / width / height, then add the new viewBox right
    // after `<svg`. Remove from the back so earlier offsets stay valid.
    let mut cuts: Vec<(usize, usize)> = [vb_attr, width, height]
        .into_iter()
        .flatten()
        .map(|(a, b, _)| (a, b))
        .collect();
    cuts.sort_unstable_by(|a, b| b.cmp(a));
    let mut new_tag = tag.to_string();
    for (a, b) in cuts {
        new_tag.replace_range(a..b, "");
    }
    new_tag.insert_str("<svg".len(), &new_view_box);

    format!("{}{}{}", &svg[..s], new_tag, &svg[e..])
}

fn fmt_num(n: f64) -> String {
    let s = format!("{:.4}", n);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_icons_are_untouched() {
        let svg =
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 15 15"><path d="M0 0"/></svg>"#;
        assert_eq!(square_viewbox(svg), svg);
        let sized = r#"<svg width="50" height="50" viewBox="5 0 50 50"><path/></svg>"#;
        assert_eq!(square_viewbox(sized), sized);
    }

    #[test]
    fn tall_viewbox_is_centred_horizontally() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 448 512"><path/></svg>"#;
        let out = square_viewbox(svg);
        assert!(out.contains(r#"viewBox="-32 0 512 512""#), "{out}");
        assert!(out.contains(r#"xmlns="http://www.w3.org/2000/svg""#));
        assert_eq!(aspect_ratio(&out), Some(1.0));
    }

    #[test]
    fn wide_viewbox_is_centred_vertically_and_sizes_are_dropped() {
        let svg = r#"<svg width="640" height="512" viewBox="0 0 640 512"><path/></svg>"#;
        let out = square_viewbox(svg);
        assert!(out.contains(r#"viewBox="0 -64 640 640""#), "{out}");
        assert!(!out.contains("width="), "{out}");
        assert_eq!(aspect_ratio(&out), Some(1.0));
    }

    #[test]
    fn missing_viewbox_is_built_from_width_and_height() {
        let svg = r#"<svg width="20" height="10"><path/></svg>"#;
        let out = square_viewbox(svg);
        assert!(out.contains(r#"viewBox="0 -5 20 20""#), "{out}");
    }

    #[test]
    fn stroke_width_is_not_mistaken_for_width() {
        let svg = r#"<svg stroke-width="3" viewBox="0 0 10 20"><path/></svg>"#;
        let out = square_viewbox(svg);
        assert!(out.contains(r#"stroke-width="3""#), "{out}");
        assert!(out.contains(r#"viewBox="-5 0 20 20""#), "{out}");
    }

    #[test]
    fn unparseable_input_is_returned_as_is() {
        assert_eq!(square_viewbox("not svg"), "not svg");
        let no_dims = "<svg><path/></svg>";
        assert_eq!(square_viewbox(no_dims), no_dims);
    }
}
