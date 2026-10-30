// SPDX-License-Identifier: Apache-2.0
//! Reports use separate summary and bundle documents so withdrawal drops content.
use crate::{Read, Result, Write};
use protocol::{ReportId, reports::*, time::Timestamp};
use serde::{Deserialize, Serialize};

pub const SUMMARY: &str = "report_summary";
pub const BUNDLE: &str = "report_bundle";
pub const QUEUE: &str = "report_queue";
pub const META: &str = "reports";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Delivery {
    Submit { report_id: ReportId },
    Answer(IntakeAnswer),
    Withdraw(WithdrawRequest),
}
impl Delivery {
    pub fn report_id(&self) -> ReportId {
        match self {
            Self::Submit { report_id } => *report_id,
            Self::Answer(a) => a.report_id,
            Self::Withdraw(w) => w.report_id,
        }
    }
    pub fn key(&self) -> String {
        match self {
            Self::Submit { report_id } => format!("{report_id}:submit"),
            Self::Answer(a) => format!("{}:answer:{}", a.report_id, a.question_id),
            Self::Withdraw(w) => format!("{}:withdraw", w.report_id),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDelivery {
    pub delivery: Delivery,
    pub attempts: u32,
    pub after: Timestamp,
}
impl crate::Store {
    /// Secure deletion clears the main database; truncating the WAL removes
    /// earlier committed copies of withdrawn bundles too.
    pub fn forget_report_bytes(&mut self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}
impl Read<'_> {
    pub fn report_settings(&self) -> Result<ReportSettings> {
        Ok(self.get(META, "settings")?.unwrap_or_default())
    }
    pub fn report_summaries(&self) -> Result<Vec<ReportSummary>> {
        let mut reports = self.list::<ReportSummary>(SUMMARY)?;
        reports.sort_by_key(|r| std::cmp::Reverse((r.created_at, r.report_id)));
        Ok(reports)
    }
    pub fn report_detail(&self, id: ReportId) -> Result<Option<ReportDetail>> {
        let Some(summary) = self.get(SUMMARY, id)? else {
            return Ok(None);
        };
        Ok(self
            .get(BUNDLE, id)?
            .map(|bundle| ReportDetail { summary, bundle }))
    }
}
impl Write<'_> {
    pub fn enqueue_report(&mut self, delivery: Delivery, now: Timestamp) -> Result<()> {
        self.put(
            QUEUE,
            delivery.key(),
            &ReportDelivery {
                delivery,
                attempts: 0,
                after: now,
            },
        )
    }
    pub fn keep_report(&mut self, bundle: &ReportBundle, now: Timestamp) -> Result<bool> {
        if self
            .read()
            .get::<ReportSummary>(SUMMARY, bundle.report_id)?
            .is_some()
        {
            return Ok(false);
        }
        let summary = ReportSummary {
            report_id: bundle.report_id,
            reference: report_reference(&bundle.report_id),
            created_at: bundle.created_at,
            origin: bundle.origin.clone(),
            words: bundle.words.as_deref().map(protocol::alerts::alert_text),
            signature: bundle.signature.clone(),
            history: vec![StatusChange {
                status: ReportProgress::Queued,
                at: now,
            }],
            questions: Vec::new(),
            checked_at: None,
        };
        self.put(SUMMARY, bundle.report_id, &summary)?;
        self.put(BUNDLE, bundle.report_id, bundle)?;
        self.enqueue_report(
            Delivery::Submit {
                report_id: bundle.report_id,
            },
            now,
        )?;
        Ok(true)
    }
    pub fn withdraw_report(&mut self, id: ReportId, now: Timestamp) -> Result<bool> {
        let mut summary: ReportSummary = self.read().require(SUMMARY, id)?;
        if summary.status() == ReportProgress::Withdrawn {
            return Ok(false);
        }
        self.delete(BUNDLE, id)?;
        for row in self.read().list::<ReportDelivery>(QUEUE)? {
            if row.delivery.report_id() == id {
                self.delete(QUEUE, row.delivery.key())?;
            }
        }
        summary.words = None;
        summary.questions.clear();
        summary.history.push(StatusChange {
            status: ReportProgress::Withdrawn,
            at: now,
        });
        self.put(SUMMARY, id, &summary)?;
        self.enqueue_report(Delivery::Withdraw(WithdrawRequest { report_id: id }), now)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_documents_survive_restart_and_withdrawal_keeps_only_a_summary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let id = ReportId::new();
        let bundle = ReportBundle {
            schema: REPORT_SCHEMA,
            report_id: id,
            created_at: Timestamp(1),
            origin: ReportOrigin::Settings,
            words: Some("Please help".into()),
            signature: ReportSignature {
                code: "none".into(),
                screen: "/settings".into(),
                build: "1".into(),
            },
            versions: ReportVersions::default(),
            context: ReportContext::default(),
            phone_trail: vec![],
            computer_trail: vec![],
            state: None,
            crashes: vec![],
            screenshot: None,
            extras: vec![],
            test: None,
        };
        let mut db = crate::Store::open(&path, crate::Retention::default()).unwrap();
        db.write(Timestamp(1), |tx| {
            assert!(tx.keep_report(&bundle, Timestamp(1))?);
            assert!(!tx.keep_report(&bundle, Timestamp(2))?);
            Ok(())
        })
        .unwrap();
        drop(db);
        let mut db = crate::Store::open(&path, crate::Retention::default()).unwrap();
        db.read(|r| {
            assert_eq!(r.report_detail(id)?.unwrap().bundle, bundle);
            assert_eq!(r.list::<ReportDelivery>(QUEUE)?.len(), 1);
            Ok(())
        })
        .unwrap();
        db.write(Timestamp(3), |tx| {
            tx.withdraw_report(id, Timestamp(3))?;
            Ok(())
        })
        .unwrap();
        db.read(|r| {
            assert!(r.report_detail(id)?.is_none());
            let summary = &r.report_summaries()?[0];
            assert_eq!(summary.status(), ReportProgress::Withdrawn);
            assert!(summary.words.is_none());
            assert!(matches!(
                r.list::<ReportDelivery>(QUEUE)?[0].delivery,
                Delivery::Withdraw(_)
            ));
            Ok(())
        })
        .unwrap();
    }
}
