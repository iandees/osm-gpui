use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

#[derive(Debug, Clone)]
pub struct OsmData {
    pub nodes: HashMap<i64, OsmNode>,
    pub ways: Vec<OsmWay>,
    pub relations: Vec<OsmRelation>,
    pub bounds: Option<OsmBounds>,
}

#[derive(Debug, Clone)]
pub struct OsmNode {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OsmWay {
    pub id: i64,
    pub nodes: Vec<i64>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OsmRelation {
    pub id: i64,
    pub members: Vec<OsmMember>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct OsmMember {
    pub member_type: String,
    pub reference: i64,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct OsmBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

pub struct OsmParser;

impl OsmParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse(&self, reader: BufReader<File>) -> Result<OsmData, OsmParseError> {
        let mut xml_reader = Reader::from_reader(reader);
        xml_reader.trim_text(true);

        let mut osm_data = OsmData {
            nodes: HashMap::new(),
            ways: Vec::new(),
            relations: Vec::new(),
            bounds: None,
        };

        let mut buf = Vec::new();
        let mut current_element = ElementType::None;
        let mut current_node: Option<OsmNode> = None;
        let mut current_way: Option<OsmWay> = None;
        let mut current_relation: Option<OsmRelation> = None;

        loop {
            match xml_reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => match e.name().as_ref() {
                    b"bounds" => {
                        osm_data.bounds = Some(self.parse_bounds(e)?);
                    }
                    b"node" => {
                        current_element = ElementType::Node;
                        current_node = Some(self.parse_node_start(e)?);
                    }
                    b"way" => {
                        current_element = ElementType::Way;
                        current_way = Some(self.parse_way_start(e)?);
                    }
                    b"relation" => {
                        current_element = ElementType::Relation;
                        current_relation = Some(self.parse_relation_start(e)?);
                    }
                    b"tag" => {
                        let (key, value) = self.parse_tag(e)?;
                        match current_element {
                            ElementType::Node => {
                                if let Some(ref mut node) = current_node {
                                    node.tags.insert(key, value);
                                }
                            }
                            ElementType::Way => {
                                if let Some(ref mut way) = current_way {
                                    way.tags.insert(key, value);
                                }
                            }
                            ElementType::Relation => {
                                if let Some(ref mut relation) = current_relation {
                                    relation.tags.insert(key, value);
                                }
                            }
                            _ => {}
                        }
                    }
                    b"nd" => {
                        if let Some(ref mut way) = current_way {
                            let node_ref = self.parse_node_ref(e)?;
                            way.nodes.push(node_ref);
                        }
                    }
                    b"member" => {
                        if let Some(ref mut relation) = current_relation {
                            let member = self.parse_member(e)?;
                            relation.members.push(member);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                    b"bounds" => {
                        osm_data.bounds = Some(self.parse_bounds(e)?);
                    }
                    b"node" => {
                        let node = self.parse_node_start(e)?;
                        osm_data.nodes.insert(node.id, node);
                    }
                    b"way" => {
                        let way = self.parse_way_start(e)?;
                        osm_data.ways.push(way);
                    }
                    b"relation" => {
                        let relation = self.parse_relation_start(e)?;
                        osm_data.relations.push(relation);
                    }
                    b"tag" => {
                        let (key, value) = self.parse_tag(e)?;
                        match current_element {
                            ElementType::Node => {
                                if let Some(ref mut node) = current_node {
                                    node.tags.insert(key, value);
                                }
                            }
                            ElementType::Way => {
                                if let Some(ref mut way) = current_way {
                                    way.tags.insert(key, value);
                                }
                            }
                            ElementType::Relation => {
                                if let Some(ref mut relation) = current_relation {
                                    relation.tags.insert(key, value);
                                }
                            }
                            _ => {}
                        }
                    }
                    b"nd" => {
                        if let Some(ref mut way) = current_way {
                            let node_ref = self.parse_node_ref(e)?;
                            way.nodes.push(node_ref);
                        }
                    }
                    b"member" => {
                        if let Some(ref mut relation) = current_relation {
                            let member = self.parse_member(e)?;
                            relation.members.push(member);
                        }
                    }
                    _ => {}
                },
                Ok(Event::End(ref e)) => match e.name().as_ref() {
                    b"node" => {
                        if let Some(node) = current_node.take() {
                            osm_data.nodes.insert(node.id, node);
                        }
                        current_element = ElementType::None;
                    }
                    b"way" => {
                        if let Some(way) = current_way.take() {
                            osm_data.ways.push(way);
                        }
                        current_element = ElementType::None;
                    }
                    b"relation" => {
                        if let Some(relation) = current_relation.take() {
                            osm_data.relations.push(relation);
                        }
                        current_element = ElementType::None;
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(OsmParseError::XmlError(e)),
                _ => {}
            }
            buf.clear();
        }

        Ok(osm_data)
    }

    pub fn parse_str(&self, xml_str: &str) -> Result<OsmData, OsmParseError> {
        let mut xml_reader = Reader::from_str(xml_str);
        xml_reader.trim_text(true);

        let mut osm_data = OsmData {
            nodes: HashMap::new(),
            ways: Vec::new(),
            relations: Vec::new(),
            bounds: None,
        };

        let mut buf = Vec::new();
        let mut current_element = ElementType::None;
        let mut current_node: Option<OsmNode> = None;
        let mut current_way: Option<OsmWay> = None;
        let mut current_relation: Option<OsmRelation> = None;

        loop {
            match xml_reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => match e.name().as_ref() {
                    b"bounds" => {
                        osm_data.bounds = Some(self.parse_bounds(e)?);
                    }
                    b"node" => {
                        current_element = ElementType::Node;
                        current_node = Some(self.parse_node_start(e)?);
                    }
                    b"way" => {
                        current_element = ElementType::Way;
                        current_way = Some(self.parse_way_start(e)?);
                    }
                    b"relation" => {
                        current_element = ElementType::Relation;
                        current_relation = Some(self.parse_relation_start(e)?);
                    }
                    b"tag" => {
                        let (key, value) = self.parse_tag(e)?;
                        match current_element {
                            ElementType::Node => {
                                if let Some(ref mut node) = current_node {
                                    node.tags.insert(key, value);
                                }
                            }
                            ElementType::Way => {
                                if let Some(ref mut way) = current_way {
                                    way.tags.insert(key, value);
                                }
                            }
                            ElementType::Relation => {
                                if let Some(ref mut relation) = current_relation {
                                    relation.tags.insert(key, value);
                                }
                            }
                            _ => {}
                        }
                    }
                    b"nd" => {
                        if let Some(ref mut way) = current_way {
                            let node_ref = self.parse_node_ref(e)?;
                            way.nodes.push(node_ref);
                        }
                    }
                    b"member" => {
                        if let Some(ref mut relation) = current_relation {
                            let member = self.parse_member(e)?;
                            relation.members.push(member);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                    b"bounds" => {
                        osm_data.bounds = Some(self.parse_bounds(e)?);
                    }
                    b"node" => {
                        let node = self.parse_node_start(e)?;
                        osm_data.nodes.insert(node.id, node);
                    }
                    b"way" => {
                        let way = self.parse_way_start(e)?;
                        osm_data.ways.push(way);
                    }
                    b"relation" => {
                        let relation = self.parse_relation_start(e)?;
                        osm_data.relations.push(relation);
                    }
                    b"tag" => {
                        let (key, value) = self.parse_tag(e)?;
                        match current_element {
                            ElementType::Node => {
                                if let Some(ref mut node) = current_node {
                                    node.tags.insert(key, value);
                                }
                            }
                            ElementType::Way => {
                                if let Some(ref mut way) = current_way {
                                    way.tags.insert(key, value);
                                }
                            }
                            ElementType::Relation => {
                                if let Some(ref mut relation) = current_relation {
                                    relation.tags.insert(key, value);
                                }
                            }
                            _ => {}
                        }
                    }
                    b"nd" => {
                        if let Some(ref mut way) = current_way {
                            let node_ref = self.parse_node_ref(e)?;
                            way.nodes.push(node_ref);
                        }
                    }
                    b"member" => {
                        if let Some(ref mut relation) = current_relation {
                            let member = self.parse_member(e)?;
                            relation.members.push(member);
                        }
                    }
                    _ => {}
                },
                Ok(Event::End(ref e)) => match e.name().as_ref() {
                    b"node" => {
                        if let Some(node) = current_node.take() {
                            osm_data.nodes.insert(node.id, node);
                        }
                        current_element = ElementType::None;
                    }
                    b"way" => {
                        if let Some(way) = current_way.take() {
                            osm_data.ways.push(way);
                        }
                        current_element = ElementType::None;
                    }
                    b"relation" => {
                        if let Some(relation) = current_relation.take() {
                            osm_data.relations.push(relation);
                        }
                        current_element = ElementType::None;
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(OsmParseError::XmlError(e)),
                _ => {}
            }
            buf.clear();
        }

        Ok(osm_data)
    }

    pub fn parse_file(&self, file_path: &str) -> Result<OsmData, OsmParseError> {
        let file = File::open(file_path).map_err(OsmParseError::IoError)?;
        let reader = BufReader::new(file);
        self.parse(reader)
    }

    fn parse_bounds(&self, e: &BytesStart) -> Result<OsmBounds, OsmParseError> {
        let mut min_lat = 0.0;
        let mut max_lat = 0.0;
        let mut min_lon = 0.0;
        let mut max_lon = 0.0;

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            match key {
                "minlat" => min_lat = self.parse_f64(value)?,
                "maxlat" => max_lat = self.parse_f64(value)?,
                "minlon" => min_lon = self.parse_f64(value)?,
                "maxlon" => max_lon = self.parse_f64(value)?,
                _ => {}
            }
        }

        Ok(OsmBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    }

    fn parse_node_start(&self, e: &BytesStart) -> Result<OsmNode, OsmParseError> {
        let mut id = 0;
        let mut lat = 0.0;
        let mut lon = 0.0;

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            match key {
                "id" => id = self.parse_i64(value)?,
                "lat" => lat = self.parse_f64(value)?,
                "lon" => lon = self.parse_f64(value)?,
                _ => {}
            }
        }

        Ok(OsmNode {
            id,
            lat,
            lon,
            tags: HashMap::new(),
        })
    }

    fn parse_way_start(&self, e: &BytesStart) -> Result<OsmWay, OsmParseError> {
        let mut id = 0;

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            if key == "id" {
                id = self.parse_i64(value)?;
            }
        }

        Ok(OsmWay {
            id,
            nodes: Vec::new(),
            tags: HashMap::new(),
        })
    }

    fn parse_relation_start(&self, e: &BytesStart) -> Result<OsmRelation, OsmParseError> {
        let mut id = 0;

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            if key == "id" {
                id = self.parse_i64(value)?;
            }
        }

        Ok(OsmRelation {
            id,
            members: Vec::new(),
            tags: HashMap::new(),
        })
    }

    fn parse_tag(&self, e: &BytesStart) -> Result<(String, String), OsmParseError> {
        let mut key = String::new();
        let mut value = String::new();

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let attr_key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let attr_value = std::str::from_utf8(&attr.value).unwrap();

            match attr_key {
                "k" => key = attr_value.to_string(),
                "v" => value = attr_value.to_string(),
                _ => {}
            }
        }

        if key.is_empty() {
            return Err(OsmParseError::MissingAttribute("k".to_string()));
        }

        Ok((key, value))
    }

    fn parse_node_ref(&self, e: &BytesStart) -> Result<i64, OsmParseError> {
        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            if key == "ref" {
                return self.parse_i64(value);
            }
        }

        Err(OsmParseError::MissingAttribute("ref".to_string()))
    }

    fn parse_member(&self, e: &BytesStart) -> Result<OsmMember, OsmParseError> {
        let mut member_type = String::new();
        let mut reference = 0;
        let mut role = String::new();

        for attr in e.attributes() {
            let attr = attr.map_err(OsmParseError::AttrError)?;
            let key = std::str::from_utf8(attr.key.as_ref()).unwrap();
            let value = std::str::from_utf8(&attr.value).unwrap();

            match key {
                "type" => member_type = value.to_string(),
                "ref" => reference = self.parse_i64(value)?,
                "role" => role = value.to_string(),
                _ => {}
            }
        }

        Ok(OsmMember {
            member_type,
            reference,
            role,
        })
    }

    fn parse_i64(&self, s: &str) -> Result<i64, OsmParseError> {
        s.parse()
            .map_err(|_| OsmParseError::ParseError(format!("Invalid i64: {}", s)))
    }

    fn parse_f64(&self, s: &str) -> Result<f64, OsmParseError> {
        s.parse()
            .map_err(|_| OsmParseError::ParseError(format!("Invalid f64: {}", s)))
    }
}

#[derive(Debug)]
enum ElementType {
    None,
    Node,
    Way,
    Relation,
}

#[derive(Debug)]
pub enum OsmParseError {
    XmlError(quick_xml::Error),
    IoError(std::io::Error),
    AttrError(quick_xml::events::attributes::AttrError),
    MissingAttribute(String),
    ParseError(String),
}

impl From<std::io::Error> for OsmParseError {
    fn from(err: std::io::Error) -> Self {
        OsmParseError::IoError(err)
    }
}

impl From<quick_xml::Error> for OsmParseError {
    fn from(err: quick_xml::Error) -> Self {
        OsmParseError::XmlError(err)
    }
}

impl From<quick_xml::events::attributes::AttrError> for OsmParseError {
    fn from(err: quick_xml::events::attributes::AttrError) -> Self {
        OsmParseError::AttrError(err)
    }
}

impl std::fmt::Display for OsmParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OsmParseError::XmlError(e) => write!(f, "XML error: {}", e),
            OsmParseError::IoError(e) => write!(f, "IO error: {}", e),
            OsmParseError::AttrError(e) => write!(f, "Attribute error: {}", e),
            OsmParseError::MissingAttribute(attr) => write!(f, "Missing attribute: {}", attr),
            OsmParseError::ParseError(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl std::error::Error for OsmParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_str_includes_self_closing_nodes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6">
  <node id="1" lat="40.0" lon="-74.0"/>
  <node id="2" lat="40.1" lon="-74.1">
    <tag k="name" v="tagged"/>
  </node>
  <way id="10">
    <nd ref="1"/>
    <nd ref="2"/>
  </way>
</osm>"#;

        let parser = OsmParser::new();
        let osm_data = parser.parse_str(xml).expect("parse_str failed");

        assert!(
            osm_data.nodes.contains_key(&1),
            "node 1 (self-closing, untagged) was not inserted"
        );
        assert!(
            osm_data.nodes.contains_key(&2),
            "node 2 (tagged, paired tags) was not inserted"
        );

        let tagged = &osm_data.nodes[&2];
        assert_eq!(
            tagged.tags.get("name").map(|s| s.as_str()),
            Some("tagged"),
            "tag on node 2 was not parsed"
        );

        assert_eq!(osm_data.ways.len(), 1, "expected exactly one way");
        assert_eq!(
            osm_data.ways[0].nodes,
            vec![1, 2],
            "way node refs do not match"
        );
    }
}
