//! Static date values and deterministic insertion into XMind topic text.
//!
//! This slice intentionally writes dates as ordinary, editable title or note text.
//! XMind's structured task dates (`taskStartDate` / `taskDueDate`) have different
//! semantics and are not inferred by this module. Title and `notes.plain.content`
//! are stable fields in modern `content.json`.

use chrono::{Datelike, Local, NaiveDate, NaiveTime, Timelike};
use serde_json::{Map, Value, json};
use thiserror::Error;

const MONTHS_EN_US: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A fixed calendar date with an optional wall-clock time.
///
/// The value is deliberately timezone-free. A UI that wants "today" can call
/// [`StaticDateValue::today_local`], then persist the resulting static value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticDateValue {
    date: NaiveDate,
    time: Option<NaiveTime>,
}

impl StaticDateValue {
    pub fn new(date: NaiveDate) -> Self {
        Self { date, time: None }
    }

    pub fn with_time(mut self, time: NaiveTime) -> Self {
        self.time = Some(time);
        self
    }

    pub fn today_local() -> Self {
        let now = Local::now();
        Self {
            date: now.date_naive(),
            time: Some(now.time().with_nanosecond(0).unwrap_or(now.time())),
        }
    }

    pub fn date(&self) -> NaiveDate {
        self.date
    }

    pub fn time(&self) -> Option<NaiveTime> {
        self.time
    }

    pub fn format(&self, format: DateDisplayFormat) -> Result<String, DateFormatError> {
        if format.precision.requires_time() && self.time.is_none() {
            return Err(DateFormatError::MissingTime);
        }
        if format.precision == DatePrecision::DateOnly {
            return Ok(self.format_date(format.style));
        }

        let time = self.time.expect("precision was validated above");
        let date = self.format_date(format.style);
        let time = self.format_time(time, format);
        Ok(format!("{date} {time}"))
    }

    fn format_date(&self, style: DateStyle) -> String {
        match style {
            DateStyle::Iso => format!(
                "{:04}-{:02}-{:02}",
                self.date.year(),
                self.date.month(),
                self.date.day()
            ),
            DateStyle::Locale(DateLocale::Chinese) => format!(
                "{}年{}月{}日",
                self.date.year(),
                self.date.month(),
                self.date.day()
            ),
            DateStyle::Locale(DateLocale::EnglishUnitedStates) => format!(
                "{} {}, {}",
                MONTHS_EN_US[(self.date.month0() as usize).min(11)],
                self.date.day(),
                self.date.year()
            ),
            DateStyle::System(pattern) => {
                let year = format!("{:04}", self.date.year());
                let month = format!("{:02}", self.date.month());
                let day = format!("{:02}", self.date.day());
                let separator = pattern.separator;
                match pattern.order {
                    DatePartOrder::YearMonthDay => {
                        format!("{year}{separator}{month}{separator}{day}")
                    }
                    DatePartOrder::MonthDayYear => {
                        format!("{month}{separator}{day}{separator}{year}")
                    }
                    DatePartOrder::DayMonthYear => {
                        format!("{day}{separator}{month}{separator}{year}")
                    }
                }
            }
        }
    }

    fn format_time(&self, time: NaiveTime, format: DateDisplayFormat) -> String {
        let include_seconds = format.precision == DatePrecision::Seconds;
        if format.style == DateStyle::Locale(DateLocale::EnglishUnitedStates) {
            let hour = match time.hour() % 12 {
                0 => 12,
                hour => hour,
            };
            let period = if time.hour() < 12 { "AM" } else { "PM" };
            if include_seconds {
                format!("{hour}:{:02}:{:02} {period}", time.minute(), time.second())
            } else {
                format!("{hour}:{:02} {period}", time.minute())
            }
        } else if include_seconds {
            format!(
                "{:02}:{:02}:{:02}",
                time.hour(),
                time.minute(),
                time.second()
            )
        } else {
            format!("{:02}:{:02}", time.hour(), time.minute())
        }
    }
}

/// How much of a [`StaticDateValue`] to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePrecision {
    DateOnly,
    Minutes,
    Seconds,
}

impl DatePrecision {
    fn requires_time(self) -> bool {
        self != Self::DateOnly
    }
}

/// Locale presets supported without platform-specific data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateLocale {
    Chinese,
    EnglishUnitedStates,
}

/// Date part order resolved from the host system locale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePartOrder {
    YearMonthDay,
    MonthDayYear,
    DayMonthYear,
}

/// A numeric system date pattern supplied by the host UI after resolving its locale.
///
/// Keeping the resolved pattern in the request makes Core deterministic and avoids
/// silently changing a document when the machine locale changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemDatePattern {
    pub order: DatePartOrder,
    pub separator: char,
}

/// Date rendering style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateStyle {
    Iso,
    Locale(DateLocale),
    System(SystemDatePattern),
}

/// Complete date display request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateDisplayFormat {
    pub style: DateStyle,
    pub precision: DatePrecision,
}

impl DateDisplayFormat {
    pub const ISO_DATE: Self = Self {
        style: DateStyle::Iso,
        precision: DatePrecision::DateOnly,
    };

    pub const ISO_DATE_TIME: Self = Self {
        style: DateStyle::Iso,
        precision: DatePrecision::Minutes,
    };

    pub const fn locale(locale: DateLocale, precision: DatePrecision) -> Self {
        Self {
            style: DateStyle::Locale(locale),
            precision,
        }
    }

    pub const fn system(pattern: SystemDatePattern, precision: DatePrecision) -> Self {
        Self {
            style: DateStyle::System(pattern),
            precision,
        }
    }
}

/// Where an inserted date is written. The target must always be explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateInsertionTarget {
    Title,
    Note,
}

/// Where in the selected text field the date is inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateInsertionPlacement {
    Start,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateInsertionRequest {
    pub value: StaticDateValue,
    pub format: DateDisplayFormat,
    pub target: DateInsertionTarget,
    pub placement: DateInsertionPlacement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateInsertionResult {
    pub text: String,
    pub target: DateInsertionTarget,
    pub placement: DateInsertionPlacement,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DateFormatError {
    #[error("selected date format requires a time value")]
    MissingTime,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DateInsertionError {
    #[error("topic must be a JSON object")]
    TopicNotObject,
    #[error("topic.title must be a string")]
    TitleNotString,
    #[error("XMind field {path} must be an object")]
    InvalidObject { path: String },
    #[error("XMind field {path} must be a string")]
    InvalidString { path: String },
    #[error(transparent)]
    Format(#[from] DateFormatError),
}

/// Insert a rendered static date into one topic without touching sibling topics.
///
/// For notes this uses XMind's stable `notes.plain.content` field. Existing note
/// fields are retained; missing `notes` / `plain` objects are created only after all
/// target validation succeeds.
pub fn insert_date_into_topic(
    topic: &mut Value,
    request: &DateInsertionRequest,
) -> Result<DateInsertionResult, DateInsertionError> {
    let rendered = request.value.format(request.format)?;
    insert_text_into_topic(topic, &rendered, request.target, request.placement)?;
    Ok(DateInsertionResult {
        text: rendered,
        target: request.target,
        placement: request.placement,
    })
}

/// Insert already-rendered, editable text into a topic field.
///
/// This is used by UI layers that need to make date intent explicit without
/// claiming an XMind structured date mapping (for example, a text-only due label).
pub fn insert_text_into_topic(
    topic: &mut Value,
    rendered: &str,
    target: DateInsertionTarget,
    placement: DateInsertionPlacement,
) -> Result<(), DateInsertionError> {
    let mut next = topic.clone();
    insert_text_in_place(&mut next, rendered, target, placement)?;
    *topic = next;
    Ok(())
}

fn insert_text_in_place(
    topic: &mut Value,
    rendered: &str,
    target: DateInsertionTarget,
    placement: DateInsertionPlacement,
) -> Result<(), DateInsertionError> {
    let topic = topic
        .as_object_mut()
        .ok_or(DateInsertionError::TopicNotObject)?;
    match target {
        DateInsertionTarget::Title => {
            let title = topic
                .get("title")
                .and_then(Value::as_str)
                .ok_or(DateInsertionError::TitleNotString)?;
            topic.insert(
                "title".to_owned(),
                Value::String(join_text(title, rendered, placement, " ")),
            );
        }
        DateInsertionTarget::Note => {
            let notes = object_field_or_insert(topic, "notes", "notes")?;
            let plain = object_field_or_insert(notes, "plain", "notes.plain")?;
            let current = plain.get("content").and_then(Value::as_str).unwrap_or("");
            plain.insert(
                "content".to_owned(),
                Value::String(join_text(current, rendered, placement, "\n")),
            );
        }
    }
    Ok(())
}

fn join_text(
    existing: &str,
    rendered: &str,
    placement: DateInsertionPlacement,
    separator: &str,
) -> String {
    if existing.is_empty() {
        return rendered.to_owned();
    }
    match placement {
        DateInsertionPlacement::Start => format!("{rendered}{separator}{existing}"),
        DateInsertionPlacement::End => format!("{existing}{separator}{rendered}"),
    }
}

fn object_field_or_insert<'a>(
    object: &'a mut Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<&'a mut Map<String, Value>, DateInsertionError> {
    if !object.contains_key(field) {
        object.insert(field.to_owned(), json!({}));
    }
    object
        .get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| DateInsertionError::InvalidObject {
            path: path.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::*;

    fn date() -> StaticDateValue {
        StaticDateValue::new(NaiveDate::from_ymd_opt(2026, 11, 27).unwrap())
            .with_time(NaiveTime::from_hms_opt(14, 5, 9).unwrap())
    }

    #[test]
    fn formats_iso_and_locale_dates_deterministically() {
        assert_eq!(
            date().format(DateDisplayFormat::ISO_DATE).unwrap(),
            "2026-11-27"
        );
        assert_eq!(
            date().format(DateDisplayFormat::ISO_DATE_TIME).unwrap(),
            "2026-11-27 14:05"
        );
        assert_eq!(
            date()
                .format(DateDisplayFormat::locale(
                    DateLocale::Chinese,
                    DatePrecision::Seconds
                ))
                .unwrap(),
            "2026年11月27日 14:05:09"
        );
        assert_eq!(
            date()
                .format(DateDisplayFormat::locale(
                    DateLocale::EnglishUnitedStates,
                    DatePrecision::Minutes
                ))
                .unwrap(),
            "Nov 27, 2026 2:05 PM"
        );
    }

    #[test]
    fn system_pattern_is_caller_resolved_and_deterministic() {
        let format = DateDisplayFormat::system(
            SystemDatePattern {
                order: DatePartOrder::DayMonthYear,
                separator: '/',
            },
            DatePrecision::DateOnly,
        );
        assert_eq!(date().format(format).unwrap(), "27/11/2026");
    }
}
