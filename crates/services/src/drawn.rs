// SPDX-License-Identifier: Apache-2.0
//! What an agent reads of a drawing the person sent. The phone keeps every
//! point of a freehand stroke at full precision with its time, which a
//! screen of scribbles turns into tens of thousands of characters the agent
//! reads again on every later turn of its session. The agent gets the
//! pictures themselves, so it needs only where each mark is, what was said
//! about it, and the ids its evidence names: strokes thinned to their shape,
//! coordinates to three places, and nothing that only the app uses.
use protocol::annotation::{AnnotationBundle, MarkShape};
use serde_json::{Value, json};

/// The most points a thinned stroke keeps.
const STROKE_POINTS: usize = 48;
/// How far, as a fraction of the picture, a thinned stroke may stray from
/// what was drawn.
const STRAY: f64 = 0.002;

/// `bundle` as the agent reads it.
pub(crate) fn for_agent(bundle: &AnnotationBundle) -> Value {
    let label = |id: &protocol::ids::MarkId| {
        (bundle.marks.iter())
            .find(|m| m.mark_id == *id)
            .map_or_else(|| id.to_string(), |m| m.label.clone())
    };
    let mut view = json!({
        "bundle_id": bundle.bundle_id,
        "revision": bundle.revision,
        "sources": bundle.sources.iter().map(|s| json!({
            "key": s.key,
            "media_type": s.media_type,
            "width": s.width,
            "height": s.height,
            "origin": s.origin,
            "derived_from": s.derived_from,
            "derivation": s.derivation,
        })).collect::<Vec<_>>(),
        "marks": bundle.marks.iter().map(|m| {
            let mut mark = json!({
                "mark_id": m.mark_id,
                "label": m.label,
                "source_key": m.source_key,
                "shape": shape(&m.shape),
            });
            if m.deleted {
                mark["deleted"] = true.into();
            }
            mark
        }).collect::<Vec<_>>(),
        "comments": bundle.comments.iter().map(|c| {
            let mut comment = json!({"text": c.text});
            if !c.mark_ids.is_empty() {
                comment["marks"] = c.mark_ids.iter().map(label).collect();
            }
            if let Some(moment) = &c.moment {
                comment["moment"] = json!(moment);
            }
            comment
        }).collect::<Vec<_>>(),
        "steps": bundle.steps,
        "general_note": bundle.general_note,
    });
    if let Value::Object(fields) = &mut view {
        fields.retain(|_, v| !(v.is_null() || v.as_array().is_some_and(Vec::is_empty)));
    }
    round(&mut view);
    view
}

/// A mark's shape, its stroke thinned and without the drawing clock, which
/// only lines speech up with drawing on the phone.
fn shape(shape: &MarkShape) -> Value {
    let at = |p: &protocol::annotation::StrokePoint| json!([p.x, p.y]);
    match shape {
        MarkShape::Stroke { points } => {
            let xy: Vec<_> = points.iter().map(|p| (p.x, p.y)).collect();
            json!({"kind": "stroke", "points": thin(&xy).iter().map(|&(x, y)| json!([x, y])).collect::<Vec<_>>()})
        }
        MarkShape::Rect { rect } => json!({"kind": "rect", "rect": rect}),
        MarkShape::Ellipse { rect } => json!({"kind": "ellipse", "rect": rect}),
        MarkShape::Arrow { from, to } => json!({"kind": "arrow", "from": at(from), "to": at(to)}),
        MarkShape::Pin { at: p } => json!({"kind": "pin", "at": at(p)}),
    }
}

/// `points` with those dropped that leave its shape within [`STRAY`]
/// (Ramer–Douglas–Peucker), then, if still longer, every few kept so that
/// about [`STROKE_POINTS`] remain, its ends included.
fn thin(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut spans = vec![(0, points.len() - 1)];
    while let Some((a, b)) = spans.pop() {
        let (far, distance) = ((a + 1)..b)
            .map(|i| (i, off_line(points[i], points[a], points[b])))
            .fold(
                (a, 0.0),
                |best, next| if next.1 > best.1 { next } else { best },
            );
        if distance > STRAY {
            keep[far] = true;
            spans.push((a, far));
            spans.push((far, b));
        }
    }
    let kept: Vec<_> = (points.iter().zip(keep))
        .filter_map(|(p, k)| k.then_some(*p))
        .collect();
    if kept.len() <= STROKE_POINTS {
        return kept;
    }
    let step = (kept.len() - 1) as f64 / (STROKE_POINTS - 1) as f64;
    (0..STROKE_POINTS)
        .map(|i| kept[((i as f64 * step).round() as usize).min(kept.len() - 1)])
        .collect()
}

/// How far `p` lies from the segment `a`–`b`.
fn off_line(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0)
    };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Every fraction in `value` to three places: a thousandth of the picture
/// is a pixel or two, and the seventeen digits the phone sends are noise.
fn round(value: &mut Value) {
    match value {
        Value::Number(n) if n.is_f64() => {
            if let Some(r) = n
                .as_f64()
                .and_then(|f| serde_json::Number::from_f64((f * 1000.0).round() / 1000.0))
            {
                *n = r;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(round),
        Value::Object(fields) => fields.values_mut().for_each(round),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{annotation::*, ids::*, time::Timestamp};

    fn bundle(marks: Vec<Mark>, comments: Vec<Comment>) -> AnnotationBundle {
        AnnotationBundle {
            bundle_id: AnnotationBundleId::new(),
            revision: 2,
            sources: vec![AnnotationSource {
                key: "S1".into(),
                artifact_id: ArtifactId::new(),
                sha256: "0".repeat(64),
                media_type: "image/png".into(),
                width: 1080,
                height: 2400,
                origin: SourceOrigin::PhoneImport {
                    original_name: None,
                },
                captured_at: Timestamp(123),
                derived_from: None,
                derivation: None,
            }],
            marks,
            comments,
            steps: vec![],
            general_note: None,
            speech: None,
        }
    }

    fn mark(label: &str, shape: MarkShape) -> Mark {
        Mark {
            mark_id: MarkId::new(),
            label: label.into(),
            source_key: "S1".into(),
            shape,
            color: 3,
            created_at: Timestamp(456),
            deleted: false,
        }
    }

    /// A circle drawn the way a phone reports it: a point every pointer
    /// move, at full precision, each with its time.
    fn circle() -> MarkShape {
        MarkShape::Stroke {
            points: (0..=240)
                .map(|i| {
                    let a = f64::from(i) / 240.0 * std::f64::consts::TAU;
                    StrokePoint {
                        x: 0.4 + 0.1 * a.cos() + 0.000_123_456_789,
                        y: 0.6 + 0.05 * a.sin() - 0.000_987_654_321,
                        t: Some(16 * i),
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn a_scribble_reaches_the_agent_as_its_shape_not_every_point() {
        let drawn = bundle(vec![mark("A", circle()), mark("B", circle())], Vec::new());
        let raw = serde_json::to_string(&drawn).unwrap();
        let view = for_agent(&drawn);
        let read = serde_json::to_string(&view).unwrap();
        assert!(
            read.len() * 8 < raw.len(),
            "{} of {}",
            read.len(),
            raw.len()
        );
        for mark in view["marks"].as_array().unwrap() {
            let points = mark["shape"]["points"].as_array().unwrap();
            assert!(
                points.len() >= 12 && points.len() <= STROKE_POINTS,
                "{}",
                points.len()
            );
            // It still goes all the way round, and starts where it was drawn.
            assert_eq!(points[0], json!([0.5, 0.599]));
            let xs: Vec<_> = points.iter().map(|p| p[0].as_f64().unwrap()).collect();
            assert!(xs.iter().any(|&x| x < 0.31) && xs.iter().any(|&x| x > 0.49));
        }
        assert!(!read.contains("\"t\""), "{read}");
    }

    #[test]
    fn the_agent_keeps_what_its_evidence_names_and_what_was_said() {
        let a = mark(
            "A",
            MarkShape::Rect {
                rect: NormRect {
                    x: 0.123_456_7,
                    y: 0.2,
                    w: 0.3,
                    h: 0.4,
                },
            },
        );
        let mut gone = mark(
            "B",
            MarkShape::Pin {
                at: StrokePoint {
                    x: 0.5,
                    y: 0.5,
                    t: None,
                },
            },
        );
        gone.deleted = true;
        let said = Comment {
            comment_id: CommentId::new(),
            text: "This button".into(),
            mark_ids: vec![a.mark_id, gone.mark_id],
            origin: CommentOrigin::Typed,
            link_origins: vec![LinkOrigin::Manual, LinkOrigin::Manual],
            moment: None,
        };
        let drawn = bundle(vec![a.clone(), gone.clone()], vec![said]);
        let view = for_agent(&drawn);
        assert_eq!(view["bundle_id"], json!(drawn.bundle_id));
        assert_eq!(view["revision"], 2);
        assert_eq!(view["marks"][0]["mark_id"], json!(a.mark_id));
        assert_eq!(view["marks"][0]["shape"]["rect"]["x"], 0.123);
        assert_eq!(view["marks"][1]["deleted"], true);
        assert_eq!(view["marks"][0].get("deleted"), None);
        assert_eq!(
            view["comments"][0],
            json!({"text": "This button", "marks": ["A", "B"]})
        );
        assert_eq!(view["sources"][0]["width"], 1080);
        let read = view.to_string();
        for noise in [
            "sha256",
            "artifact_id",
            "captured_at",
            "created_at",
            "color",
            "link_origins",
            "steps",
            "general_note",
        ] {
            assert!(!read.contains(noise), "{noise} in {read}");
        }
    }
}
