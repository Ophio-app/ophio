// SPDX-License-Identifier: Apache-2.0
//! AT-SPI metadata for the active application window. Text/value interfaces are never used.
use async_trait::async_trait;
use protocol::voice::{VoiceBounds, VoiceElement};
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::Arc,
};
use voice_control::{Accessibility, Error, Snapshot};
use zbus::{Connection, Proxy, zvariant::OwnedObjectPath};

pub const REMEDY: &str = "Enable accessibility on the computer (GNOME or KDE Accessibility settings; on other Linux desktops start at-spi-bus-launcher)";
// Accessibility is running, but there is nothing to read: the screen is
// locked or empty, or the app in front doesn't take part.
pub const NO_FOCUS: &str = "No app on the computer that exposes its controls has focus. Bring one to the front; many terminals don't expose theirs";
pub const NO_CONTROLS: &str = "The focused app on the computer exposes no controls";
const SLOW: &str = "Accessibility on the computer didn't answer in time";
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Object {
    pub bus: String,
    pub path: OwnedObjectPath,
}
#[derive(Debug, Clone)]
pub struct Metadata {
    pub role: String,
    pub name: String,
    pub state: Vec<u32>,
    pub bounds: VoiceBounds,
    pub actions: Vec<String>,
}
pub fn state_has(words: &[u32], bit: usize) -> bool {
    words
        .get(bit / 32)
        .is_some_and(|n| n & (1 << (bit % 32)) != 0)
}
#[async_trait]
pub trait Bus: Send + Sync + std::fmt::Debug {
    async fn children(&self, object: &Object) -> Result<Vec<Object>, Error>;
    async fn metadata(&self, object: &Object) -> Result<Metadata, Error>;
    async fn activate(&self, object: &Object, focus: bool) -> Result<bool, Error>;
}
#[derive(Debug)]
pub struct AtSpi {
    bus: Arc<dyn Bus>,
    nodes: tokio::sync::Mutex<HashMap<String, Object>>,
}
impl AtSpi {
    pub fn with_bus(bus: Arc<dyn Bus>) -> Self {
        Self {
            bus,
            nodes: Default::default(),
        }
    }
    pub async fn connect() -> Result<Self, Error> {
        let session = Connection::session().await.map_err(failure)?;
        let proxy = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
            .await
            .map_err(failure)?;
        let address: String = proxy.call("GetAddress", &()).await.map_err(failure)?;
        let connection = zbus::connection::Builder::address(address.as_str())
            .map_err(failure)?
            .build()
            .await
            .map_err(failure)?;
        Ok(Self::with_bus(Arc::new(NativeBus(connection))))
    }
}
fn failure(_: impl std::fmt::Display) -> Error {
    Error::Unavailable(REMEDY.into())
}
#[derive(Debug)]
struct NativeBus(Connection);
impl NativeBus {
    async fn proxy<'a>(
        &'a self,
        o: &'a Object,
        interface: &'static str,
    ) -> Result<Proxy<'a>, Error> {
        Proxy::new(&self.0, o.bus.as_str(), o.path.as_str(), interface)
            .await
            .map_err(failure)
    }
}
#[async_trait]
impl Bus for NativeBus {
    async fn children(&self, o: &Object) -> Result<Vec<Object>, Error> {
        let rows: Vec<(String, OwnedObjectPath)> = self
            .proxy(o, "org.a11y.atspi.Accessible")
            .await?
            .call("GetChildren", &())
            .await
            .map_err(failure)?;
        Ok(rows
            .into_iter()
            .take(1000)
            .map(|(bus, path)| Object { bus, path })
            .collect())
    }
    async fn metadata(&self, o: &Object) -> Result<Metadata, Error> {
        let p = self.proxy(o, "org.a11y.atspi.Accessible").await?;
        let role: String = p.call("GetRoleName", &()).await.map_err(failure)?;
        let name: String = p.get_property("Name").await.map_err(failure)?;
        let state: Vec<u32> = p.call("GetState", &()).await.map_err(failure)?;
        let bounds: (i32, i32, i32, i32) = self
            .proxy(o, "org.a11y.atspi.Component")
            .await?
            .call("GetExtents", &(0u32,))
            .await
            .unwrap_or((0, 0, 0, 0));
        let actions: Vec<(String, String, String)> = self
            .proxy(o, "org.a11y.atspi.Action")
            .await?
            .call("GetActions", &())
            .await
            .unwrap_or_default();
        Ok(Metadata {
            role,
            name,
            state,
            bounds: VoiceBounds {
                x: bounds.0.into(),
                y: bounds.1.into(),
                width: bounds.2.into(),
                height: bounds.3.into(),
            },
            actions: actions.into_iter().map(|(name, _, _)| name).collect(),
        })
    }
    async fn activate(&self, o: &Object, focus: bool) -> Result<bool, Error> {
        if focus {
            return self
                .proxy(o, "org.a11y.atspi.Component")
                .await?
                .call("GrabFocus", &())
                .await
                .map_err(failure);
        }
        let m = self.metadata(o).await?;
        if let Some(index) = m
            .actions
            .iter()
            .position(|a| matches!(a.as_str(), "click" | "activate" | "press"))
        {
            self.proxy(o, "org.a11y.atspi.Action")
                .await?
                .call("DoAction", &(index as i32,))
                .await
                .map_err(|_| Error::OutcomeUnknown)
        } else {
            Ok(false)
        }
    }
}
fn element(o: &Object, m: Metadata) -> VoiceElement {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    o.hash(&mut h);
    let sensitive = m.role.contains("password")
        || m.name.to_lowercase().split_whitespace().any(|w| {
            matches!(
                w,
                "password"
                    | "secret"
                    | "token"
                    | "card"
                    | "cvv"
                    | "passcode"
                    | "key"
                    | "credential"
                    | "credentials"
                    | "authorization"
                    | "ssn"
                    | "cvc"
                    | "otp"
                    | "seed"
                    | "recovery"
            )
        });
    VoiceElement {
        id: format!("a{:x}", h.finish()),
        role: m.role,
        name: m.name,
        bounds: m.bounds,
        visible: state_has(&m.state, 25) && state_has(&m.state, 30),
        enabled: state_has(&m.state, 7),
        focused: state_has(&m.state, 12),
        sensitive,
        destructive: false,
    }
}
#[async_trait]
impl Accessibility for AtSpi {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let root = Object {
                bus: "org.a11y.atspi.Registry".into(),
                path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root")
                    .map_err(failure)?,
            };
            let mut focused = None;
            for app in self.bus.children(&root).await?.into_iter().take(100) {
                for window in self
                    .bus
                    .children(&app)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .take(100)
                {
                    if let Ok(m) = self.bus.metadata(&window).await
                        && (state_has(&m.state, 1) || state_has(&m.state, 12))
                    {
                        focused = Some(window);
                        break;
                    }
                }
                if focused.is_some() {
                    break;
                }
            }
            let root = focused.ok_or_else(|| Error::Unavailable(NO_FOCUS.into()))?;
            let context = format!("{}:{}", root.bus, root.path);
            let mut queue = VecDeque::from([root]);
            let mut elements = Vec::new();
            let mut nodes = HashMap::new();
            let mut visited = std::collections::HashSet::new();
            while let Some(o) = queue.pop_front() {
                if !visited.insert(o.clone()) {
                    continue;
                }
                if visited.len() > 1000 {
                    break;
                }
                if let Ok(m) = self.bus.metadata(&o).await {
                    if !m.actions.is_empty() || state_has(&m.state, 11) || state_has(&m.state, 12) {
                        let e = element(&o, m);
                        nodes.insert(e.id.clone(), o.clone());
                        elements.push(e);
                    }
                    queue.extend(self.bus.children(&o).await.unwrap_or_default());
                }
            }
            *self.nodes.lock().await = nodes;
            let snapshot = Snapshot::new(elements, context);
            if snapshot.elements.is_empty() {
                return Err(Error::Unavailable(NO_CONTROLS.into()));
            }
            Ok(snapshot)
        })
        .await
        .map_err(|_| Error::Unavailable(SLOW.into()))?
    }
    async fn activate(&self, element: &VoiceElement, focus: bool) -> Result<bool, Error> {
        let fresh = self.snapshot().await?;
        if !fresh.elements.contains(element) {
            return Err(Error::StaleTarget);
        }
        let object = self
            .nodes
            .lock()
            .await
            .get(&element.id)
            .cloned()
            .ok_or(Error::StaleTarget)?;
        self.bus.activate(&object, focus).await
    }
}
pub(crate) fn map_point(
    x: f64,
    y: f64,
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
    current: bool,
) -> Result<protocol::control::TargetPoint, ports::PlatformError> {
    let point = protocol::control::TargetPoint {
        x: (x - origin_x) / width,
        y: (y - origin_y) / height,
    };
    if !current || !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
        return Err(ports::PlatformError::GeometryChanged);
    }
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_state_words_and_password_metadata() {
        let words = vec![(1 << 7) | (1 << 11) | (1 << 12) | (1 << 25) | (1 << 30), 0];
        let o = Object {
            bus: ":1.42".into(),
            path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/7").unwrap(),
        };
        let e = element(
            &o,
            Metadata {
                role: "password text".into(),
                name: "Sign in".into(),
                state: words,
                bounds: VoiceBounds {
                    x: 10.,
                    y: 20.,
                    width: 100.,
                    height: 30.,
                },
                actions: vec![],
            },
        );
        assert!(e.sensitive && e.visible && e.enabled && e.focused);
        assert!(!state_has(&[], 30));
    }
    #[test]
    fn accessibility_point_respects_negative_origin_scale_and_revision() {
        let p = map_point(-50., 25., -100., 0., 100., 50., true).unwrap();
        assert_eq!(p, protocol::control::TargetPoint { x: 0.5, y: 0.5 });
        assert!(map_point(100., 0., -100., 0., 100., 50., true).is_err());
        assert!(map_point(-50., 25., -100., 0., 100., 50., false).is_err());
    }
    #[derive(Debug, Default)]
    struct RecordedBus {
        changed: std::sync::atomic::AtomicBool,
        actions: std::sync::atomic::AtomicUsize,
    }
    fn object(path: &str) -> Object {
        Object {
            bus: ":1.42".into(),
            path: OwnedObjectPath::try_from(path).unwrap(),
        }
    }
    #[async_trait]
    impl Bus for RecordedBus {
        async fn children(&self, o: &Object) -> Result<Vec<Object>, Error> {
            Ok(match o.path.as_str() {
                "/org/a11y/atspi/accessible/root" => vec![object("/app")],
                "/app" => vec![object("/window")],
                "/window" => vec![object("/button"), object("/password")],
                _ => vec![],
            })
        }
        async fn metadata(&self, o: &Object) -> Result<Metadata, Error> {
            let window = o.path.as_str() == "/window";
            let password = o.path.as_str() == "/password";
            Ok(Metadata {
                role: if window {
                    "frame"
                } else if password {
                    "password text"
                } else {
                    "push button"
                }
                .into(),
                name: if self.changed.load(std::sync::atomic::Ordering::SeqCst) {
                    "Changed"
                } else {
                    "Fixture"
                }
                .into(),
                state: vec![
                    (1 << 7) | (1 << 25) | (1 << 30) | if window { 1 << 1 } else { 1 << 11 },
                ],
                bounds: VoiceBounds {
                    x: 10.,
                    y: 20.,
                    width: 40.,
                    height: 30.,
                },
                actions: if window { vec![] } else { vec!["click".into()] },
            })
        }
        async fn activate(&self, _: &Object, _: bool) -> Result<bool, Error> {
            self.actions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(true)
        }
    }
    #[tokio::test]
    async fn fake_bus_focus_snapshot_actions_and_stale_refusal() {
        let bus = Arc::new(RecordedBus::default());
        let adapter = AtSpi::with_bus(bus.clone());
        let s = adapter.snapshot().await.unwrap();
        assert_eq!(s.elements.len(), 2);
        assert_eq!(s.elements.iter().filter(|e| e.sensitive).count(), 1);
        assert!(adapter.activate(&s.elements[0], false).await.unwrap());
        bus.changed.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            adapter.activate(&s.elements[0], false).await,
            Err(Error::StaleTarget)
        ));
        assert_eq!(bus.actions.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    /// One app with one window; `active` says whether it has focus and
    /// `controls` whether it has a button.
    #[derive(Debug)]
    struct QuietBus {
        active: bool,
        controls: bool,
    }
    #[async_trait]
    impl Bus for QuietBus {
        async fn children(&self, o: &Object) -> Result<Vec<Object>, Error> {
            Ok(match o.path.as_str() {
                "/org/a11y/atspi/accessible/root" => vec![object("/app")],
                "/app" => vec![object("/window")],
                "/window" if self.controls => vec![object("/button")],
                _ => vec![],
            })
        }
        async fn metadata(&self, o: &Object) -> Result<Metadata, Error> {
            let window = o.path.as_str() == "/window";
            Ok(Metadata {
                role: if window { "frame" } else { "push button" }.into(),
                name: "Fixture".into(),
                state: vec![if window && self.active { 1 << 1 } else { 0 }],
                bounds: VoiceBounds {
                    x: 0.,
                    y: 0.,
                    width: 10.,
                    height: 10.,
                },
                actions: if window { vec![] } else { vec!["click".into()] },
            })
        }
        async fn activate(&self, _: &Object, _: bool) -> Result<bool, Error> {
            Ok(false)
        }
    }
    #[tokio::test]
    async fn running_accessibility_with_nothing_to_read_says_so_instead_of_asking_to_enable_it() {
        let reason = |e| match e {
            Err(Error::Unavailable(r)) => r,
            other => panic!("{other:?}"),
        };
        let nothing_focused = AtSpi::with_bus(Arc::new(QuietBus {
            active: false,
            controls: true,
        }));
        assert_eq!(reason(nothing_focused.snapshot().await), NO_FOCUS);
        let no_controls = AtSpi::with_bus(Arc::new(QuietBus {
            active: true,
            controls: false,
        }));
        assert_eq!(reason(no_controls.snapshot().await), NO_CONTROLS);
    }
}
