// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result, digest,
    gates::GateReport,
    schema::{ReproTest, ReviewVerdict, TriageVerdict},
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Identifier(String);
impl TryFrom<String> for Identifier {
    type Error = &'static str;
    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        if s.is_empty()
            || s.len() > 160
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        {
            return Err("invalid identifier");
        }
        Ok(Self(s))
    }
}
impl From<Identifier> for String {
    fn from(id: Identifier) -> Self {
        id.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkKind {
    Bundle,
    Triage,
    Test,
    BaseCommit,
    Diff,
    Gates,
    Review,
    Ci,
    Approval,
    ReleaseBuild,
}
const ORDER: [LinkKind; 10] = [
    LinkKind::Bundle,
    LinkKind::Triage,
    LinkKind::Test,
    LinkKind::BaseCommit,
    LinkKind::Diff,
    LinkKind::Gates,
    LinkKind::Review,
    LinkKind::Ci,
    LinkKind::Approval,
    LinkKind::ReleaseBuild,
];
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub kind: LinkKind,
    pub content_hash: [u8; 32],
    pub previous: [u8; 32],
    pub hash: [u8; 32],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CiResult {
    Passed,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiRun {
    pub id: Identifier,
    pub result: CiResult,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub who: Identifier,
    pub when_ms: i64,
    pub merge_commit: Identifier,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub version: u32,
    pub report_id: protocol::ReportId,
    pub links: Vec<Link>,
    pub base_commit: Identifier,
    pub ci: CiRun,
    pub approval: Approval,
    pub release_build: Identifier,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedStatement {
    pub statement: Statement,
    pub public_key: [u8; 32],
    pub signature: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopCause {
    Classified,
    Question,
    NotReproduced,
    Signal(crate::signals::Signal),
    Budget(crate::budget::StopReason),
    Gates,
    Oscillation,
    ReviewBlocked,
    CiFailed,
    OwnerClosed,
    LeaseLost,
    Interrupted,
    Infrastructure,
    /// A submitted test no agent may be shown, even reduced. Not hostile.
    UnshowableTest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopStatement {
    pub report_id: protocol::ReportId,
    pub bundle_hash: [u8; 32],
    pub stage: crate::budget::Stage,
    pub cause: StopCause,
    pub when_ms: i64,
    pub artifacts_hash: [u8; 32],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedStop {
    pub statement: StopStatement,
    pub public_key: [u8; 32],
    pub signature: Vec<u8>,
}
fn link_hash(kind: LinkKind, content: [u8; 32], previous: [u8; 32]) -> [u8; 32] {
    let mut bytes = b"ophio-report-provenance-v1\0".to_vec();
    bytes.push(kind as u8);
    bytes.extend(previous);
    bytes.extend(content);
    digest(&bytes)
}
pub struct FixEvidence<'a> {
    pub report_id: protocol::ReportId,
    pub bundle_hash: [u8; 32],
    pub triage: &'a TriageVerdict,
    pub test: &'a ReproTest,
    pub base_commit: Identifier,
    pub diff: &'a [u8],
    pub gates: &'a GateReport,
    pub review: &'a ReviewVerdict,
    pub ci: CiRun,
    pub approval: Approval,
    pub release_build: Identifier,
}
impl std::fmt::Debug for FixEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixEvidence")
            .field("report_id", &self.report_id)
            .finish_non_exhaustive()
    }
}
impl Statement {
    pub fn new(e: FixEvidence<'_>) -> Result<Self> {
        crate::git::oid(&e.base_commit.0)?;
        if e.base_commit.0 != e.test.base() {
            return Err(Error::Invalid("provenance repro base mismatch"));
        }
        crate::git::oid(&e.approval.merge_commit.0)?;
        if e.approval.when_ms < 0 {
            return Err(Error::Invalid("approval time"));
        }
        let hashes = [
            e.bundle_hash,
            digest(&serde_json::to_vec(e.triage)?),
            e.test.hash(),
            digest(e.base_commit.0.as_bytes()),
            digest(e.diff),
            digest(&serde_json::to_vec(e.gates)?),
            digest(&serde_json::to_vec(e.review)?),
            digest(&serde_json::to_vec(&e.ci)?),
            digest(&serde_json::to_vec(&e.approval)?),
            digest(e.release_build.0.as_bytes()),
        ];
        let mut previous = [0; 32];
        let mut links = Vec::new();
        for (kind, content_hash) in ORDER.into_iter().zip(hashes) {
            let hash = link_hash(kind, content_hash, previous);
            links.push(Link {
                kind,
                content_hash,
                previous,
                hash,
            });
            previous = hash;
        }
        Ok(Self {
            version: 1,
            report_id: e.report_id,
            links,
            base_commit: e.base_commit,
            ci: e.ci,
            approval: e.approval,
            release_build: e.release_build,
        })
    }
    pub fn matches(&self, kind: LinkKind, bytes: &[u8]) -> bool {
        self.links
            .iter()
            .any(|l| l.kind == kind && l.content_hash == digest(bytes))
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.links.len() != ORDER.len() || self.approval.when_ms < 0 {
            return Err(Error::Invalid("provenance structure"));
        }
        crate::git::oid(&self.base_commit.0)?;
        crate::git::oid(&self.approval.merge_commit.0)?;
        let mut previous = [0; 32];
        for (l, k) in self.links.iter().zip(ORDER) {
            if l.kind != k
                || l.previous != previous
                || l.hash != link_hash(k, l.content_hash, previous)
            {
                return Err(Error::Invalid("provenance chain"));
            }
            previous = l.hash;
        }
        for (kind, bytes) in [
            (LinkKind::BaseCommit, self.base_commit.0.as_bytes().to_vec()),
            (LinkKind::Ci, serde_json::to_vec(&self.ci)?),
            (LinkKind::Approval, serde_json::to_vec(&self.approval)?),
            (
                LinkKind::ReleaseBuild,
                self.release_build.0.as_bytes().to_vec(),
            ),
        ] {
            if !self.matches(kind, &bytes) {
                return Err(Error::Invalid("provenance metadata"));
            }
        }
        Ok(())
    }
}
/// This dedicated key never signs app releases. The caller protects its directory
/// from sandbox access and pins the public key outside the statement.
pub struct PipelineKey(SigningKey);
impl std::fmt::Debug for PipelineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PipelineKey([redacted])")
    }
}
impl PipelineKey {
    pub fn load_or_create(directory: &Path) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = directory;
            return Err(Error::Invalid(
                "key files require Unix permission enforcement",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
            let directory = directory.canonicalize()?;
            let path = directory.join("report-pipeline.ed25519");
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            match options.open(&path) {
                Ok(mut file) => {
                    let mut bytes = Zeroizing::new([0u8; 32]);
                    getrandom::fill(bytes.as_mut())
                        .map_err(|_| Error::Invalid("randomness unavailable"))?;
                    file.write_all(bytes.as_ref())?;
                    file.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            let before = fs::symlink_metadata(&path)?;
            if !before.is_file()
                || before.file_type().is_symlink()
                || before.permissions().mode() & 0o777 != 0o600
                || before.nlink() != 1
            {
                return Err(Error::Invalid(
                    "pipeline key must be a private regular 0600 file",
                ));
            }
            let mut file = OpenOptions::new().read(true).open(path)?;
            let after = file.metadata()?;
            if before.dev() != after.dev() || before.ino() != after.ino() || after.len() != 32 {
                return Err(Error::Invalid("pipeline key changed"));
            }
            let mut bytes = Zeroizing::new([0u8; 32]);
            file.read_exact(bytes.as_mut())?;
            Ok(Self(SigningKey::from_bytes(&bytes)))
        }
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    pub fn sign_stop(&self, statement: StopStatement) -> Result<SignedStop> {
        if statement.when_ms < 0 {
            return Err(Error::Invalid("stop time"));
        }
        let bytes = serde_json::to_vec(&statement)?;
        Ok(SignedStop {
            statement,
            public_key: self.public_key(),
            signature: self.0.sign(&bytes).to_bytes().to_vec(),
        })
    }
    pub fn sign(&self, statement: Statement) -> Result<SignedStatement> {
        statement.validate()?;
        let bytes = serde_json::to_vec(&statement)?;
        Ok(SignedStatement {
            statement,
            public_key: self.public_key(),
            signature: self.0.sign(&bytes).to_bytes().to_vec(),
        })
    }
}
pub fn verify(signed: &SignedStatement, trusted_key: [u8; 32]) -> Result<()> {
    signed.statement.validate()?;
    if signed.public_key != trusted_key {
        return Err(Error::Invalid("untrusted provenance key"));
    }
    let key = VerifyingKey::from_bytes(&trusted_key)
        .map_err(|_| Error::Invalid("provenance public key"))?;
    let signature = ed25519_dalek::Signature::from_slice(&signed.signature)
        .map_err(|_| Error::Invalid("provenance signature"))?;
    key.verify_strict(&serde_json::to_vec(&signed.statement)?, &signature)
        .map_err(|_| Error::Invalid("provenance signature"))
}
