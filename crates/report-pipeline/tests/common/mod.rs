// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]
use report_pipeline::{
    gates::{CommitDiff, GateConfig, GateReport, TestEvidence, TestResult, evaluate},
    schema::{ReproTest, TriageContext, TriageVerdict, parse_triage},
};
use std::{collections::BTreeMap, path::Path, process::Command};
pub const REPRO: &str = "client/src/state/repro.test.ts";
pub const SOURCE: &str = "test('repro', () => { expect(value).toBe(2); });\n";
pub struct Repo {
    pub dir: tempfile::TempDir,
    pub base: String,
}
impl Repo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q"]);
        git(path, &["config", "user.name", "Test"]);
        git(path, &["config", "user.email", "test@example.invalid"]);
        let r = Self {
            dir,
            base: String::new(),
        };
        for (p, s) in [
            (
                "Cargo.toml",
                "[package]\nname='sample'\nversion='0.1.0'\n[dependencies]\n",
            ),
            (
                "client/package.json",
                "{\"name\":\"sample\",\"dependencies\":{}}\n",
            ),
            (
                "client/src/state/existing.test.ts",
                "test('existing', () => { expect(true).toBe(true); });\n",
            ),
            ("client/src/state/ordinary.ts", "export const value = 1;\n"),
            (
                "crates/terminal/tests/existing.rs",
                "#[test] fn existing() { assert!(true); }\n",
            ),
            ("crates/terminal/src/lib.rs", "pub fn value() -> u8 { 1 }\n"),
            (
                "crates/companion/src/server.rs",
                "pub fn check_approval() -> bool { false }\n",
            ),
        ] {
            r.write(p, s.as_bytes());
        }
        let base = r.commit();
        Self { base, ..r }
    }
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
    pub fn write(&self, p: &str, b: &[u8]) {
        let path = self.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b).unwrap();
    }
    pub fn commit(&self) -> String {
        git(self.path(), &["add", "-A"]);
        git(
            self.path(),
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "test fixture",
            ],
        );
        git(self.path(), &["rev-parse", "HEAD"])
    }
    pub fn triage(&self) -> TriageVerdict {
        verdict(REPRO, "vitest")
    }
    pub fn repro(&self) -> ReproTest {
        ReproTest::validate(
            self.path(),
            &self.base,
            self.triage().repro().unwrap(),
            SOURCE.as_bytes(),
        )
        .unwrap()
    }
    pub fn fixed(&self) -> CommitDiff {
        self.write(REPRO, SOURCE.as_bytes());
        let fix = self.commit();
        CommitDiff::inspect(self.path(), &self.base, &fix).unwrap()
    }
    pub fn gates(&self) -> GateReport {
        evaluate(
            &self.fixed(),
            &self.repro(),
            &evidence(),
            GateConfig::default(),
        )
        .unwrap()
    }
}
pub fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
pub fn triage_json() -> serde_json::Value {
    serde_json::json!({"category":"bug","duplicate_of":null,"docs":{"relation":"contradicts","doc":0},"area":"work","question":null,"confidence":"high","repro":{"kind":"vitest","path":REPRO}})
}
pub fn parse(v: &serde_json::Value) -> Result<TriageVerdict, report_pipeline::schema::Rejection> {
    parse_triage(
        &serde_json::to_vec(v).unwrap(),
        TriageContext {
            candidates: 1,
            docs: 1,
        },
    )
}
pub fn verdict(path: &str, kind: &str) -> TriageVerdict {
    let mut v = triage_json();
    v["repro"] = serde_json::json!({"kind":kind,"path":path});
    parse(&v).unwrap()
}
pub fn evidence() -> TestEvidence {
    TestEvidence {
        repro_base: TestResult::Failed,
        repro_fix: TestResult::Passed,
        others_base: BTreeMap::from([("suite".into(), TestResult::Passed)]),
        others_fix: BTreeMap::from([("suite".into(), TestResult::Passed)]),
    }
}
