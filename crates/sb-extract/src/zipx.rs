//! Bounded zip reading and a small XML event layer for OOXML.

use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::{ExtractError, ExtractLimits};

pub(crate) struct Archive<'a> {
    z: zip::ZipArchive<Cursor<&'a [u8]>>,
    remaining: u64,
}

impl<'a> Archive<'a> {
    pub fn open(bytes: &'a [u8], limits: &ExtractLimits) -> Result<Self, ExtractError> {
        let mut z = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| ExtractError::Corrupt(format!("not a valid zip container: {e}")))?;
        if z.len() > limits.max_entries {
            return Err(ExtractError::TooLarge(format!(
                "{} archive members (limit {})",
                z.len(),
                limits.max_entries
            )));
        }
        let mut declared = 0u64;
        for i in 0..z.len() {
            let f = z
                .by_index_raw(i)
                .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
            declared = declared.saturating_add(f.size());
        }
        if declared > limits.max_decompressed_bytes {
            return Err(ExtractError::TooLarge(format!(
                "{declared} bytes when decompressed (limit {})",
                limits.max_decompressed_bytes
            )));
        }
        Ok(Archive {
            z,
            remaining: limits.max_decompressed_bytes,
        })
    }

    pub fn names(&self) -> Vec<String> {
        self.z.file_names().map(str::to_string).collect()
    }

    /// Read one member; `None` if it does not exist. The declared size is not
    /// trusted: reading stops at the remaining budget.
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, ExtractError> {
        let f = match self.z.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(ExtractError::Corrupt(e.to_string())),
        };
        if f.encrypted() {
            return Err(ExtractError::Encrypted);
        }
        let mut buf = Vec::new();
        let n = f
            .take(self.remaining.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| ExtractError::Corrupt(e.to_string()))? as u64;
        if n > self.remaining {
            return Err(ExtractError::TooLarge(
                "decompressed size above the limit".into(),
            ));
        }
        self.remaining -= n;
        Ok(Some(buf))
    }
}

/// A simplified XML event: names are local names (no prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ev {
    Start(String, Vec<(String, String)>),
    End(String),
    Empty(String, Vec<(String, String)>),
    Text(String),
}

pub(crate) fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse a whole XML part into events. Entities are resolved.
pub(crate) fn events(xml: &[u8]) -> Result<Vec<Ev>, ExtractError> {
    let mut r = Reader::from_reader(xml);
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ev = r
            .read_event_into(&mut buf)
            .map_err(|e| ExtractError::Corrupt(format!("xml: {e}")))?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => out.push(Ev::Start(local(&e), attrs(&e))),
            Event::Empty(e) => out.push(Ev::Empty(local(&e), attrs(&e))),
            Event::End(e) => out.push(Ev::End(e.local_name().as_ref().to_string())),
            Event::Text(t) => out.push(Ev::Text(t.xml10_content().into_owned())),
            Event::CData(t) => out.push(Ev::Text(t.xml10_content().into_owned())),
            Event::GeneralRef(g) => {
                let name = g.into_inner();
                let s = match name.as_ref() {
                    "lt" => "<".to_string(),
                    "gt" => ">".to_string(),
                    "amp" => "&".to_string(),
                    "quot" => "\"".to_string(),
                    "apos" => "'".to_string(),
                    n => match n.strip_prefix('#') {
                        Some(num) => {
                            let code = match num.strip_prefix('x') {
                                Some(h) => u32::from_str_radix(h, 16).ok(),
                                None => num.parse().ok(),
                            };
                            code.and_then(char::from_u32)
                                .map(String::from)
                                .unwrap_or_default()
                        }
                        None => String::new(),
                    },
                };
                out.push(Ev::Text(s));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn local(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.local_name().as_ref().to_string()
}

fn attrs(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .filter_map(Result::ok)
        .map(|a| {
            (
                a.key.local_name().as_ref().to_string(),
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map(|v| v.into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Title, created and modified from `docProps/core.xml`.
pub(crate) fn core_props(
    a: &mut Archive<'_>,
) -> (
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
) {
    let Ok(Some(xml)) = a.read("docProps/core.xml") else {
        return (None, None, None);
    };
    let Ok(evs) = events(&xml) else {
        return (None, None, None);
    };
    let (mut title, mut created, mut modified) = (None, None, None);
    let mut cur: Option<String> = None;
    for ev in evs {
        match ev {
            Ev::Start(n, _) => cur = Some(n),
            Ev::End(_) => cur = None,
            Ev::Text(t) => match cur.as_deref() {
                Some("title") if !t.trim().is_empty() => title = Some(t),
                Some("created") => created = crate::parse_time(&t),
                Some("modified") => modified = crate::parse_time(&t),
                _ => {}
            },
            Ev::Empty(..) => {}
        }
    }
    (title, created, modified)
}
