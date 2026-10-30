// SPDX-License-Identifier: Apache-2.0
use clap::{Parser, Subcommand};
use intake::{
    Store,
    maintainer::{self, Command, ReportFilter},
};
use protocol::{ReportId, ReportQuestionId, time::Timestamp};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "ophio-intake",
    version,
    about = "Receive private problem reports"
)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}
#[derive(Debug, Subcommand)]
enum Action {
    /// Create a private data directory and credentials. Does not start a service.
    Init {
        #[arg(long)]
        data: PathBuf,
    },
    /// Run until interrupted. TLS belongs at the reverse proxy.
    Serve {
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8070")]
        listen: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:8071")]
        maintainer_listen: SocketAddr,
    },
    /// Operate on the local database, using the same audited commands as HTTP.
    Admin {
        #[arg(long)]
        data: PathBuf,
        #[command(subcommand)]
        action: Admin,
    },
}
#[derive(Debug, Subcommand)]
enum Admin {
    Pause {
        #[arg(action=clap::ArgAction::Set)]
        paused: bool,
    },
    Close {
        #[arg(action=clap::ArgAction::Set)]
        closed: bool,
    },
    Ban {
        key_fingerprint: String,
    },
    Unban {
        key_fingerprint: String,
    },
    /// Read a KnownIssue JSON file.
    Known {
        file: PathBuf,
    },
    ForgetKnown {
        signature: String,
    },
    KnownList,
    /// Read a ReleaseEntry JSON file.
    Publish {
        file: PathBuf,
    },
    Widen {
        build: String,
        ring: String,
    },
    Halt {
        build: String,
    },
    Complete {
        build: String,
    },
    Releases,
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        signature: Option<String>,
        #[arg(long)]
        since: Option<i64>,
    },
    Get {
        report_id: ReportId,
    },
    /// Let the fixer claim a report. Nothing else is claimable.
    Admit {
        report_id: ReportId,
    },
    Claim {
        worker: String,
        #[arg(long, default_value_t = 300_000)]
        lease_ms: u64,
    },
    /// Take the oldest unchecked test submission, admitted or not.
    ClaimCheck {
        worker: String,
        #[arg(long, default_value_t = 2_700_000)]
        lease_ms: u64,
    },
    /// Status is ReportProgress JSON, for example '{"state":"reproduced"}'.
    Status {
        report_id: ReportId,
        status: String,
    },
    /// Template is QuestionTemplate JSON.
    Ask {
        report_id: ReportId,
        template: String,
    },
    CloseQuestion {
        report_id: ReportId,
        question_id: ReportQuestionId,
    },
    Switches,
    Counters,
    /// Read any maintainer Command JSON file.
    Command {
        file: PathBuf,
    },
    /// Delete expired closed-report bundles now.
    Sweep,
}
fn file<T: serde::de::DeserializeOwned>(path: PathBuf) -> intake::Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn print(value: impl serde::Serialize) -> intake::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn admin(data: PathBuf, action: Admin) -> intake::Result<()> {
    let mut store = Store::open(data.join("intake.sqlite"))?;
    let command = match action {
        Admin::Pause { paused } => Command::PipelinePaused { paused },
        Admin::Close { closed } => Command::IntakeClosed { closed },
        Admin::Ban { key_fingerprint } => Command::Ban {
            key_fingerprint,
            banned: true,
        },
        Admin::Unban { key_fingerprint } => Command::Ban {
            key_fingerprint,
            banned: false,
        },
        Admin::Known { file: path } => Command::PutKnown { issue: file(path)? },
        Admin::ForgetKnown { signature } => Command::DeleteKnown { signature },
        Admin::Publish { file: path } => Command::Publish {
            release: file(path)?,
        },
        Admin::Widen { build, ring } => Command::Widen {
            build,
            ring: serde_json::from_value(serde_json::Value::String(ring))?,
        },
        Admin::Halt { build } => Command::Halt { build },
        Admin::Complete { build } => Command::Complete { build },
        Admin::Admit { report_id } => Command::Admit { report_id },
        Admin::Claim { worker, lease_ms } => Command::Claim { worker, lease_ms },
        Admin::ClaimCheck { worker, lease_ms } => Command::ClaimCheck { worker, lease_ms },
        Admin::Status { report_id, status } => Command::SetStatus {
            report_id,
            status: serde_json::from_str(&status)?,
        },
        Admin::Ask {
            report_id,
            template,
        } => Command::Ask {
            report_id,
            template: serde_json::from_str(&template)?,
        },
        Admin::CloseQuestion {
            report_id,
            question_id,
        } => Command::CloseQuestion {
            report_id,
            question_id,
        },
        Admin::Command { file: path } => file::<maintainer::Command>(path)?,
        Admin::List {
            status,
            signature,
            since,
        } => {
            return print(store.list(ReportFilter {
                status: status.map(|s| serde_json::from_str(&s)).transpose()?,
                signature,
                since: since.map(Timestamp),
                ..Default::default()
            })?);
        }
        Admin::Get { report_id } => return print(store.get(report_id)?),
        Admin::Switches => return print(store.switches()?),
        Admin::Counters => return print(store.counters()?),
        Admin::KnownList => return print(store.known_issues()?),
        Admin::Releases => return print(store.releases(Timestamp::now())?),
        Admin::Sweep => Command::Sweep {
            retention_days: intake::config::Config::load(&data)?.retention_days,
        },
    };
    print(store.command(command, Timestamp::now())?)
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().action {
        Action::Init { data } => intake::config::init(&data)?,
        Action::Admin { data, action } => admin(data, action)?,
        Action::Serve {
            data,
            listen,
            maintainer_listen,
        } => {
            if !maintainer_listen.ip().is_loopback() {
                return Err(intake::Error::Invalid.into());
            }
            let intake = intake::Intake::open(&data)?;
            let public = tokio::net::TcpListener::bind(listen).await?;
            let private = tokio::net::TcpListener::bind(maintainer_listen).await?;
            intake.serve(public, private).await?;
        }
    }
    Ok(())
}
