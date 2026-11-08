// SPDX-License-Identifier: Apache-2.0
//! Which alerts reach the person's phones (see [`protocol::notifications`]).
use crate::{CompanionHub, error};
use protocol::{
    notifications::{
        AlertTopic, AlertType, MAX_PROJECT_NOTIFICATIONS, NotificationSettings,
        ProjectNotifications, Tier, TypeTier,
    },
    *,
};

impl CompanionHub {
    pub(crate) fn notification_settings(
        r: &store::Read<'_>,
    ) -> store::Result<NotificationSettings> {
        Ok(r.get("meta", "notification_settings")?.unwrap_or_default())
    }
    pub(crate) fn notification_settings_view(&self) -> Result<NotificationSettings, ProtocolError> {
        self.read(Self::notification_settings)
    }
    pub(crate) fn set_notification_settings(
        &self,
        tx: &mut store::Write<'_>,
        off: &[AlertTopic],
        bundle: bool,
        projects: &[ProjectNotifications],
        types: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let old = Self::notification_settings(&tx.read())?;
        domain::revision(revision, old.revision)?;
        // An older phone's save says nothing of the choices by type: each
        // project keeps its own.
        let mut projects = projects.to_vec();
        if !types {
            for kept in old.projects.iter().filter(|p| !p.tiers.is_empty()) {
                match projects
                    .iter_mut()
                    .find(|p| p.project_id == kept.project_id)
                {
                    Some(p) => p.tiers.clone_from(&kept.tiers),
                    None => projects.push(ProjectNotifications {
                        tiers: kept.tiers.clone(),
                        ..ProjectNotifications::new(kept.project_id)
                    }),
                }
            }
        }
        let too_many = || {
            error(
                ErrorCode::LimitExceeded,
                format!("At most {MAX_PROJECT_NOTIFICATIONS} projects can have their own alerts"),
            )
        };
        let mut settings = NotificationSettings {
            off: off.to_vec(),
            bundle,
            projects,
            // Not the phone's to set: brought over, and kept.
            tiers: old.tiers.clone(),
            revision: old.revision + 1,
        }
        .tidy()
        .ok_or_else(too_many)?;
        // An older phone's screen still saves topics turned off; they take
        // effect as the choices by type that replaced them.
        bring_over(&mut settings);
        let settings = settings.tidy().ok_or_else(too_many)?;
        tx.put("meta", "notification_settings", &settings)?;
        tx.emit(Event::NotificationSettingsChanged {})?;
        Ok(())
    }
}

/// Brings the topic switches saved before notification types existed over
/// as the choices by type that replaced them, once, as Øphio starts. A
/// computer with none saved changes nothing; one brought over already has
/// none left, so this never runs twice for the same choices.
pub(crate) fn bring_over_saved(tx: &mut store::Write<'_>) -> store::Result<()> {
    let mut settings = CompanionHub::notification_settings(&tx.read())?;
    if !bring_over(&mut settings) {
        return Ok(());
    }
    // Saved settings never list more projects than fit, and only as many
    // are added; should they somehow, the old switches stay as they are.
    let Some(mut settings) = settings.tidy() else {
        tracing::warn!("notification settings list too many projects to bring over");
        return Ok(());
    };
    settings.revision += 1;
    tx.put("meta", "notification_settings", &settings)?;
    tx.emit(Event::NotificationSettingsChanged {})?;
    Ok(())
}

/// The notification types an old topic switch covered. The watchdog's
/// "stopped for showing no sign of progress" was under slow work, but it is
/// a failure now, and turning slow steps off never meant hearing of no
/// failure at all, so slow work covers only slow steps.
fn types_of(topic: AlertTopic) -> &'static [AlertType] {
    use AlertType::*;
    match topic {
        AlertTopic::Question => &[Question],
        AlertTopic::Approval => &[Approval],
        AlertTopic::NeedsYou => &[Decision, GoAhead, SignIn],
        AlertTopic::Finished => &[Finished, Reply, Batch],
        AlertTopic::Failed => &[Failed],
        AlertTopic::PrReady => &[PrReady],
        AlertTopic::PrNeedsYou => &[PrNeedsYou],
        AlertTopic::Merged => &[Merged],
        AlertTopic::Issues => &[Issues, IssueCall],
        AlertTopic::SlowWork => &[Slow],
        AlertTopic::Reports => &[Reports, ReportQuestion],
    }
}

/// Turns the old topic switches in `settings` into choices by type, and
/// clears them; says whether there were any.
///
/// A topic a project turned off makes that project `Off` for each type the
/// topic covered. A topic off for every project makes this computer `Off`
/// for those types in every project ([`NotificationSettings::tiers`]), so
/// a project made later follows it too; a project that had turned the
/// topic back on keeps the type's recommended choice as its own. Nothing
/// louder is ever written: a type a project chose for itself already is
/// left as it is, and so is one that is `Off` by default anyway.
///
/// What is left out has nowhere to go: a topic that was off for every
/// project can't reach the alerts that belong to none (an agent signed out,
/// problem reports), which each phone now chooses for itself.
///
/// Afterwards every phone is told the same: an older phone, which can't
/// choose by type, is told an alert is quiet by these choices (see
/// [`crate::alerts`]) rather than by the old switches. Its own settings
/// screen still lists those switches, and shows them all on again, as the
/// computer keeps none; turning one off there brings it over in the same
/// way when the phone saves.
pub(crate) fn bring_over(settings: &mut NotificationSettings) -> bool {
    let had = !settings.off.is_empty()
        || (settings.projects.iter()).any(|p| !p.on.is_empty() || !p.off.is_empty());
    if !had {
        return false;
    }
    let everywhere = std::mem::take(&mut settings.off);
    let stricter = |t: &AlertType| t.default_tier() > Tier::Off;
    for topic in &everywhere {
        for alert_type in types_of(*topic).iter().filter(|t| stricter(t)) {
            if settings.computer_tier(*alert_type).is_none() {
                settings.tiers.push(TypeTier {
                    alert_type: *alert_type,
                    tier: Tier::Off,
                });
            }
        }
    }
    for p in &mut settings.projects {
        let on = std::mem::take(&mut p.on);
        let off = std::mem::take(&mut p.off);
        for topic in AlertTopic::ALL {
            // Off here; or back on here though off everywhere, which keeps
            // the recommended choice here.
            let tier = if off.contains(&topic) {
                Tier::Off
            } else if everywhere.contains(&topic) && on.contains(&topic) {
                Tier::Alert
            } else {
                continue;
            };
            for &alert_type in types_of(topic).iter().filter(|t| stricter(t)) {
                if p.tier(alert_type).is_none() {
                    p.tiers.push(TypeTier {
                        alert_type,
                        tier: tier.min(alert_type.default_tier()),
                    });
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(alert_type: AlertType, tier: Tier) -> TypeTier {
        TypeTier { alert_type, tier }
    }

    /// Off for every project but one, off in a project, and on in a
    /// project: the computer is Off for the types a topic off everywhere
    /// covered, new projects included; a project is Off for its own; one
    /// that turned a topic back on keeps the recommended choice; nothing is
    /// louder, and what a project chose itself stays.
    #[test]
    fn old_switches_come_over_as_stricter_choices_by_type_once() {
        let (a, b, c) = (ProjectId::new(), ProjectId::new(), ProjectId::new());
        let mut settings = NotificationSettings {
            off: vec![AlertTopic::Finished, AlertTopic::Question],
            bundle: true,
            projects: vec![
                ProjectNotifications {
                    // Back on here, and its own word on questions.
                    on: vec![AlertTopic::Finished],
                    off: vec![AlertTopic::PrReady, AlertTopic::Merged],
                    tiers: vec![tier(AlertType::Question, Tier::Alert)],
                    ..ProjectNotifications::new(a)
                },
                ProjectNotifications {
                    mute: true,
                    ..ProjectNotifications::new(b)
                },
            ],
            tiers: Vec::new(),
            revision: 4,
        };
        assert!(bring_over(&mut settings));
        let settings = settings.tidy().unwrap();
        assert!(settings.off.is_empty() && settings.bundle);
        assert_eq!(settings.revision, 4);
        // Finished tasks are Off by default, so only what the topics also
        // covered that alerts or shows by default is written.
        assert_eq!(
            settings.tiers,
            [
                tier(AlertType::Question, Tier::Off),
                tier(AlertType::Reply, Tier::Off),
                tier(AlertType::Batch, Tier::Off),
            ]
        );
        let a_choices = settings.project(a).unwrap().clone();
        assert!(a_choices.on.is_empty() && a_choices.off.is_empty());
        assert_eq!(
            a_choices.tiers,
            [
                tier(AlertType::Question, Tier::Alert),
                tier(AlertType::Reply, Tier::Silent),
                tier(AlertType::PrReady, Tier::Off),
                tier(AlertType::Batch, Tier::Silent),
            ]
        );
        assert!(settings.project(b).unwrap().mute);
        // A project with nothing of its own, or made later, follows the
        // computer.
        for project in [c, ProjectId::new()] {
            assert_eq!(
                settings.project_tier(project, AlertType::Question),
                Some(Tier::Off)
            );
            assert_eq!(settings.project_tier(project, AlertType::Approval), None);
        }
        assert_eq!(
            settings.project_tier(a, AlertType::Question),
            Some(Tier::Alert)
        );
        assert_eq!(
            settings.project_tier(a, AlertType::Reply),
            Some(Tier::Silent)
        );

        // Brought over already, nothing is left to bring.
        let mut again = settings.clone();
        assert!(!bring_over(&mut again));
        assert_eq!(again, settings);
    }

    #[test]
    fn switches_that_were_off_by_default_anyway_bring_nothing_over() {
        let a = ProjectId::new();
        let mut settings = NotificationSettings {
            off: vec![AlertTopic::Merged, AlertTopic::SlowWork],
            projects: vec![ProjectNotifications {
                on: vec![AlertTopic::Question],
                ..ProjectNotifications::new(a)
            }],
            ..NotificationSettings::default()
        };
        assert!(bring_over(&mut settings));
        let settings = settings.tidy().unwrap();
        assert_eq!(settings, NotificationSettings::default());
    }

    /// Every old topic covers at least one type, and every type an item of
    /// that topic is labelled with on its own is among them.
    #[test]
    fn every_topic_covers_the_types_its_items_get() {
        use protocol::model::{AttentionItem, AttentionKind};
        for topic in AlertTopic::ALL {
            assert!(!types_of(topic).is_empty(), "{topic:?}");
        }
        let item = |kind, topic| AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            kind,
            title: String::new(),
            body: None,
            task_id: Some(TaskId::new()),
            run_id: None,
            approval_id: None,
            created_at: protocol::time::Timestamp(0),
            resolved_at: None,
            priority: 1,
            topic,
            quiet: false,
            report_id: None,
            account_id: None,
            alert: None,
            alert_handled_at: None,
        };
        for kind in [
            AttentionKind::Question,
            AttentionKind::Approval,
            AttentionKind::WaitingForYou,
            AttentionKind::Conflict,
            AttentionKind::Completed,
            AttentionKind::Failure,
        ] {
            let item = item(kind, None);
            let topic = item.topic().unwrap();
            for conversation in [false, true] {
                let t = AlertType::of(&item, conversation).unwrap();
                assert!(types_of(topic).contains(&t), "{kind:?} {topic:?} {t:?}");
            }
        }
        // Items that name their topic, as Øphio raises them.
        for (kind, topic, t) in [
            (
                AttentionKind::Completed,
                AlertTopic::Merged,
                AlertType::Merged,
            ),
            (
                AttentionKind::Completed,
                AlertTopic::PrReady,
                AlertType::PrReady,
            ),
            (
                AttentionKind::Completed,
                AlertTopic::Issues,
                AlertType::Issues,
            ),
            (
                AttentionKind::WaitingForYou,
                AlertTopic::Issues,
                AlertType::IssueCall,
            ),
            (
                AttentionKind::WaitingForYou,
                AlertTopic::SlowWork,
                AlertType::Slow,
            ),
            (
                AttentionKind::WaitingForYou,
                AlertTopic::PrNeedsYou,
                AlertType::PrNeedsYou,
            ),
        ] {
            assert_eq!(AlertType::of(&item(kind, Some(topic)), false), Some(t));
            assert!(types_of(topic).contains(&t), "{topic:?}");
        }
    }
}
