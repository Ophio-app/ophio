// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Signal {
    OffSchemaOutput,
    BlockedNetwork,
    BlockedPath,
    RelayRefusal,
    CanarySeen,
    BudgetSpent,
    GateTampering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quarantine {
    pub stop: bool,
    pub quarantine_report: bool,
    pub count_against_reporter: bool,
}

impl Signal {
    pub fn decision(self) -> Quarantine {
        let hostile = !matches!(self, Self::BudgetSpent | Self::RelayRefusal);
        Quarantine {
            stop: true,
            quarantine_report: hostile,
            count_against_reporter: hostile,
        }
    }
}
