//! Typed access to the modern XMind graph structure and branch connector styles.
//!
//! XMind stores the sheet graph structure on `rootTopic.structureClass` and stores
//! connector fields in style `properties` maps. This module deliberately edits a
//! small allow-list of fields and never reconstructs a topic or style object from a
//! typed model, which keeps unknown XMind data intact.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use thiserror::Error;

pub const STRUCTURE_CLASS_FIELD: &str = "structureClass";
pub const CONNECTOR_CLASS_FIELD: &str = "line-class";
pub const CONNECTOR_CORNER_FIELD: &str = "line-corner";
pub const CONNECTOR_WIDTH_FIELD: &str = "line-width";
pub const CONNECTOR_COLOR_FIELD: &str = "line-color";
pub const CONNECTOR_PATTERN_FIELD: &str = "line-pattern";
pub const MAX_CONNECTOR_CORNER_RADIUS: u16 = 256;

/// Graph structures supported by the Kite editor.
///
/// `Other` preserves a class that this version does not understand. Reading and
/// then saving an unedited document never rewrites that value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphStructure {
    MindMap,
    LogicRight,
    LogicLeft,
    OrgChart,
    Tree,
    Other(String),
}

impl GraphStructure {
    pub fn from_xmind_value(value: &str) -> Self {
        match value {
            "org.xmind.ui.logic.right" | "org.xmind.ui.logic-chart.right" => Self::LogicRight,
            "org.xmind.ui.logic.left" | "org.xmind.ui.logic-chart.left" => Self::LogicLeft,
            "org.xmind.ui.org-chart.down" => Self::OrgChart,
            "org.xmind.ui.tree.right" => Self::Tree,
            value if value.starts_with("org.xmind.ui.map") => Self::MindMap,
            value => Self::Other(value.to_owned()),
        }
    }

    pub fn xmind_value(&self) -> &str {
        match self {
            Self::MindMap => "org.xmind.ui.map",
            Self::LogicRight => "org.xmind.ui.logic.right",
            Self::LogicLeft => "org.xmind.ui.logic.left",
            Self::OrgChart => "org.xmind.ui.org-chart.down",
            Self::Tree => "org.xmind.ui.tree.right",
            Self::Other(value) => value,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl fmt::Display for GraphStructure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.xmind_value())
    }
}

impl Serialize for GraphStructure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.xmind_value())
    }
}

impl<'de> Deserialize<'de> for GraphStructure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_xmind_value(&value))
    }
}

/// Connector shapes supported by the Kite editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectorKind {
    Curved,
    Straight,
    Elbow,
    RoundedElbow,
    Other(String),
}

impl ConnectorKind {
    pub fn from_xmind_value(value: &str) -> Self {
        match value {
            "org.xmind.branchConnection.curve" => Self::Curved,
            "org.xmind.branchConnection.straight" => Self::Straight,
            "org.xmind.branchConnection.elbow" => Self::Elbow,
            "org.xmind.branchConnection.roundedElbow" => Self::RoundedElbow,
            value => Self::Other(value.to_owned()),
        }
    }

    pub fn xmind_value(&self) -> &str {
        match self {
            Self::Curved => "org.xmind.branchConnection.curve",
            Self::Straight => "org.xmind.branchConnection.straight",
            Self::Elbow => "org.xmind.branchConnection.elbow",
            Self::RoundedElbow => "org.xmind.branchConnection.roundedElbow",
            Self::Other(value) => value,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl fmt::Display for ConnectorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.xmind_value())
    }
}

impl Serialize for ConnectorKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.xmind_value())
    }
}

impl<'de> Deserialize<'de> for ConnectorKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_xmind_value(&value))
    }
}

/// XMind connector line pattern values observed in bundled templates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectorLinePattern {
    Inherited,
    Solid,
    Dash,
    Dot,
    DashDot,
    DashDotDot,
    Other(String),
}

impl ConnectorLinePattern {
    pub fn from_xmind_value(value: &str) -> Self {
        match value {
            "inherited" => Self::Inherited,
            "solid" => Self::Solid,
            "dash" => Self::Dash,
            "dot" => Self::Dot,
            "dash-dot" => Self::DashDot,
            "dash-dot-dot" => Self::DashDotDot,
            value => Self::Other(value.to_owned()),
        }
    }

    pub fn xmind_value(&self) -> &str {
        match self {
            Self::Inherited => "inherited",
            Self::Solid => "solid",
            Self::Dash => "dash",
            Self::Dot => "dot",
            Self::DashDot => "dash-dot",
            Self::DashDotDot => "dash-dot-dot",
            Self::Other(value) => value,
        }
    }
}

impl fmt::Display for ConnectorLinePattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.xmind_value())
    }
}

impl Serialize for ConnectorLinePattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.xmind_value())
    }
}

impl<'de> Deserialize<'de> for ConnectorLinePattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_xmind_value(&value))
    }
}

/// Corner radius stored in XMind's `line-corner` style property.
///
/// The property key is exposed by XMind's style model, but the inspected bundled
/// templates do not contain a serialized example. Until XMind 27.01 visual
/// verification is recorded, this is an experimental mapping and must not be
/// treated as a pixel-identical compatibility claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectorCornerRadius(u16);

impl ConnectorCornerRadius {
    pub fn new(value: u16) -> Result<Self, GraphStyleError> {
        if value > MAX_CONNECTOR_CORNER_RADIUS {
            return Err(GraphStyleError::CornerRadiusOutOfRange {
                value,
                max: MAX_CONNECTOR_CORNER_RADIUS,
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> u16 {
        self.0
    }

    fn from_xmind_value(value: &Value) -> Option<Self> {
        let value = match value {
            Value::Number(value) => value.as_u64().and_then(|value| u16::try_from(value).ok())?,
            Value::String(value) => value
                .strip_suffix("px")
                .or_else(|| value.strip_suffix("pt"))
                .unwrap_or(value)
                .parse()
                .ok()?,
            _ => return None,
        };
        Self::new(value).ok()
    }

    fn xmind_value(self) -> String {
        self.0.to_string()
    }
}

/// A validated non-empty XMind line width such as `2`, `2pt`, or `inherited`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectorWidth(String);

impl ConnectorWidth {
    pub fn new(value: impl Into<String>) -> Result<Self, GraphStyleError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(GraphStyleError::EmptyField(CONNECTOR_WIDTH_FIELD));
        }
        Ok(Self(value))
    }

    pub fn points(value: u16) -> Self {
        Self(format!("{value}pt"))
    }

    pub fn inherited() -> Self {
        Self("inherited".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated non-empty XMind color value, including 8-digit alpha colors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectorColor(String);

impl ConnectorColor {
    pub fn new(value: impl Into<String>) -> Result<Self, GraphStyleError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(GraphStyleError::EmptyField(CONNECTOR_COLOR_FIELD));
        }
        if trimmed != "inherited" && (!trimmed.starts_with('#') || !matches!(trimmed.len(), 7 | 9))
        {
            return Err(GraphStyleError::InvalidColor(value));
        }
        Ok(Self(value))
    }

    pub fn inherited() -> Self {
        Self("inherited".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Connector line settings read from one XMind style properties map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorStyle {
    pub kind: ConnectorKind,
    pub corner_radius: Option<ConnectorCornerRadius>,
    pub width: Option<ConnectorWidth>,
    pub color: Option<ConnectorColor>,
    pub pattern: Option<ConnectorLinePattern>,
}

impl ConnectorStyle {
    pub fn from_properties(properties: &Map<String, Value>) -> Option<Self> {
        let kind = properties
            .get(CONNECTOR_CLASS_FIELD)
            .and_then(Value::as_str)
            .map(ConnectorKind::from_xmind_value)?;
        let corner_radius = properties
            .get(CONNECTOR_CORNER_FIELD)
            .and_then(ConnectorCornerRadius::from_xmind_value);
        let width = properties
            .get(CONNECTOR_WIDTH_FIELD)
            .and_then(value_as_xmind_string)
            .map(ConnectorWidth);
        let color = properties
            .get(CONNECTOR_COLOR_FIELD)
            .and_then(value_as_xmind_string)
            .map(ConnectorColor);
        let pattern = properties
            .get(CONNECTOR_PATTERN_FIELD)
            .and_then(Value::as_str)
            .map(ConnectorLinePattern::from_xmind_value);
        Some(Self {
            kind,
            corner_radius,
            width,
            color,
            pattern,
        })
    }
}

/// Document-level graph and connector values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentGraphStyle {
    pub structure: Option<GraphStructure>,
    pub connector: Option<ConnectorStyle>,
}

/// Partial document-level graph style update. `None` keeps the corresponding field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentGraphStylePatch {
    pub structure: Option<GraphStructure>,
    pub connector: Option<ConnectorStylePatch>,
}

/// Partial connector update. Unknown XMind properties are always left in place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectorStylePatch {
    pub kind: Option<ConnectorKind>,
    pub corner_radius: Option<ConnectorCornerRadius>,
    pub width: Option<ConnectorWidth>,
    pub color: Option<ConnectorColor>,
    pub pattern: Option<ConnectorLinePattern>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphStyleError {
    #[error("sheet index {index} is out of range")]
    SheetNotFound { index: usize },
    #[error("sheet {index} has no object-valued rootTopic")]
    MissingRootTopic { index: usize },
    #[error("XMind field {path} must be an object")]
    InvalidObject { path: String },
    #[error("corner radius {value} exceeds the supported maximum of {max}")]
    CornerRadiusOutOfRange { value: u16, max: u16 },
    #[error("XMind field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("invalid XMind color value {0:?}")]
    InvalidColor(String),
}

/// Read the document-level graph style from one sheet.
///
/// Connector fields use the root topic's explicit style first, then XMind's
/// `theme.centralTopic` fallback.
pub fn read_document_graph_style(
    content: &Value,
    sheet_index: usize,
) -> Result<DocumentGraphStyle, GraphStyleError> {
    let sheet = sheet_at(content, sheet_index)?;
    let root = root_topic(sheet, sheet_index)?;
    let structure = root
        .get(STRUCTURE_CLASS_FIELD)
        .and_then(Value::as_str)
        .map(GraphStructure::from_xmind_value);
    let connector = root
        .pointer("/style/properties")
        .and_then(Value::as_object)
        .and_then(ConnectorStyle::from_properties)
        .or_else(|| {
            sheet
                .pointer("/theme/centralTopic/properties")
                .and_then(Value::as_object)
                .and_then(ConnectorStyle::from_properties)
        });

    Ok(DocumentGraphStyle {
        structure,
        connector,
    })
}

/// Apply a document-level graph style update.
///
/// The function edits only these known fields:
///
/// - `rootTopic.structureClass`
/// - `rootTopic.style.properties.{line-class,line-corner,line-width,line-color,line-pattern}`
/// - the same connector fields in `theme.centralTopic.properties` when a theme exists
///
/// No topic/style object is replaced wholesale, and the skeleton-structure extension
/// is intentionally left untouched because its exact update semantics are not yet
/// verified against the XMind 27.01 compatibility baseline.
pub fn apply_document_graph_style(
    content: &mut Value,
    sheet_index: usize,
    patch: &DocumentGraphStylePatch,
) -> Result<(), GraphStyleError> {
    {
        let sheet = sheet_at_mut(content, sheet_index)?;
        let root = root_topic_mut(sheet, sheet_index)?;
        if let Some(structure) = &patch.structure {
            let root = root
                .as_object_mut()
                .ok_or_else(|| GraphStyleError::InvalidObject {
                    path: format!("sheets[{sheet_index}].rootTopic"),
                })?;
            root.insert(
                STRUCTURE_CLASS_FIELD.to_owned(),
                Value::String(structure.xmind_value().to_owned()),
            );
        }
    }

    if let Some(connector) = &patch.connector {
        let sheet = sheet_at_mut(content, sheet_index)?;

        {
            let root = root_topic_mut(sheet, sheet_index)?;
            let root = root
                .as_object_mut()
                .ok_or_else(|| GraphStyleError::InvalidObject {
                    path: format!("sheets[{sheet_index}].rootTopic"),
                })?;
            let style = object_field_mut(root, "style", sheet_index, "rootTopic.style")?;
            let properties = object_field_mut(
                style,
                "properties",
                sheet_index,
                "rootTopic.style.properties",
            )?;
            apply_connector_properties(properties, connector);
        }

        if sheet.get("theme").is_some() {
            let sheet = sheet
                .as_object_mut()
                .ok_or_else(|| GraphStyleError::InvalidObject {
                    path: format!("sheets[{sheet_index}]"),
                })?;
            let theme = object_field_mut(sheet, "theme", sheet_index, "theme")?;
            if theme.contains_key("centralTopic") {
                let central_topic =
                    object_field_mut(theme, "centralTopic", sheet_index, "theme.centralTopic")?;
                let properties = object_field_mut(
                    central_topic,
                    "properties",
                    sheet_index,
                    "theme.centralTopic.properties",
                )?;
                apply_connector_properties(properties, connector);
            }
        }
    }

    Ok(())
}

fn apply_connector_properties(properties: &mut Map<String, Value>, patch: &ConnectorStylePatch) {
    if let Some(kind) = &patch.kind {
        properties.insert(
            CONNECTOR_CLASS_FIELD.to_owned(),
            Value::String(kind.xmind_value().to_owned()),
        );
    }
    if let Some(radius) = patch.corner_radius {
        properties.insert(
            CONNECTOR_CORNER_FIELD.to_owned(),
            Value::String(radius.xmind_value()),
        );
    }
    if let Some(width) = &patch.width {
        properties.insert(
            CONNECTOR_WIDTH_FIELD.to_owned(),
            Value::String(width.as_str().to_owned()),
        );
    }
    if let Some(color) = &patch.color {
        properties.insert(
            CONNECTOR_COLOR_FIELD.to_owned(),
            Value::String(color.as_str().to_owned()),
        );
    }
    if let Some(pattern) = &patch.pattern {
        properties.insert(
            CONNECTOR_PATTERN_FIELD.to_owned(),
            Value::String(pattern.xmind_value().to_owned()),
        );
    }
}

fn sheet_at(content: &Value, sheet_index: usize) -> Result<&Value, GraphStyleError> {
    content
        .as_array()
        .and_then(|sheets| sheets.get(sheet_index))
        .ok_or(GraphStyleError::SheetNotFound { index: sheet_index })
}

fn sheet_at_mut(content: &mut Value, sheet_index: usize) -> Result<&mut Value, GraphStyleError> {
    content
        .as_array_mut()
        .and_then(|sheets| sheets.get_mut(sheet_index))
        .ok_or(GraphStyleError::SheetNotFound { index: sheet_index })
}

fn root_topic(sheet: &Value, sheet_index: usize) -> Result<&Value, GraphStyleError> {
    sheet
        .get("rootTopic")
        .filter(|root| root.is_object())
        .ok_or(GraphStyleError::MissingRootTopic { index: sheet_index })
}

fn root_topic_mut(sheet: &mut Value, sheet_index: usize) -> Result<&mut Value, GraphStyleError> {
    sheet
        .get_mut("rootTopic")
        .filter(|root| root.is_object())
        .ok_or(GraphStyleError::MissingRootTopic { index: sheet_index })
}

fn object_field_mut<'a>(
    object: &'a mut Map<String, Value>,
    field: &str,
    sheet_index: usize,
    path: &str,
) -> Result<&'a mut Map<String, Value>, GraphStyleError> {
    if !object.contains_key(field) {
        object.insert(field.to_owned(), json!({}));
    }
    object
        .get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| GraphStyleError::InvalidObject {
            path: format!("sheets[{sheet_index}].{path}"),
        })
}

fn value_as_xmind_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}
