// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, api::*, error, internal, unsupported};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use protocol::{auth::*, time::Timestamp, *};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Device {
    pub record: DeviceRecord,
    pub key: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
struct Ticket {
    hash: [u8; 32],
    expires: Timestamp,
}
/// A device's pairing request waiting for the local owner. Kept only in
/// memory: a restart closes the window as well.
pub(crate) struct WaitingPairing {
    request: PairRequest,
    code: String,
    expires: Timestamp,
    answer: oneshot::Sender<Result<PairResponse, ProtocolError>>,
}
impl WaitingPairing {
    /// The phone stopped waiting, or the window closed, without an answer.
    fn lapsed(&self, now: Timestamp) -> bool {
        self.answer.is_closed() || self.expires <= now
    }
}
/// Longest chat name kept with a link request, in bytes.
const MAX_LINK_NAME: usize = 200;
/// Link codes one chat, and all chats together, may be given in a minute,
/// however many people ask. The owner reviews each code by hand, so more
/// than this isn't people linking; it also bounds how many codes wait at once.
const CHAT_LINKS_PER_MINUTE: usize = 3;
const LINKS_PER_MINUTE: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingLink {
    pub channel: ChannelKind,
    pub user: String,
    pub chat: String,
    pub name: String,
    pub expires: Timestamp,
}
impl PendingLink {
    /// When the code was handed out: it lasts `LINK_CODE_MINUTES` from then.
    fn asked_at(&self) -> Timestamp {
        Timestamp(
            self.expires
                .0
                .saturating_sub(LINK_CODE_MINUTES as i64 * 60_000),
        )
    }
}

/// What the owner decided about a link code, kept until the code would have
/// expired so the chat's bot can tell the person who asked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LinkOutcome {
    pub channel: ChannelKind,
    pub approved: bool,
    pub expires: Timestamp,
}

/// Where chats work, as the companion last reported it, and how to change it.
#[derive(Default)]
pub(crate) struct ChatProjectSlot {
    settings: Option<Arc<dyn ChatProjects>>,
    projects: HashMap<ChannelKind, BTreeMap<String, ProjectId>>,
}

pub(crate) fn random_secret() -> Result<String, ProtocolError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(crate) fn hash(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}
/// A request's device key, once its fields are fit to show the owner. The name
/// and platform are shown in a terminal too, where control characters could
/// rewrite or hide what is printed.
fn device_key(request: &PairRequest) -> Result<[u8; 32], ProtocolError> {
    if !ProtocolVersion::CURRENT.compatible_with(&request.protocol) {
        return Err(error(
            ErrorCode::IncompatibleVersion,
            "Unsupported protocol version",
        ));
    }
    if request.device_name.trim().is_empty()
        || request.device_name.len() > 200
        || request.device_platform.len() > 100
        || request.pairing_secret.len() > 100
        || [&request.device_name, &request.device_platform]
            .iter()
            .any(|text| text.chars().any(char::is_control))
    {
        return Err(error(ErrorCode::InvalidRequest, "Invalid pairing request"));
    }
    URL_SAFE_NO_PAD
        .decode(&request.device_public_key)
        .map_err(|_| error(ErrorCode::InvalidRequest, "Invalid public key"))?
        .try_into()
        .map_err(|_| {
            error(
                ErrorCode::InvalidRequest,
                "Public key must contain 32 bytes",
            )
        })
}
/// The open pairing window, if `secret` is its secret and it has not expired.
fn open_window(r: &store::Read<'_>, secret: &str, now: Timestamp) -> store::Result<Ticket> {
    let ticket: Ticket = r
        .get("meta", "pairing")?
        .ok_or_else(|| error(ErrorCode::Unauthenticated, "No pairing window is open"))?;
    let matches = bool::from(ticket.hash.ct_eq(&hash(secret)));
    if ticket.expires <= now {
        return Err(error(ErrorCode::Expired, "Pairing window expired").into());
    }
    if !matches {
        return Err(error(ErrorCode::Unauthenticated, "Invalid pairing secret").into());
    }
    Ok(ticket)
}
/// The paired device a request says it replaces, while that device still
/// has access. A request naming any other device replaces nothing.
fn replaced(r: &store::Read<'_>, request: &PairRequest) -> store::Result<Option<Device>> {
    let Some(id) = request.replaces else {
        return Ok(None);
    };
    Ok(r.get::<Device>("device", id)?
        .filter(|d| d.record.revoked_at.is_none()))
}
fn owner_answers(caller: &Caller) -> Result<(), ProtocolError> {
    if caller.principal != Principal::LocalOwner {
        return Err(error(
            ErrorCode::PermissionDenied,
            "Pairing must be answered by the local owner",
        ));
    }
    Ok(())
}

impl CompanionHub {
    pub(crate) async fn open_ticket(
        &self,
        caller: &Caller,
    ) -> Result<PairingTicket, ProtocolError> {
        if caller.principal != Principal::LocalOwner {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Pairing must be opened by the local owner",
            ));
        }
        let secret = random_secret()?;
        let expires_at = Timestamp(self.inner.clock.now().0.saturating_add(300_000));
        // A request made with the window this one replaces can no longer enroll.
        let mut waiting = self.waiting_pairing();
        if let Some(replaced) = waiting.take() {
            let _ = replaced.answer.send(Err(error(
                ErrorCode::Expired,
                "The computer replaced this pairing code with a new one",
            )));
        }
        self.write(|tx| {
            tx.put(
                "meta",
                "pairing",
                &Ticket {
                    hash: hash(&secret),
                    expires: expires_at,
                },
            )?;
            Ok(())
        })?;
        Ok(PairingTicket { secret, expires_at })
    }
    /// Holds a request that carries the open window's secret until the local
    /// owner answers it. A second request is refused while one waits, so the
    /// owner compares one code at a time.
    pub(crate) fn hold_pairing(
        &self,
        request: PairRequest,
        code: String,
    ) -> Result<PairingClaim, ProtocolError> {
        let key = device_key(&request)?;
        let now = self.inner.clock.now();
        let mut waiting = self.waiting_pairing();
        let expires = self.read(|r| {
            let ticket = open_window(r, &request.pairing_secret, now)?;
            if r.list::<Device>("device")?.iter().any(|d| d.key == key) {
                return Err(error(ErrorCode::Conflict, "This key was already enrolled").into());
            }
            Ok(ticket.expires)
        })?;
        if waiting.as_ref().is_some_and(|w| !w.lapsed(now)) {
            return Err(error(
                ErrorCode::Conflict,
                "Another device is already waiting for approval on this computer",
            ));
        }
        let (answer, receiver) = oneshot::channel();
        *waiting = Some(WaitingPairing {
            request,
            code,
            expires,
            answer,
        });
        Ok(PairingClaim {
            expires_at: expires,
            answer: receiver,
        })
    }
    pub(crate) fn pairing_prompt(
        &self,
        caller: &Caller,
    ) -> Result<Option<PairingPrompt>, ProtocolError> {
        owner_answers(caller)?;
        let now = self.inner.clock.now();
        let mut waiting = self.waiting_pairing();
        if waiting.as_ref().is_some_and(|w| w.lapsed(now)) {
            *waiting = None;
        }
        let Some(w) = waiting.as_ref() else {
            return Ok(None);
        };
        Ok(Some(PairingPrompt {
            device_name: w.request.device_name.clone(),
            device_platform: w.request.device_platform.clone(),
            code: w.code.clone(),
            shows_code: w.request.shows_code,
            expires_at: w.expires,
            replaces: self.read(|r| replaced(r, &w.request))?.map(|d| d.record),
        }))
    }
    /// Confirming enrolls the waiting device, in place of the device it
    /// replaces if any; that one's id is returned beside the new record.
    /// Refusing closes the window, so a link someone else saw cannot be tried
    /// again. A device whose app shows no code is never enrolled.
    pub(crate) fn answer_pairing_request(
        &self,
        caller: &Caller,
        code: &str,
        confirm: bool,
    ) -> Result<Option<(DeviceRecord, Option<DeviceId>)>, ProtocolError> {
        owner_answers(caller)?;
        let now = self.inner.clock.now();
        let code: String = code.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
        let waiting = {
            let mut waiting = self.waiting_pairing();
            if waiting.as_ref().is_some_and(|w| w.lapsed(now)) {
                *waiting = None;
            }
            waiting.take_if(|w| w.code == code)
        }
        .ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                "No device is waiting for approval with that code",
            )
        })?;
        if !waiting.request.shows_code {
            // With no code on the device, the owner could compare only its
            // name, which anyone who saw the link can copy. The window stays
            // open, so the same device can try again once its app is updated.
            let _ = waiting.answer.send(Err(error(
                ErrorCode::IncompatibleVersion,
                "This app is too old to pair safely. Update Øphio and try again.",
            )));
            return if confirm {
                Err(error(
                    ErrorCode::IncompatibleVersion,
                    "This device's app is too old to pair safely. Update Øphio on the device and try again.",
                ))
            } else {
                Ok(None)
            };
        }
        if !confirm {
            self.write(|tx| tx.delete("meta", "pairing"))?;
            let _ = waiting.answer.send(Err(error(
                ErrorCode::PermissionDenied,
                "The computer's owner refused this device",
            )));
            return Ok(None);
        }
        let enrolled = self.enroll(waiting.request, now);
        let _ = waiting
            .answer
            .send(enrolled.clone().map(|(response, _, _)| response));
        enrolled.map(|(_, record, replaced)| Some((record, replaced)))
    }
    fn enroll(
        &self,
        request: PairRequest,
        now: Timestamp,
    ) -> Result<(PairResponse, DeviceRecord, Option<DeviceId>), ProtocolError> {
        let key = device_key(&request)?;
        self.write(|tx| {
            open_window(&tx.read(), &request.pairing_secret, now)?;
            let devices = tx.read().list::<Device>("device")?;
            if devices.iter().any(|d| d.key == key) {
                return Err(error(ErrorCode::Conflict, "This key was already enrolled").into());
            }
            // The owner confirmed a prompt naming the device this one
            // replaces: it passes on what that device may do, and that
            // device loses access in the same step.
            let replaced = replaced(&tx.read(), &request)?;
            if let Some(mut old) = replaced.clone() {
                old.record.revoked_at = Some(now);
                tx.put("device", old.record.device_id, &old)?;
            }
            // With no device left that can manage it, the one pairing now does.
            let grants = if let Some(old) = &replaced {
                old.record.grants.clone()
            } else if !devices
                .iter()
                .any(|d| d.record.revoked_at.is_none() && d.record.grants.admin)
            {
                domain::grants::manager_defaults()
            } else {
                domain::grants::device_defaults()
            };
            let device_id = DeviceId::new();
            let record = DeviceRecord {
                device_id,
                name: request.device_name,
                platform: request.device_platform,
                paired_at: now,
                last_seen_at: None,
                revoked_at: None,
                grants: grants.clone(),
                is_current: false,
            };
            tx.delete("meta", "pairing")?;
            tx.put(
                "device",
                device_id,
                &Device {
                    record: record.clone(),
                    key,
                },
            )?;
            emit_devices(tx)?;
            Ok((
                PairResponse {
                    device_id,
                    host_id: self.inner.host_id,
                    host_name: self.inner.name.clone(),
                    grants,
                },
                record,
                replaced.map(|d| d.record.device_id),
            ))
        })
    }
    fn waiting_pairing(&self) -> std::sync::MutexGuard<'_, Option<WaitingPairing>> {
        self.inner
            .pairing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn credential(&self, id: DeviceId) -> Option<DeviceCredential> {
        self.read(|r| {
            Ok(r.get::<Device>("device", id)?.map(|d| DeviceCredential {
                device_id: id,
                public_key: d.key,
                grants: d.record.grants,
                revoked: d.record.revoked_at.is_some(),
            }))
        })
        .ok()
        .flatten()
    }
    pub(crate) fn seen(&self, id: DeviceId) {
        let now = self.inner.clock.now();
        let _ = self.write(|tx| {
            if let Some(mut d) = tx.read().get::<Device>("device", id)?
                && d.record.revoked_at.is_none()
            {
                let due = d
                    .record
                    .last_seen_at
                    .is_none_or(|last| now.0.saturating_sub(last.0) >= 60_000);
                // The first authentication after another pairing must clear its
                // observation-gap hint even within the normal heartbeat throttle.
                let after_pairing = !due
                    && tx.read().list::<Device>("device")?.iter().any(|other| {
                        d.record
                            .last_seen_at
                            .is_some_and(|last| other.record.paired_at > last)
                    });
                if due || after_pairing {
                    d.record.last_seen_at = Some(now);
                    tx.put("device", id, &d)?;
                    emit_devices(tx)?;
                }
            }
            Ok(())
        });
    }
    pub(crate) fn find_channel(
        &self,
        kind: ChannelKind,
        user: &str,
        chat: &str,
    ) -> Option<ChannelIdentity> {
        self.read(|r| {
            Ok(r.list::<ChannelIdentity>("channel")?.into_iter().find(|c| {
                c.channel == kind
                    && c.external_user == user
                    && c.external_chat == chat
                    && c.revoked_at.is_none()
            }))
        })
        .ok()
        .flatten()
    }
    pub(crate) fn link_request(
        &self,
        mut request: ChannelLinkRequest,
    ) -> Result<String, ProtocolError> {
        if request.external_user.is_empty()
            || request.external_chat.is_empty()
            || request.external_user.len() > 200
            || request.external_chat.len() > 200
        {
            return Err(error(ErrorCode::InvalidRequest, "Invalid channel identity"));
        }
        // A long chat name is shortened; it is never a reason to refuse a link.
        let name = &mut request.display_name;
        if name.len() > MAX_LINK_NAME {
            name.truncate(name.floor_char_boundary(MAX_LINK_NAME - '…'.len_utf8()));
            name.push('…');
        }
        let rate_key = format!("{:?}:{}", request.channel, request.external_user);
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(internal)?;
        let alphabet = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
        let code: String = random
            .iter()
            .map(|v| alphabet[usize::from(*v) % alphabet.len()] as char)
            .collect();
        let now = self.inner.clock.now();
        let limited = || {
            error(
                ErrorCode::LimitExceeded,
                "Wait before requesting another link code",
            )
        };
        self.write(|tx| {
            let recent = |at: Timestamp| now.0.saturating_sub(at.0) < 60_000;
            if tx
                .read()
                .get::<Timestamp>("link_rate", &rate_key)?
                .is_some_and(recent)
            {
                return Err(limited().into());
            }
            prune_links(tx, now)?;
            let asked: Vec<_> = tx
                .read()
                .list::<PendingLink>("link")?
                .into_iter()
                .filter(|link| recent(link.asked_at()))
                .collect();
            let from_chat = asked
                .iter()
                .filter(|link| {
                    link.channel == request.channel && link.chat == request.external_chat
                })
                .count();
            if from_chat >= CHAT_LINKS_PER_MINUTE || asked.len() >= LINKS_PER_MINUTE {
                return Err(limited().into());
            }
            // A code whose outcome is still remembered is not reissued, so the
            // outcome is never reported to the wrong chat.
            if tx.read().get::<PendingLink>("link", &code)?.is_some()
                || tx
                    .read()
                    .get::<LinkOutcome>("link_outcome", &code)?
                    .is_some()
            {
                return Err(error(
                    ErrorCode::TemporarilyUnavailable,
                    "Try requesting a link code again",
                )
                .into());
            }
            // Only the last minute's requests limit anyone, so older ones go.
            for (key, at) in tx.read().entries::<Timestamp>("link_rate")? {
                if !recent(at) {
                    tx.delete("link_rate", &key)?;
                }
            }
            tx.put("link_rate", rate_key, &now)?;
            tx.put(
                "link",
                &code,
                &PendingLink {
                    channel: request.channel,
                    user: request.external_user,
                    chat: request.external_chat,
                    name: request.display_name,
                    expires: Timestamp(now.0.saturating_add(LINK_CODE_MINUTES as i64 * 60_000)),
                },
            )?;
            Ok(code)
        })
    }
    /// Clears expired codes before `code` is approved. An expired code is
    /// reported as expired this once and is unknown afterwards.
    pub(crate) fn check_link_code(&self, code: &str) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        if self
            .write(|tx| prune_links(tx, now))?
            .iter()
            .any(|c| c == code)
        {
            return Err(error(ErrorCode::Expired, "Channel link expired"));
        }
        Ok(())
    }
    /// Who asked for `code` and from which chat, for the owner to check
    /// before approving or refusing it.
    pub(crate) fn pending_link(&self, code: &str) -> Result<PendingChannelLink, ProtocolError> {
        let now = self.inner.clock.now();
        let link = self.read(|r| r.require::<PendingLink>("link", code))?;
        if link.expires <= now {
            return Err(error(ErrorCode::Expired, "Channel link expired"));
        }
        let project_id = self
            .chat_projects()
            .projects
            .get(&link.channel)
            .and_then(|p| p.get(&link.chat))
            .copied();
        Ok(PendingChannelLink {
            channel: link.channel,
            external_user: link.user,
            external_chat: link.chat,
            display_name: link.name,
            project_id,
            expires_at: link.expires,
        })
    }
    pub(crate) fn link_outcome(
        &self,
        channel: ChannelKind,
        code: &str,
    ) -> Option<ChannelLinkOutcome> {
        let now = self.inner.clock.now();
        self.read(|r| {
            if let Some(link) = r.get::<PendingLink>("link", code)? {
                return Ok((link.channel == channel && link.expires > now)
                    .then_some(ChannelLinkOutcome::Waiting));
            }
            Ok(r.get::<LinkOutcome>("link_outcome", code)?
                .filter(|o| o.channel == channel && o.expires > now)
                .map(|o| {
                    if o.approved {
                        ChannelLinkOutcome::Approved
                    } else {
                        ChannelLinkOutcome::Refused
                    }
                }))
        })
        .ok()
        .flatten()
    }
    /// Lets the app choose the project a linked chat works in.
    pub fn set_chat_project_settings(&self, settings: Arc<dyn ChatProjects>) {
        self.chat_projects().settings = Some(settings);
    }
    /// The companion reports where each of a platform's chats works whenever it
    /// applies its chat settings, including changes made with the CLI.
    pub fn set_chat_projects(
        &self,
        channel: ChannelKind,
        projects: BTreeMap<String, ProjectId>,
    ) -> Result<(), ProtocolError> {
        {
            let mut slot = self.chat_projects();
            let current = slot.projects.get(&channel);
            if current.map_or(projects.is_empty(), |c| *c == projects) {
                return Ok(());
            }
            slot.projects.insert(channel, projects);
        }
        self.write(|tx| self.emit_identities(tx))
    }
    fn chat_projects(&self) -> std::sync::MutexGuard<'_, ChatProjectSlot> {
        self.inner
            .chat_projects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn chat_project_settings(&self) -> Result<Arc<dyn ChatProjects>, ProtocolError> {
        self.chat_projects()
            .settings
            .clone()
            .ok_or_else(|| unsupported("Chat project settings"))
    }
    /// Linked chats as the app lists them, each with the project its chat works in.
    pub(crate) fn channel_identities(
        &self,
        r: &store::Read<'_>,
    ) -> store::Result<Vec<ChannelIdentity>> {
        let slot = self.chat_projects();
        Ok(r.list::<ChannelIdentity>("channel")?
            .into_iter()
            .map(|mut c| {
                c.project_id = slot
                    .projects
                    .get(&c.channel)
                    .and_then(|p| p.get(&c.external_chat))
                    .copied();
                c
            })
            .collect())
    }
    pub(crate) fn emit_identities(&self, tx: &mut store::Write<'_>) -> store::Result<()> {
        let identities = self.channel_identities(&tx.read())?;
        tx.emit(Event::ChannelIdentitiesChanged { identities })?;
        Ok(())
    }
}
/// Deletes pending links that can no longer be approved and returns their
/// codes, with the outcomes of codes that would have expired by now.
fn prune_links(tx: &store::Write<'_>, now: Timestamp) -> store::Result<Vec<String>> {
    let mut pruned = Vec::new();
    for (code, link) in tx.read().entries::<PendingLink>("link")? {
        if link.expires <= now {
            tx.delete("link", &code)?;
            pruned.push(code);
        }
    }
    for (code, outcome) in tx.read().entries::<LinkOutcome>("link_outcome")? {
        if outcome.expires <= now {
            tx.delete("link_outcome", &code)?;
        }
    }
    Ok(pruned)
}
pub(crate) fn emit_devices(tx: &mut store::Write<'_>) -> store::Result<()> {
    let devices = tx
        .read()
        .list::<Device>("device")?
        .into_iter()
        .map(|d| d.record)
        .collect();
    tx.emit(Event::DevicesChanged { devices })?;
    Ok(())
}
