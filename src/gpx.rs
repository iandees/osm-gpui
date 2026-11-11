//! GPX reader: tracks (`trk`/`trkseg`/`trkpt`), routes (`rte`/`rtept`) and
//! waypoints (`wpt`) become reference features. Names, descriptions, comments,
//! types and symbols are kept; elevation, time and everything else is skipped.
//!
//! The document is read as a stream, so a 50 MB track never exists as a tree.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::reference::{Builder, FeatureKind, Format, RefError, ReferenceData};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Gpx,
    Trk,
    Trkseg,
    Trkpt,
    Rte,
    Rtept,
    Wpt,
    Name,
    Desc,
    Cmt,
    Type,
    Sym,
    Other,
}

fn classify(local: &[u8]) -> Tag {
    match local {
        b"gpx" => Tag::Gpx,
        b"trk" => Tag::Trk,
        b"trkseg" => Tag::Trkseg,
        b"trkpt" => Tag::Trkpt,
        b"rte" => Tag::Rte,
        b"rtept" => Tag::Rtept,
        b"wpt" => Tag::Wpt,
        b"name" => Tag::Name,
        b"desc" => Tag::Desc,
        b"cmt" => Tag::Cmt,
        b"type" => Tag::Type,
        b"sym" => Tag::Sym,
        _ => Tag::Other,
    }
}

/// The track, route or waypoint being read.
#[derive(Default)]
struct Item {
    name: Option<String>,
    props: Vec<(String, String)>,
    /// `(lon, lat)` runs: one per `trkseg`, or the single run of a route.
    segments: Vec<Vec<(f64, f64)>>,
    /// A waypoint's position.
    point: Option<(f64, f64)>,
}

impl Item {
    fn set_text(&mut self, tag: Tag, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let key = match tag {
            Tag::Name => {
                self.name = Some(text);
                return;
            }
            Tag::Desc => "desc",
            Tag::Cmt => "cmt",
            Tag::Type => "type",
            Tag::Sym => "sym",
            _ => return,
        };
        self.props.push((key.to_string(), text));
    }
}

/// `lat` and `lon` of a point element, `None` if either is missing or not a
/// number (such a point is skipped rather than failing the file).
fn position(e: &BytesStart) -> Option<(f64, f64)> {
    let (mut lat, mut lon) = (None, None);
    for attr in e.attributes().flatten() {
        let value = attr.unescape_value().ok()?;
        match attr.key.local_name().as_ref() {
            b"lat" => lat = value.trim().parse::<f64>().ok(),
            b"lon" => lon = value.trim().parse::<f64>().ok(),
            _ => {}
        }
    }
    Some((lon?, lat?))
}

/// Parse a GPX document.
pub fn parse(bytes: &[u8]) -> Result<ReferenceData, RefError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut builder = Builder::new(Format::Gpx);
    let mut buf = Vec::new();
    let mut stack: Vec<Tag> = Vec::new();
    let mut seen_root = false;
    let mut item: Option<(Tag, Item)> = None;
    // The leaf element (`name`, `desc`, ...) whose text is being collected.
    let mut text_for: Option<Tag> = None;
    let mut text = String::new();

    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| {
            RefError::Malformed(format!("{e} at byte {}", reader.buffer_position()))
        })?;
        let (open, close, tag) = match &event {
            Event::Start(e) => (true, false, classify(e.local_name().as_ref())),
            Event::Empty(e) => (true, true, classify(e.local_name().as_ref())),
            Event::End(e) => (false, true, classify(e.local_name().as_ref())),
            Event::Text(t) => {
                if text_for.is_some() {
                    match t.unescape() {
                        Ok(s) => text.push_str(&s),
                        Err(_) => text.push_str(&String::from_utf8_lossy(t)),
                    }
                }
                buf.clear();
                continue;
            }
            Event::CData(t) => {
                if text_for.is_some() {
                    text.push_str(&String::from_utf8_lossy(t));
                }
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {
                buf.clear();
                continue;
            }
        };

        if open {
            let (Event::Start(e) | Event::Empty(e)) = &event else {
                unreachable!()
            };
            if stack.is_empty() {
                // The root element must be <gpx>.
                if tag != Tag::Gpx {
                    return Err(RefError::Malformed(
                        crate::tr!("reference.error.not_gpx").to_string(),
                    ));
                }
                seen_root = true;
            }
            let parent = stack.last().copied();
            match tag {
                Tag::Wpt if parent == Some(Tag::Gpx) => {
                    let it = Item {
                        point: position(e),
                        ..Item::default()
                    };
                    item = Some((Tag::Wpt, it));
                }
                Tag::Trk if parent == Some(Tag::Gpx) => {
                    item = Some((Tag::Trk, Item::default()));
                }
                Tag::Rte if parent == Some(Tag::Gpx) => {
                    let mut it = Item::default();
                    it.segments.push(Vec::new());
                    item = Some((Tag::Rte, it));
                }
                Tag::Trkseg if parent == Some(Tag::Trk) => {
                    if let Some((_, it)) = item.as_mut() {
                        it.segments.push(Vec::new());
                    }
                }
                Tag::Trkpt if parent == Some(Tag::Trkseg) => {
                    if let (Some((_, it)), Some(p)) = (item.as_mut(), position(e)) {
                        if let Some(seg) = it.segments.last_mut() {
                            seg.push(p);
                        }
                    }
                }
                Tag::Rtept if parent == Some(Tag::Rte) => {
                    if let (Some((_, it)), Some(p)) = (item.as_mut(), position(e)) {
                        if let Some(seg) = it.segments.last_mut() {
                            seg.push(p);
                        }
                    }
                }
                Tag::Name | Tag::Desc | Tag::Cmt | Tag::Type | Tag::Sym
                    if matches!(parent, Some(Tag::Wpt | Tag::Trk | Tag::Rte)) && item.is_some() =>
                {
                    text_for = Some(tag);
                    text.clear();
                }
                _ => {}
            }
            if !close {
                stack.push(tag);
            }
        }

        if close {
            if !open {
                stack.pop();
            }
            // `stack` now holds the parents of the element that ended.
            let parent = stack.last().copied();
            match tag {
                Tag::Name | Tag::Desc | Tag::Cmt | Tag::Type | Tag::Sym
                    if text_for == Some(tag) =>
                {
                    if let Some((_, it)) = item.as_mut() {
                        it.set_text(tag, std::mem::take(&mut text));
                    }
                    text_for = None;
                }
                Tag::Wpt | Tag::Trk | Tag::Rte if parent == Some(Tag::Gpx) => {
                    if let Some((kind, it)) = item.take() {
                        add_item(&mut builder, kind, it)?;
                    }
                }
                _ => {}
            }
        }
        buf.clear();
    }

    if !seen_root {
        return Err(RefError::Malformed(
            crate::tr!("reference.error.not_gpx").to_string(),
        ));
    }
    builder.finish()
}

fn add_item(builder: &mut Builder, kind: Tag, item: Item) -> Result<(), RefError> {
    let feature_kind = match kind {
        Tag::Wpt => FeatureKind::Waypoint,
        Tag::Trk => FeatureKind::Track,
        _ => FeatureKind::Route,
    };
    let id = builder.feature(feature_kind, "", item.name, item.props);
    if let Some((lon, lat)) = item.point {
        builder.point(id, lon, lat)?;
    }
    for seg in &item.segments {
        builder.line(id, seg)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
  <metadata><name>not a feature</name></metadata>
  <wpt lat="40.001" lon="-74.002">
    <ele>12.5</ele><time>2024-01-01T00:00:00Z</time>
    <name>Trailhead &amp; Parking</name>
    <desc>Start of the walk</desc>
    <sym>Flag</sym>
  </wpt>
  <wpt lat="40.002" lon="-74.003"/>
  <rte>
    <name>Planned</name>
    <rtept lat="40.0" lon="-74.0"><name>ignored</name></rtept>
    <rtept lat="40.01" lon="-74.01"/>
    <rtept lat="40.02" lon="-74.0"/>
  </rte>
  <trk>
    <name>Morning run</name>
    <cmt>sunny</cmt>
    <trkseg>
      <trkpt lat="40.0" lon="-74.0"><ele>1</ele><time>2024-01-01T00:00:00Z</time></trkpt>
      <trkpt lat="40.001" lon="-74.001"/>
      <trkpt lat="40.002" lon="-74.0"/>
    </trkseg>
    <trkseg>
      <trkpt lat="40.1" lon="-74.1"/>
      <trkpt lat="40.101" lon="-74.101"/>
    </trkseg>
  </trk>
</gpx>"#;

    #[test]
    fn reads_tracks_routes_and_waypoints() {
        let data = parse(FULL.as_bytes()).unwrap();
        assert_eq!(data.format, Format::Gpx);
        assert_eq!(data.features.len(), 4);
        let kinds: Vec<_> = data.features.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FeatureKind::Waypoint,
                FeatureKind::Waypoint,
                FeatureKind::Route,
                FeatureKind::Track
            ]
        );
        assert_eq!(data.markers.len(), 2);
        // Route: one run. Track: two segments.
        assert_eq!(data.parts.len(), 3);
        assert_eq!(data.vertex_count, 2 + 3 + 3 + 2);
        assert_eq!(data.extent(), Some((40.0, 40.101, -74.101, -74.0)));
    }

    #[test]
    fn keeps_names_and_text_properties() {
        let data = parse(FULL.as_bytes()).unwrap();
        let wpt = &data.features[0];
        assert_eq!(wpt.name.as_deref(), Some("Trailhead & Parking"));
        assert_eq!(
            wpt.props,
            vec![
                ("desc".to_string(), "Start of the walk".to_string()),
                ("sym".to_string(), "Flag".to_string())
            ]
        );
        assert_eq!(data.features[1].name, None);
        assert_eq!(data.features[2].name.as_deref(), Some("Planned"));
        let trk = &data.features[3];
        assert_eq!(trk.name.as_deref(), Some("Morning run"));
        assert_eq!(trk.props, vec![("cmt".to_string(), "sunny".to_string())]);
    }

    #[test]
    fn works_without_a_namespace_and_with_a_bom() {
        let doc = "\u{feff}<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\"/><trkpt lat=\"1.5\" lon=\"2.5\"/></trkseg></trk></gpx>";
        let data = parse(doc.as_bytes()).unwrap();
        assert_eq!(data.parts.len(), 1);
    }

    #[test]
    fn points_without_usable_positions_are_skipped() {
        let doc = r#"<gpx><trk><trkseg>
            <trkpt lat="1" lon="2"/><trkpt lat="x" lon="2"/><trkpt lon="3"/><trkpt lat="1.5" lon="2.5"/>
            </trkseg></trk><wpt/></gpx>"#;
        let data = parse(doc.as_bytes()).unwrap();
        assert_eq!(data.vertex_count, 2);
        assert_eq!(data.markers.len(), 0);
    }

    #[test]
    fn malformed_xml_is_an_error() {
        let err = parse(b"<gpx><trk><trkseg></trk></gpx>").unwrap_err();
        assert!(matches!(err, RefError::Malformed(_)), "{err:?}");
        let err = parse(b"this is not xml at all").unwrap_err();
        assert!(matches!(err, RefError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn other_xml_is_not_gpx() {
        let err =
            parse(b"<osm version=\"0.6\"><node id=\"1\" lat=\"1\" lon=\"1\"/></osm>").unwrap_err();
        assert!(matches!(err, RefError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn empty_and_geometry_free_files_are_errors() {
        assert!(matches!(parse(b""), Err(RefError::Malformed(_))));
        assert_eq!(
            parse(b"<gpx version=\"1.1\"></gpx>").unwrap_err(),
            RefError::NoGeometry
        );
        assert_eq!(
            parse(b"<gpx><trk><name>x</name><trkseg/></trk></gpx>").unwrap_err(),
            RefError::NoGeometry
        );
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        let err = parse(b"<gpx><wpt lat=\"123\" lon=\"2\"/></gpx>").unwrap_err();
        assert!(matches!(err, RefError::NotGeographic { .. }), "{err:?}");
    }
}
