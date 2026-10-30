// SPDX-License-Identifier: Apache-2.0
//! Pure policy for owner-only Windows objects.
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Windows user SID")]
pub struct InvalidSid;

/// Canonical SID text. Construction prevents SDDL and pipe-name injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSid(String);
impl UserSid {
    pub fn parse(value: &str) -> Result<Self, InvalidSid> {
        if value.len() > 184 {
            return Err(InvalidSid);
        }
        let parts: Vec<_> = value.split('-').collect();
        if parts.len() < 4 || parts.len() > 18 || parts[0] != "S" || parts[1] != "1" {
            return Err(InvalidSid);
        }
        for (i, part) in parts.iter().enumerate().skip(2) {
            if part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(InvalidSid);
            }
            let n: u64 = part.parse().map_err(|_| InvalidSid)?;
            if (i == 2 && n >= 1 << 48) || (i > 2 && n > u64::from(u32::MAX)) {
                return Err(InvalidSid);
            }
        }
        // SYSTEM is not an interactive owner account.
        if value == "S-1-5-18" {
            return Err(InvalidSid);
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for UserSid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    File,
    Directory,
    Pipe,
}

/// Exactly two explicit grants; protected from any parent DACL. Directory
/// inheritance is deliberately disabled: children must be created explicitly.
pub fn owner_sddl(owner: &UserSid, _kind: ObjectKind) -> String {
    format!("O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)")
}

pub fn pipe_name(owner: &UserSid) -> String {
    format!(r"\\.\pipe\{}-{owner}", ports::PROGRAM_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_owner_and_system_protected_without_inheritance() {
        let sid = UserSid::parse("S-1-5-21-123-456-789-1001").unwrap();
        for kind in [ObjectKind::File, ObjectKind::Directory, ObjectKind::Pipe] {
            assert_eq!(
                owner_sddl(&sid, kind),
                "O:S-1-5-21-123-456-789-1001D:P(A;;FA;;;S-1-5-21-123-456-789-1001)(A;;FA;;;SY)"
            );
        }
    }
    #[test]
    fn stable_user_specific_pipe_and_strict_sid() {
        let a = UserSid::parse("S-1-5-21-1-2-3-1001").unwrap();
        let b = UserSid::parse("S-1-5-21-1-2-3-1002").unwrap();
        assert_eq!(pipe_name(&a), pipe_name(&a.clone()));
        assert_ne!(pipe_name(&a), pipe_name(&b));
        assert!(pipe_name(&a).starts_with(r"\\.\pipe\ophio-S-1-"));
        for bad in [
            "",
            "S-2-5-1",
            "S-1-5-18",
            "S-1-5-01",
            "S-1-5-1)(A;;FA;;;WD)",
            "S-1-5-4294967296",
            "S-1-5-1/2",
            "S-1-5-1\\x",
        ] {
            assert!(UserSid::parse(bad).is_err(), "{bad}");
        }
    }
}
