// SPDX-License-Identifier: Apache-2.0
//! Which alerts reach the person's phones (see [`protocol::notifications`]).
use crate::{CompanionHub, error};
use protocol::{
    notifications::{
        AlertTopic, MAX_PROJECT_NOTIFICATIONS, NotificationSettings, ProjectNotifications,
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
        revision: Option<u64>,
    ) -> store::Result<()> {
        let old = Self::notification_settings(&tx.read())?;
        domain::revision(revision, old.revision)?;
        let settings = NotificationSettings {
            off: off.to_vec(),
            bundle,
            projects: projects.to_vec(),
            revision: old.revision + 1,
        }
        .tidy()
        .ok_or_else(|| {
            error(
                ErrorCode::LimitExceeded,
                format!("At most {MAX_PROJECT_NOTIFICATIONS} projects can have their own alerts"),
            )
        })?;
        tx.put("meta", "notification_settings", &settings)?;
        tx.emit(Event::NotificationSettingsChanged {})?;
        Ok(())
    }
}
