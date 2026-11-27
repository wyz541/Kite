//! Typed topic text and node style access for modern XMind `content.json`.
//!
//! XMind stores local overrides in `topic.style.properties`. This module edits only
//! an allow-list of documented keys and leaves every unknown style property and
//! unknown topic field untouched.
//!
//! Confirmed keys are `fo:*` text properties plus `svg:fill`, `border-line-*`, and
//! `shape-corner`. `fo:line-height` and `fo:letter-spacing` are isolated as
//! experimental mappings because stable serialized examples were not found in the
//! audited XMind 26.05 resources. They round-trip correctly but their XMind 27.01
//! visual consumption is not yet verified.

use std::fmt;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const FONT_FAMILY_FIELD: &str = "fo:font-family";
pub const FONT_SIZE_FIELD: &str = "fo:font-size";
pub const FONT_WEIGHT_FIELD: &str = "fo:font-weight";
pub const FONT_STYLE_FIELD: &str = "fo:font-style";
pub const TEXT_DECORATION_FIELD: &str = "fo:text-decoration";
pub const TEXT_COLOR_FIELD: &str = "fo:color";
pub const TEXT_ALIGNMENT_FIELD: &str = "fo:text-align";
pub const LINE_HEIGHT_FIELD: &str = "fo:line-height";
pub const LETTER_SPACING_FIELD: &str = "fo:letter-spacing";
pub const NODE_BACKGROUND_FIELD: &str = "svg:fill";
pub const NODE_BORDER_COLOR_FIELD: &str = "border-line-color";
pub const NODE_BORDER_WIDTH_FIELD: &str = "border-line-width";
pub const NODE_BORDER_PATTERN_FIELD: &str = "border-line-pattern";
pub const NODE_CORNER_RADIUS_FIELD: &str = "shape-corner";

const MAX_FONT_SIZE_POINTS: f64 = 512.0;
const MAX_NODE_CORNER_RADIUS: u16 = 512;

/// A non-empty font-family expression. Comma-separated fallback lists are preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFamily(String);

impl FontFamily {
    pub fn new(value: impl Into<String>) -> Result<Self, TopicStyleError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(TopicStyleError::EmptyField(FONT_FAMILY_FIELD));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FontFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A finite font size in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontSize(f64);

impl FontSize {
    pub fn points(points: f64) -> Result<Self, TopicStyleError> {
        if !points.is_finite() || points <= 0.0 || points > MAX_FONT_SIZE_POINTS {
            return Err(TopicStyleError::InvalidNumber {
                field: FONT_SIZE_FIELD,
                value: points.to_string(),
            });
        }
        Ok(Self(points))
    }

    pub fn points_value(self) -> f64 {
        self.0
    }

    fn from_xmind_value(value: &Value) -> Option<Self> {
        let text = value_as_xmind_string(value)?;
        let points = text
            .strip_suffix("pt")
            .unwrap_or(&text)
            .trim()
            .parse()
            .ok()?;
        Self::points(points).ok()
    }

    fn xmind_value(self) -> String {
        format!("{}pt", format_number(self.0))
    }
}

/// XMind text weight. Numeric weights and XMind's normal/bold keywords are preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontWeight {
    Inherited,
    Normal,
    Bold,
    Numeric(u16),
    Other(String),
}

impl FontWeight {
    pub fn numeric(value: u16) -> Result<Self, TopicStyleError> {
        if !(1..=1000).contains(&value) {
            return Err(TopicStyleError::InvalidNumber {
                field: FONT_WEIGHT_FIELD,
                value: value.to_string(),
            });
        }
        Ok(Self::Numeric(value))
    }

    fn from_xmind_value(value: &Value) -> Option<Self> {
        let value = value_as_xmind_string(value)?;
        let weight = match value.as_str() {
            "inherited" => Self::Inherited,
            "normal" => Self::Normal,
            "bold" => Self::Bold,
            number => number
                .parse::<u16>()
                .ok()
                .and_then(|number| Self::numeric(number).ok())
                .unwrap_or_else(|| Self::Other(number.to_owned())),
        };
        Some(weight)
    }

    fn xmind_value(&self) -> String {
        match self {
            Self::Inherited => "inherited".to_owned(),
            Self::Normal => "normal".to_owned(),
            Self::Bold => "bold".to_owned(),
            Self::Numeric(value) => value.to_string(),
            Self::Other(value) => value.clone(),
        }
    }
}

/// XMind font slant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Inherited,
    Normal,
    Italic,
    Other(String),
}

impl FontStyle {
    fn from_xmind_value(value: &str) -> Self {
        match value {
            "inherited" => Self::Inherited,
            "normal" => Self::Normal,
            "italic" => Self::Italic,
            value => Self::Other(value.to_owned()),
        }
    }

    fn xmind_value(&self) -> &str {
        match self {
            Self::Inherited => "inherited",
            Self::Normal => "normal",
            Self::Italic => "italic",
            Self::Other(value) => value,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextAlignment {
    Inherited,
    Left,
    Center,
    Right,
    Justify,
    Other(String),
}

impl TextAlignment {
    fn from_xmind_value(value: &str) -> Self {
        match value {
            "inherited" => Self::Inherited,
            "left" => Self::Left,
            "center" => Self::Center,
            "right" => Self::Right,
            "justify" => Self::Justify,
            value => Self::Other(value.to_owned()),
        }
    }

    fn xmind_value(&self) -> &str {
        match self {
            Self::Inherited => "inherited",
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Justify => "justify",
            Self::Other(value) => value,
        }
    }
}

/// Recognized decoration flags. Unknown decoration tokens remain in the JSON value
/// when another decoration flag is changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextDecoration {
    pub underline: bool,
    pub strike: bool,
}

/// A typed color value that preserves future/named XMind colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StyleColor {
    Inherited,
    None,
    Hex(String),
    Named(String),
}

impl StyleColor {
    pub fn new(value: impl Into<String>) -> Result<Self, TopicStyleError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TopicStyleError::EmptyField("color"));
        }
        match trimmed {
            "inherited" => Ok(Self::Inherited),
            "none" => Ok(Self::None),
            value if value.starts_with('#') => {
                if !matches!(value.len(), 4 | 5 | 7 | 9)
                    || !value[1..]
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
                {
                    return Err(TopicStyleError::InvalidColor(value.to_owned()));
                }
                Ok(Self::Hex(value.to_owned()))
            }
            value => Ok(Self::Named(value.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Inherited => "inherited",
            Self::None => "none",
            Self::Hex(value) | Self::Named(value) => value,
        }
    }

    fn from_xmind_value(value: &Value) -> Option<Self> {
        value_as_xmind_string(value).and_then(|value| Self::new(value).ok())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleUnit {
    Unitless,
    Points,
    Pixels,
    Percent,
    Em,
}

/// Typed measurement for node widths and the experimental line-height/letter-spacing
/// properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StyleMeasurement {
    pub value: f64,
    pub unit: StyleUnit,
}

impl StyleMeasurement {
    pub fn new(value: f64, unit: StyleUnit) -> Result<Self, TopicStyleError> {
        if !value.is_finite() {
            return Err(TopicStyleError::InvalidNumber {
                field: "style measurement",
                value: value.to_string(),
            });
        }
        Ok(Self { value, unit })
    }

    pub fn points(value: f64) -> Result<Self, TopicStyleError> {
        Self::new(value, StyleUnit::Points)
    }

    pub fn unitless(value: f64) -> Result<Self, TopicStyleError> {
        Self::new(value, StyleUnit::Unitless)
    }

    fn from_xmind_value(value: &Value) -> Option<Self> {
        let text = value_as_xmind_string(value)?;
        let text = text.trim();
        let (number, unit) = if let Some(number) = text.strip_suffix('%') {
            (number, StyleUnit::Percent)
        } else if let Some(number) = text.strip_suffix("pt") {
            (number, StyleUnit::Points)
        } else if let Some(number) = text.strip_suffix("px") {
            (number, StyleUnit::Pixels)
        } else if let Some(number) = text.strip_suffix("em") {
            (number, StyleUnit::Em)
        } else {
            (text, StyleUnit::Unitless)
        };
        Self::new(number.trim().parse().ok()?, unit).ok()
    }

    fn xmind_value(self) -> String {
        let unit = match self.unit {
            StyleUnit::Unitless => "",
            StyleUnit::Points => "pt",
            StyleUnit::Pixels => "px",
            StyleUnit::Percent => "%",
            StyleUnit::Em => "em",
        };
        format!("{}{unit}", format_number(self.value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BorderLinePattern {
    Inherited,
    None,
    Solid,
    Dash,
    Dot,
    DashDot,
    Other(String),
}

impl BorderLinePattern {
    fn from_xmind_value(value: &str) -> Self {
        match value {
            "inherited" => Self::Inherited,
            "none" => Self::None,
            "solid" => Self::Solid,
            "dash" => Self::Dash,
            "dot" => Self::Dot,
            "dash-dot" => Self::DashDot,
            value => Self::Other(value.to_owned()),
        }
    }

    fn xmind_value(&self) -> &str {
        match self {
            Self::Inherited => "inherited",
            Self::None => "none",
            Self::Solid => "solid",
            Self::Dash => "dash",
            Self::Dot => "dot",
            Self::DashDot => "dash-dot",
            Self::Other(value) => value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeCornerRadius(u16);

impl NodeCornerRadius {
    pub fn new(value: u16) -> Result<Self, TopicStyleError> {
        if value > MAX_NODE_CORNER_RADIUS {
            return Err(TopicStyleError::InvalidNumber {
                field: NODE_CORNER_RADIUS_FIELD,
                value: value.to_string(),
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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopicTextStyle {
    pub font_family: Option<FontFamily>,
    pub font_size: Option<FontSize>,
    pub font_weight: Option<FontWeight>,
    pub font_style: Option<FontStyle>,
    pub decoration: Option<TextDecoration>,
    pub color: Option<StyleColor>,
    pub alignment: Option<TextAlignment>,
    /// Experimental XMind mapping; visual consumption is not yet verified.
    pub line_height: Option<StyleMeasurement>,
    /// Experimental XMind mapping; visual consumption is not yet verified.
    pub letter_spacing: Option<StyleMeasurement>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopicNodeStyle {
    pub background: Option<StyleColor>,
    pub border_color: Option<StyleColor>,
    pub border_width: Option<StyleMeasurement>,
    pub border_pattern: Option<BorderLinePattern>,
    pub corner_radius: Option<NodeCornerRadius>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopicStyle {
    pub text: TopicTextStyle,
    pub node: TopicNodeStyle,
}

/// Partial local text style update. `None` leaves the existing property unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextStylePatch {
    pub font_family: Option<FontFamily>,
    pub font_size: Option<FontSize>,
    pub font_weight: Option<FontWeight>,
    pub font_style: Option<FontStyle>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub color: Option<StyleColor>,
    pub alignment: Option<TextAlignment>,
    pub line_height: Option<StyleMeasurement>,
    pub letter_spacing: Option<StyleMeasurement>,
}

impl TextStylePatch {
    fn is_empty(&self) -> bool {
        self.font_family.is_none()
            && self.font_size.is_none()
            && self.font_weight.is_none()
            && self.font_style.is_none()
            && self.underline.is_none()
            && self.strike.is_none()
            && self.color.is_none()
            && self.alignment.is_none()
            && self.line_height.is_none()
            && self.letter_spacing.is_none()
    }
}

/// Partial local node style update. `None` leaves the existing property unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeStylePatch {
    pub background: Option<StyleColor>,
    pub border_color: Option<StyleColor>,
    pub border_width: Option<StyleMeasurement>,
    pub border_pattern: Option<BorderLinePattern>,
    pub corner_radius: Option<NodeCornerRadius>,
}

impl NodeStylePatch {
    fn is_empty(&self) -> bool {
        self.background.is_none()
            && self.border_color.is_none()
            && self.border_width.is_none()
            && self.border_pattern.is_none()
            && self.corner_radius.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopicStylePatch {
    pub text: TextStylePatch,
    pub node: NodeStylePatch,
}

impl TopicStylePatch {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.node.is_empty()
    }
}

/// What a renderer should do when the requested font is not installed.
///
/// This module never enumerates platform fonts and never rewrites the requested font
/// family in the XMind file as an implicit side effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MissingFontPolicy {
    /// Keep the file's font name and let the platform render a fallback.
    #[default]
    PreserveRequested,
    /// Keep the file's font name but use this family for on-screen layout.
    SubstituteForLayout(FontFamily),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingFontAction {
    PreserveRequested,
    SubstituteForLayout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFontWarning {
    pub requested_family: FontFamily,
    pub persisted_family: FontFamily,
    pub render_family: Option<FontFamily>,
    pub action: MissingFontAction,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFontResolution {
    pub warning: MissingFontWarning,
}

impl MissingFontPolicy {
    /// Resolve a missing font after the caller determines availability.
    ///
    /// The requested family remains the persisted value in every policy branch.
    pub fn resolve_missing(&self, requested: &FontFamily) -> MissingFontResolution {
        match self {
            Self::PreserveRequested => MissingFontResolution {
                warning: MissingFontWarning {
                    requested_family: requested.clone(),
                    persisted_family: requested.clone(),
                    render_family: None,
                    action: MissingFontAction::PreserveRequested,
                    message: format!(
                        "Font \"{requested}\" is not installed; the requested family remains in the file and the platform will choose a fallback"
                    ),
                },
            },
            Self::SubstituteForLayout(fallback) => MissingFontResolution {
                warning: MissingFontWarning {
                    requested_family: requested.clone(),
                    persisted_family: requested.clone(),
                    render_family: Some(fallback.clone()),
                    action: MissingFontAction::SubstituteForLayout,
                    message: format!(
                        "Font \"{requested}\" is not installed; \"{fallback}\" is used for layout while the requested family remains in the file"
                    ),
                },
            },
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopicStyleError {
    #[error("topic must be a JSON object")]
    TopicNotObject,
    #[error("XMind field {path} must be an object")]
    InvalidObject { path: String },
    #[error("XMind field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("invalid numeric value {value:?} for {field}")]
    InvalidNumber { field: &'static str, value: String },
    #[error("invalid XMind color value {0:?}")]
    InvalidColor(String),
}

/// Read local style overrides from one topic. Missing local style returns defaults.
pub fn read_topic_style(topic: &Value) -> Result<TopicStyle, TopicStyleError> {
    let topic = topic.as_object().ok_or(TopicStyleError::TopicNotObject)?;
    let Some(style) = topic.get("style") else {
        return Ok(TopicStyle::default());
    };
    let style = style
        .as_object()
        .ok_or_else(|| TopicStyleError::InvalidObject {
            path: "topic.style".to_owned(),
        })?;
    let Some(properties) = style.get("properties") else {
        return Ok(TopicStyle::default());
    };
    let properties = properties
        .as_object()
        .ok_or_else(|| TopicStyleError::InvalidObject {
            path: "topic.style.properties".to_owned(),
        })?;
    Ok(read_properties(properties))
}

/// Apply a local style patch to exactly one topic.
///
/// The operation is atomic for the supplied JSON value: validation errors leave the
/// topic unchanged. Sibling topics are never visited.
pub fn apply_topic_style(
    topic: &mut Value,
    patch: &TopicStylePatch,
) -> Result<(), TopicStyleError> {
    let mut next = topic.clone();
    apply_topic_style_in_place(&mut next, patch)?;
    *topic = next;
    Ok(())
}

fn apply_topic_style_in_place(
    topic: &mut Value,
    patch: &TopicStylePatch,
) -> Result<(), TopicStyleError> {
    let topic = topic
        .as_object_mut()
        .ok_or(TopicStyleError::TopicNotObject)?;
    if patch.is_empty() {
        return Ok(());
    }

    if !topic.contains_key("style") {
        let seed = topic
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("anonymous-topic");
        topic.insert(
            "style".to_owned(),
            json!({
                "id": stable_style_id(seed),
                "type": "topic",
                "properties": {}
            }),
        );
    }
    let style = topic
        .get_mut("style")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| TopicStyleError::InvalidObject {
            path: "topic.style".to_owned(),
        })?;
    if !style.contains_key("properties") {
        style.insert("properties".to_owned(), json!({}));
    }
    let properties = style
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| TopicStyleError::InvalidObject {
            path: "topic.style.properties".to_owned(),
        })?;

    apply_text_properties(properties, &patch.text);
    apply_node_properties(properties, &patch.node);
    Ok(())
}

fn read_properties(properties: &Map<String, Value>) -> TopicStyle {
    let decoration = properties
        .get(TEXT_DECORATION_FIELD)
        .and_then(Value::as_str)
        .and_then(parse_decoration);
    TopicStyle {
        text: TopicTextStyle {
            font_family: properties
                .get(FONT_FAMILY_FIELD)
                .and_then(Value::as_str)
                .and_then(|value| FontFamily::new(value).ok()),
            font_size: properties
                .get(FONT_SIZE_FIELD)
                .and_then(FontSize::from_xmind_value),
            font_weight: properties
                .get(FONT_WEIGHT_FIELD)
                .and_then(FontWeight::from_xmind_value),
            font_style: properties
                .get(FONT_STYLE_FIELD)
                .and_then(Value::as_str)
                .map(FontStyle::from_xmind_value),
            decoration,
            color: properties
                .get(TEXT_COLOR_FIELD)
                .and_then(StyleColor::from_xmind_value),
            alignment: properties
                .get(TEXT_ALIGNMENT_FIELD)
                .and_then(Value::as_str)
                .map(TextAlignment::from_xmind_value),
            line_height: properties
                .get(LINE_HEIGHT_FIELD)
                .and_then(StyleMeasurement::from_xmind_value),
            letter_spacing: properties
                .get(LETTER_SPACING_FIELD)
                .and_then(StyleMeasurement::from_xmind_value),
        },
        node: TopicNodeStyle {
            background: properties
                .get(NODE_BACKGROUND_FIELD)
                .and_then(StyleColor::from_xmind_value),
            border_color: properties
                .get(NODE_BORDER_COLOR_FIELD)
                .and_then(StyleColor::from_xmind_value),
            border_width: properties
                .get(NODE_BORDER_WIDTH_FIELD)
                .and_then(StyleMeasurement::from_xmind_value),
            border_pattern: properties
                .get(NODE_BORDER_PATTERN_FIELD)
                .and_then(Value::as_str)
                .map(BorderLinePattern::from_xmind_value),
            corner_radius: properties
                .get(NODE_CORNER_RADIUS_FIELD)
                .and_then(NodeCornerRadius::from_xmind_value),
        },
    }
}

fn apply_text_properties(properties: &mut Map<String, Value>, patch: &TextStylePatch) {
    if let Some(font_family) = &patch.font_family {
        properties.insert(
            FONT_FAMILY_FIELD.to_owned(),
            Value::String(font_family.as_str().to_owned()),
        );
    }
    if let Some(font_size) = patch.font_size {
        properties.insert(
            FONT_SIZE_FIELD.to_owned(),
            Value::String(font_size.xmind_value()),
        );
    }
    if let Some(font_weight) = &patch.font_weight {
        properties.insert(
            FONT_WEIGHT_FIELD.to_owned(),
            Value::String(font_weight.xmind_value()),
        );
    }
    if let Some(font_style) = &patch.font_style {
        properties.insert(
            FONT_STYLE_FIELD.to_owned(),
            Value::String(font_style.xmind_value().to_owned()),
        );
    }
    if patch.underline.is_some() || patch.strike.is_some() {
        let current = properties
            .get(TEXT_DECORATION_FIELD)
            .and_then(Value::as_str)
            .unwrap_or("none");
        let mut tokens = current
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            tokens.push("none".to_owned());
        }
        set_decoration_token(&mut tokens, "underline", patch.underline);
        set_decoration_token(&mut tokens, "line-through", patch.strike);
        if tokens.is_empty() {
            tokens.push("none".to_owned());
        }
        properties.insert(
            TEXT_DECORATION_FIELD.to_owned(),
            Value::String(tokens.join(" ")),
        );
    }
    if let Some(color) = &patch.color {
        properties.insert(
            TEXT_COLOR_FIELD.to_owned(),
            Value::String(color.as_str().to_owned()),
        );
    }
    if let Some(alignment) = &patch.alignment {
        properties.insert(
            TEXT_ALIGNMENT_FIELD.to_owned(),
            Value::String(alignment.xmind_value().to_owned()),
        );
    }
    if let Some(line_height) = patch.line_height {
        properties.insert(
            LINE_HEIGHT_FIELD.to_owned(),
            Value::String(line_height.xmind_value()),
        );
    }
    if let Some(letter_spacing) = patch.letter_spacing {
        properties.insert(
            LETTER_SPACING_FIELD.to_owned(),
            Value::String(letter_spacing.xmind_value()),
        );
    }
}

fn apply_node_properties(properties: &mut Map<String, Value>, patch: &NodeStylePatch) {
    if let Some(background) = &patch.background {
        properties.insert(
            NODE_BACKGROUND_FIELD.to_owned(),
            Value::String(background.as_str().to_owned()),
        );
    }
    if let Some(border_color) = &patch.border_color {
        properties.insert(
            NODE_BORDER_COLOR_FIELD.to_owned(),
            Value::String(border_color.as_str().to_owned()),
        );
    }
    if let Some(border_width) = patch.border_width {
        properties.insert(
            NODE_BORDER_WIDTH_FIELD.to_owned(),
            Value::String(border_width.xmind_value()),
        );
    }
    if let Some(border_pattern) = &patch.border_pattern {
        properties.insert(
            NODE_BORDER_PATTERN_FIELD.to_owned(),
            Value::String(border_pattern.xmind_value().to_owned()),
        );
    }
    if let Some(corner_radius) = patch.corner_radius {
        properties.insert(
            NODE_CORNER_RADIUS_FIELD.to_owned(),
            Value::String(corner_radius.value().to_string()),
        );
    }
}

fn parse_decoration(value: &str) -> Option<TextDecoration> {
    let mut recognized = false;
    let mut decoration = TextDecoration {
        underline: false,
        strike: false,
    };
    for token in value.split_whitespace() {
        match token {
            "none" => recognized = true,
            "underline" => {
                decoration.underline = true;
                recognized = true;
            }
            "line-through" => {
                decoration.strike = true;
                recognized = true;
            }
            _ => {}
        }
    }
    recognized.then_some(decoration)
}

fn set_decoration_token(tokens: &mut Vec<String>, token: &str, enabled: Option<bool>) {
    match enabled {
        Some(true) => {
            tokens.retain(|existing| existing != "none" && existing != token);
            tokens.push(token.to_owned());
        }
        Some(false) => {
            tokens.retain(|existing| existing != token);
            if tokens.iter().all(|existing| existing == "none") {
                tokens.clear();
            }
        }
        None => {}
    }
}

fn stable_style_id(seed: &str) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("kite-topic-style|{seed}").as_bytes())
    );
    digest[..32].to_owned()
}

fn value_as_xmind_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_decoration_merge_preserves_unknown_tokens() {
        let mut tokens = vec![
            "overline".to_owned(),
            "underline".to_owned(),
            "line-through".to_owned(),
        ];
        set_decoration_token(&mut tokens, "underline", Some(false));
        assert_eq!(tokens, ["overline", "line-through"]);
        set_decoration_token(&mut tokens, "underline", Some(true));
        assert_eq!(tokens, ["overline", "line-through", "underline"]);
    }

    #[test]
    fn missing_font_policies_preserve_persisted_family() {
        let requested = FontFamily::new("Kite Industrial Mono").unwrap();
        let fallback = FontFamily::new("JetBrains Mono").unwrap();
        let preserve = MissingFontPolicy::PreserveRequested.resolve_missing(&requested);
        assert_eq!(preserve.warning.persisted_family, requested);
        assert!(preserve.warning.render_family.is_none());

        let substitute =
            MissingFontPolicy::SubstituteForLayout(fallback.clone()).resolve_missing(&requested);
        assert_eq!(substitute.warning.persisted_family, requested);
        assert_eq!(substitute.warning.render_family, Some(fallback));
    }
}
