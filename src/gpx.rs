//! GPX reader: tracks (`trk`/`trkseg`/`trkpt`), routes (`rte`/`rtept`) and
//! waypoints (`wpt`) become reference features. Names, descriptions, comments,
//! types and symbols are kept; elevation, time and everything else is skipped.
//!
//! The document is read as a stream, so a 50 MB track never exists as a tree.
//!
//! [`parse_trackpoints`] reads the pages the OSM API returns for
//! `/api/0.6/trackpoints`. There a segment whose points carry a `<time>` is a
//! real path (a trackable or identifiable trace), while the points of private
//! traces come in one segment without times, sorted by position: joined up
//! they would zigzag across the area, so they are kept as separate points.

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
    Url,
    Time,
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
        b"url" => Tag::Url,
        b"time" => Tag::Time,
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
    /// Per segment: whether any of its points had a `<time>`.
    timed: Vec<bool>,
    /// Per segment, per point: its `<time>` (Unix seconds), kept only for
    /// trackpoints answers.
    times: Vec<Vec<Option<i64>>>,
    /// The `trkpt` being read was kept (it had a usable position).
    in_point: bool,
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
            Tag::Url => "url",
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
    let mut builder = Builder::new(Format::Gpx);
    read_into(&mut builder, bytes, false)?;
    builder.finish()
}

/// Parse the pages of an OSM API `trackpoints` answer into one layer's data.
/// Timed segments become tracks, untimed points become
/// [`FeatureKind::TracePoints`] markers. `NoGeometry` when no page held a
/// point.
pub fn parse_trackpoints(pages: &[&[u8]]) -> Result<ReferenceData, RefError> {
    let mut builder = Builder::new(Format::OsmTraces);
    for page in pages {
        read_into(&mut builder, page, true)?;
    }
    builder.finish()
}

/// How many `<trkpt>` elements `bytes` holds, without parsing it: the API
/// pages trackpoints, and a full page means there may be another.
pub fn count_trackpoints(bytes: &[u8]) -> usize {
    const NEEDLE: &[u8] = b"<trkpt";
    bytes.windows(NEEDLE.len()).filter(|w| *w == NEEDLE).count()
}

/// Read one GPX document into `builder`. With `traces`, segments are split
/// into paths and loose points as described in the module docs.
fn read_into(builder: &mut Builder, bytes: &[u8], traces: bool) -> Result<(), RefError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
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
                        it.timed.push(false);
                        it.times.push(Vec::new());
                    }
                }
                Tag::Time if parent == Some(Tag::Trkpt) => {
                    if let Some((_, it)) = item.as_mut() {
                        if let Some(t) = it.timed.last_mut() {
                            *t = true;
                        }
                        if traces && it.in_point {
                            text_for = Some(Tag::Time);
                            text.clear();
                        }
                    }
                }
                Tag::Trkpt if parent == Some(Tag::Trkseg) => {
                    if let Some((_, it)) = item.as_mut() {
                        it.in_point = false;
                        if let (Some(p), Some(seg)) = (position(e), it.segments.last_mut()) {
                            seg.push(p);
                            if let Some(times) = it.times.last_mut() {
                                times.push(None);
                            }
                            it.in_point = true;
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
                Tag::Name | Tag::Desc | Tag::Cmt | Tag::Type | Tag::Sym | Tag::Url
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
                Tag::Name | Tag::Desc | Tag::Cmt | Tag::Type | Tag::Sym | Tag::Url
                    if text_for == Some(tag) =>
                {
                    if let Some((_, it)) = item.as_mut() {
                        it.set_text(tag, std::mem::take(&mut text));
                    }
                    text_for = None;
                }
                Tag::Time if text_for == Some(Tag::Time) => {
                    if let Some((_, it)) = item.as_mut() {
                        let t = crate::object_history::parse_timestamp(text.trim());
                        if let Some(slot) = it.times.last_mut().and_then(|s| s.last_mut()) {
                            *slot = t;
                        }
                    }
                    text.clear();
                    text_for = None;
                }
                Tag::Wpt | Tag::Trk | Tag::Rte if parent == Some(Tag::Gpx) => {
                    if let Some((kind, it)) = item.take() {
                        if traces {
                            add_trace(builder, it)?;
                        } else {
                            add_item(builder, kind, it)?;
                        }
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
    Ok(())
}

/// One `<trk>` of a trackpoints answer: its timed segments as one track
/// feature, its untimed points as one feature of loose points.
fn add_trace(builder: &mut Builder, item: Item) -> Result<(), RefError> {
    let (mut timed, mut loose): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
    for ((seg, &has_time), times) in item.segments.iter().zip(&item.timed).zip(&item.times) {
        if has_time {
            timed.push((seg, times));
        } else {
            loose.push(seg);
        }
    }
    if !timed.is_empty() {
        let start = timed
            .iter()
            .flat_map(|(_, times)| times.iter().flatten())
            .min()
            .copied();
        let id = builder.feature(FeatureKind::Track, "", item.name, item.props);
        for (seg, times) in timed {
            builder.line_timed(id, seg, times, start)?;
        }
    }
    if loose.iter().any(|s| !s.is_empty()) {
        let id = builder.feature(FeatureKind::TracePoints, "", None, Vec::new());
        for &(lon, lat) in loose.into_iter().flatten() {
            builder.point(id, lon, lat)?;
        }
    }
    Ok(())
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
        assert_eq!(
            data.extent(),
            Some(crate::coordinates::GeoBounds::new(
                40.0, 40.101, -74.101, -74.0
            ))
        );
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

    /// Shaped like an `/api/0.6/trackpoints` page: an identifiable trace, a
    /// trackable one (no name, but timed), then private points.
    const TRACKPOINTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.0" creator="OpenStreetMap.org" xmlns="http://www.topografix.com/GPX/1/0">
  <trk>
    <name>20190626.gpx</name>
    <desc>Footpath near station</desc>
    <url>https://www.openstreetmap.org/user/someone/traces/123</url>
    <trkseg>
      <trkpt lat="51.5" lon="-0.1"><time>2019-06-26T07:00:00Z</time></trkpt>
      <trkpt lat="51.501" lon="-0.101"><time>2019-06-26T07:00:05Z</time></trkpt>
    </trkseg>
  </trk>
  <trk>
    <trkseg>
      <trkpt lat="51.52" lon="-0.12"><time>2020-01-01T00:00:00Z</time></trkpt>
      <trkpt lat="51.521" lon="-0.121"><time>2020-01-01T00:00:01Z</time></trkpt>
      <trkpt lat="51.522" lon="-0.12"><time>2020-01-01T00:00:02Z</time></trkpt>
    </trkseg>
  </trk>
  <trk>
    <trkseg>
      <trkpt lat="51.49" lon="-0.13"/>
      <trkpt lat="51.49" lon="-0.09"/>
      <trkpt lat="51.51" lon="-0.13"/>
      <trkpt lat="51.51" lon="-0.09"/>
    </trkseg>
  </trk>
</gpx>"#;

    #[test]
    fn trackpoints_draw_timed_segments_as_lines_and_private_points_as_dots() {
        let data = parse_trackpoints(&[TRACKPOINTS.as_bytes()]).unwrap();
        assert_eq!(data.format, Format::OsmTraces);
        let kinds: Vec<_> = data.features.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FeatureKind::Track,
                FeatureKind::Track,
                FeatureKind::TracePoints
            ]
        );
        assert_eq!(data.parts.len(), 2);
        assert_eq!(data.markers.len(), 4);
        assert_eq!(data.vertex_count, 2 + 3 + 4);
        let named = &data.features[0];
        assert_eq!(named.name.as_deref(), Some("20190626.gpx"));
        assert!(named
            .props
            .iter()
            .any(|(k, v)| k == "url" && v.ends_with("/123")));
    }

    #[test]
    fn trackpoint_traces_carry_speed_bearing_and_start_time() {
        let data = parse_trackpoints(&[TRACKPOINTS.as_bytes()]).unwrap();
        let first = data.parts[0].track.as_ref().unwrap();
        // 2019-06-26T07:00:00Z
        assert_eq!(first.start, Some(1_561_532_400));
        // ~133 m to the north-west in 5 s.
        assert_eq!(first.speed.len(), 2);
        assert!((first.speed[0] - 26.6).abs() < 1.0, "{:?}", first.speed);
        assert!(
            (300.0..340.0).contains(&first.bearing[0]),
            "{:?}",
            first.bearing
        );
        assert_eq!(first.speed[1], first.speed[0]);
        // One-second steps are measured over two seconds.
        let second = data.parts[1].track.as_ref().unwrap();
        assert!(second.speed.iter().all(|s| s.is_finite() && *s > 0.0));
        // Files keep no measures.
        assert!(parse(FULL.as_bytes()).unwrap().parts[0].track.is_none());
    }

    #[test]
    fn points_sharing_a_second_still_get_a_speed() {
        let doc = r#"<gpx><trk><trkseg>
            <trkpt lat="1.0" lon="2.0"><time>2020-01-01T00:00:00Z</time></trkpt>
            <trkpt lat="1.0001" lon="2.0"><time>2020-01-01T00:00:00Z</time></trkpt>
            <trkpt lat="1.0002" lon="2.0"><time>2020-01-01T00:00:01Z</time></trkpt>
            <trkpt lat="1.0003" lon="2.0"><time>2020-01-01T00:00:02Z</time></trkpt>
            </trkseg></trk></gpx>"#;
        let data = parse_trackpoints(&[doc.as_bytes()]).unwrap();
        let t = data.parts[0].track.as_ref().unwrap();
        // ~11 m a step at about a step a second, heading north. Times are
        // whole seconds, so the measure is rough.
        for s in &t.speed {
            assert!((8.0..25.0).contains(s), "{:?}", t.speed);
        }
        assert!(t.bearing[0] < 1.0 || t.bearing[0] > 359.0);
    }

    #[test]
    fn trackpoint_pages_add_up_and_an_empty_answer_has_nothing_to_draw() {
        let empty: &[u8] = br#"<gpx version="1.0" creator="OpenStreetMap.org"></gpx>"#;
        assert_eq!(
            parse_trackpoints(&[empty]).unwrap_err(),
            RefError::NoGeometry
        );
        let data =
            parse_trackpoints(&[TRACKPOINTS.as_bytes(), empty, TRACKPOINTS.as_bytes()]).unwrap();
        assert_eq!(data.features.len(), 6);
        assert_eq!(data.markers.len(), 8);
    }

    #[test]
    fn counts_trackpoints_without_parsing() {
        assert_eq!(count_trackpoints(TRACKPOINTS.as_bytes()), 9);
        // `<trkseg` does not count.
        assert_eq!(count_trackpoints(b"<gpx><trk><trkseg/></trk></gpx>"), 0);
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        let err = parse(b"<gpx><wpt lat=\"123\" lon=\"2\"/></gpx>").unwrap_err();
        assert!(matches!(err, RefError::NotGeographic { .. }), "{err:?}");
    }
}
