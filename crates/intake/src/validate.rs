// SPDX-License-Identifier: Apache-2.0
//! Bounded validation after deserialization; no diagnostic values in errors.
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use protocol::reports::*;
use serde_json::Value;

pub fn code(value: &str) -> Result<()> {
    if value.chars().count() > MAX_REPORT_CODE || value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(())
}

/// A signature key consists of two individually bounded identifiers.
pub fn signature_key(value: &str) -> Result<()> {
    if value.len() > (MAX_REPORT_CODE * 2 + 1) * 4 {
        return Err(Error::Invalid);
    }
    // Crash codes can themselves contain @. Accept any bounded split.
    if value
        .match_indices('@')
        .any(|(at, _)| code(&value[..at]).is_ok() && code(&value[at + 1..]).is_ok())
    {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

pub fn bundle(bundle: &ReportBundle) -> Result<()> {
    if bundle.schema != REPORT_SCHEMA
        || bundle
            .words
            .as_ref()
            .is_some_and(|s| s.chars().count() > MAX_REPORT_WORDS)
        || bundle.phone_trail.len() > MAX_TRAIL_EVENTS
        || bundle.computer_trail.len() > MAX_TRAIL_EVENTS
        || bundle.crashes.iter().any(|c| c.frames.len() > MAX_FRAMES)
        || serde_json::to_vec(bundle)?.len() > MAX_REPORT_BYTES
    {
        return Err(Error::Invalid);
    }
    if let Some(test) = &bundle.test {
        test.check().map_err(|_| Error::Invalid)?;
    }
    let mut structured = serde_json::to_value(bundle)?;
    // Free text, picture bytes and a submitted test have separate bounds.
    let object = structured.as_object_mut().ok_or(Error::Invalid)?;
    object.remove("words");
    object.remove("screenshot");
    object.remove("test");
    if let Some(Value::Array(extras)) = object.get_mut("extras") {
        for extra in extras {
            if let Some(entries) = extra.get_mut("entries").and_then(Value::as_array_mut) {
                for entry in entries {
                    entry.as_object_mut().ok_or(Error::Invalid)?.remove("text");
                }
            }
        }
    }
    identifiers(&structured)?;
    if let Some(screenshot) = &bundle.screenshot {
        picture(screenshot)?;
    }
    Ok(())
}

fn identifiers(value: &Value) -> Result<()> {
    match value {
        Value::String(s) => code(s),
        Value::Array(values) => {
            for value in values {
                identifiers(value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                code(key)?;
                identifiers(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A check as a worker records it: a full main commit and, when given, a
/// bounded `path:line` failure site of plain path characters.
pub fn check(check: &crate::maintainer::TestCheck) -> Result<()> {
    if !valid_source_commit(&check.main_commit)
        || check.failure_site.as_ref().is_some_and(|site| {
            site.is_empty()
                || site.len() > crate::maintainer::MAX_FAILURE_SITE
                || !site
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.:".contains(&b))
        })
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub fn question(template: &QuestionTemplate) -> Result<()> {
    match template {
        QuestionTemplate::StillHappens { build } => code(build)?,
        QuestionTemplate::Expected { options } if options.is_empty() || options.len() > 5 => {
            return Err(Error::Invalid);
        }
        _ => {}
    }
    Ok(())
}

pub fn progress(progress: &ReportProgress) -> Result<()> {
    match progress {
        ReportProgress::Known {
            fixed_in: Some(build),
        }
        | ReportProgress::Fixed { build }
        | ReportProgress::Shipped { build } => code(build),
        _ => Ok(()),
    }
}

pub fn picture(picture: &ReportScreenshot) -> Result<()> {
    if picture.data.len() > MAX_SCREENSHOT_BYTES.div_ceil(3) * 4 {
        return Err(Error::Invalid);
    }
    let bytes = STANDARD.decode(&picture.data).map_err(|_| Error::Invalid)?;
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(Error::Invalid);
    }
    let dimensions = match picture.media_type.as_str() {
        "image/png"
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") =>
        {
            Some((be32(&bytes, 16)?, be32(&bytes, 20)?))
        }
        "image/jpeg" if bytes.starts_with(&[0xff, 0xd8, 0xff]) => jpeg_size(&bytes),
        "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            webp_size(&bytes)
        }
        _ => None,
    }
    .ok_or(Error::Invalid)?;
    let (width, height) = dimensions;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 32_000_000
        || dimensions != (picture.width, picture.height)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn be32(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(Error::Invalid)?
            .try_into()
            .map_err(|_| Error::Invalid)?,
    ))
}
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xff {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xff {
            at += 1;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 || at + 2 + length > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let segment = bytes.get(at + 4..at + 2 + length)?;
            if segment.len() < 5 {
                return None;
            }
            return Some((
                u32::from(u16::from_be_bytes([segment[3], segment[4]])),
                u32::from(u16::from_be_bytes([segment[1], segment[2]])),
            ));
        }
        at += 2 + length;
    }
    None
}
fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let size = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    if size.checked_add(8)? != bytes.len() {
        return None;
    }
    let data = bytes.get(20..)?;
    match bytes.get(12..16)? {
        b"VP8X" if data.len() >= 10 => {
            let n = |at| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], 0]) + 1;
            Some((n(4), n(7)))
        }
        b"VP8 " if data.len() >= 10 && data.get(3..6)? == [0x9d, 0x01, 0x2a] => Some((
            u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3fff),
            u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3fff),
        )),
        b"VP8L" if data.len() >= 5 && data[0] == 0x2f => {
            let bits = u32::from_le_bytes(data[1..5].try_into().ok()?);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_bounded_by_characters_and_reject_controls() {
        assert!(code(&"é".repeat(MAX_REPORT_CODE)).is_ok());
        assert!(code(&"a".repeat(MAX_REPORT_CODE + 1)).is_err());
        assert!(code("a\nb").is_err());
        assert!(
            signature_key(&format!(
                "{}@{}",
                "a".repeat(MAX_REPORT_CODE),
                "b".repeat(MAX_REPORT_CODE)
            ))
            .is_ok()
        );
    }
    #[test]
    fn pictures_check_magic_dimensions_encoding_and_bounds() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        let mut p = ReportScreenshot {
            media_type: "image/png".into(),
            data: STANDARD.encode(bytes),
            width: 1,
            height: 2,
            marked: false,
        };
        assert!(picture(&p).is_ok());
        p.width = 2;
        assert!(picture(&p).is_err());
        p.width = 1;
        p.media_type = "image/jpeg".into();
        assert!(picture(&p).is_err());
        p.data = "%%%%".into();
        assert!(picture(&p).is_err());
        assert!(jpeg_size(&[0xff, 0xd8, 0xff]).is_none());
        assert!(webp_size(b"RIFFxxxxWEBP").is_none());
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    fn fixture() -> ReportBundle {
        serde_json::from_value(serde_json::json!({
            "schema":REPORT_SCHEMA,"report_id":protocol::ReportId::new(),"created_at":0,
            "origin":{"type":"settings"},"signature":{"code":"empty","screen":"/work/:task","build":"test-1"},
            "versions":{},"context":{"screen":"/work/:task"}
        })).unwrap()
    }
    #[test]
    fn bundles_distinguish_free_text_from_bounded_diagnostics() {
        let mut report = fixture();
        report.words = Some("é".repeat(MAX_REPORT_WORDS));
        assert!(bundle(&report).is_ok());
        report.words.as_mut().unwrap().push('é');
        assert!(bundle(&report).is_err());
        report.words = None;
        report.extras.push(ReportExtraPart::Names {
            names: vec![NamedPseudonym {
                pseudonym: "t:12345678".into(),
                name: "x".repeat(MAX_REPORT_CODE + 1),
            }],
        });
        assert!(bundle(&report).is_err());
        report.extras.clear();
        report.extras.push(ReportExtraPart::TaskTranscript {
            task: "t:12345678".into(),
            entries: vec![TranscriptEntry {
                at: protocol::time::Timestamp(0),
                role: "person".into(),
                text: "x".repeat(MAX_REPORT_CODE + 1),
            }],
            cut: false,
        });
        assert!(bundle(&report).is_ok());
        report.state = Some(StateShape {
            version: 1,
            store: serde_json::json!({"bad\nkey":true}),
        });
        assert!(bundle(&report).is_err());
        report.state = None;
        report.context.command = Some("x".repeat(MAX_REPORT_CODE + 1));
        assert!(bundle(&report).is_err());
        report.context.command = None;
        report.schema = REPORT_SCHEMA + 1;
        assert!(bundle(&report).is_err());
    }
    #[test]
    fn the_bundle_limit_includes_consented_extras() {
        let mut report = fixture();
        report.extras.push(ReportExtraPart::TaskTranscript {
            task: "t:12345678".into(),
            entries: vec![TranscriptEntry {
                at: protocol::time::Timestamp(0),
                role: "person".into(),
                text: "x".repeat(MAX_REPORT_BYTES),
            }],
            cut: false,
        });
        assert!(bundle(&report).is_err());
    }
    #[test]
    fn templates_and_build_statuses_keep_bounded_identifiers() {
        assert!(question(&QuestionTemplate::Expected { options: vec![] }).is_err());
        assert!(
            question(&QuestionTemplate::Expected {
                options: vec![ExpectedOption::ShouldWork]
            })
            .is_ok()
        );
        assert!(
            question(&QuestionTemplate::StillHappens {
                build: "a\nb".into()
            })
            .is_err()
        );
        assert!(
            progress(&ReportProgress::Fixed {
                build: "x".repeat(MAX_REPORT_CODE + 1)
            })
            .is_err()
        );
        assert!(progress(&ReportProgress::Known { fixed_in: None }).is_ok());
        assert!(
            signature_key(&format!(
                "{}@{}",
                "js:name@fn".repeat(10),
                "/".repeat(MAX_REPORT_CODE)
            ))
            .is_ok()
        );
    }
    #[test]
    fn jpeg_and_all_webp_headers_check_the_encoded_dimensions() {
        let jpeg = vec![0xff, 0xd8, 0xff, 0xc0, 0, 7, 8, 0, 2, 0, 1];
        let screenshot = |media: &str, bytes: Vec<u8>| ReportScreenshot {
            media_type: media.into(),
            data: STANDARD.encode(bytes),
            width: 1,
            height: 2,
            marked: false,
        };
        assert!(picture(&screenshot("image/jpeg", jpeg.clone())).is_ok());
        assert!(picture(&screenshot("image/png", jpeg)).is_err());
        for (kind, payload) in [
            (b"VP8X", vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0]),
            (b"VP8 ", vec![0, 0, 0, 0x9d, 1, 0x2a, 1, 0, 2, 0]),
            (b"VP8L", vec![0x2f, 0, 0x40, 0, 0]),
        ] {
            let mut webp = b"RIFF".to_vec();
            webp.extend_from_slice(&(12u32 + payload.len() as u32).to_le_bytes());
            webp.extend_from_slice(b"WEBP");
            webp.extend_from_slice(kind);
            webp.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            webp.extend_from_slice(&payload);
            assert!(picture(&screenshot("image/webp", webp.clone())).is_ok());
            webp[4] = 0;
            assert!(picture(&screenshot("image/webp", webp)).is_err());
        }
    }
}
