// SPDX-License-Identifier: Apache-2.0
//! Authenticator codes (RFC 6238) from a setup key, as an authenticator app
//! makes them.
use ring::hmac;
use zeroize::Zeroizing;

/// Shorter keys are guessable; sites give 10 bytes (16 letters) or more.
const SHORTEST: usize = 10;
const LONGEST: usize = 128;
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}
impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
        }
    }
    fn hmac(self) -> hmac::Algorithm {
        match self {
            Self::Sha1 => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => hmac::HMAC_SHA256,
            Self::Sha512 => hmac::HMAC_SHA512,
        }
    }
}

/// A setup key and how its codes are made.
pub struct CodeKey {
    key: Zeroizing<Vec<u8>>,
    pub algorithm: Algorithm,
    pub digits: u32,
    /// Seconds each code lasts.
    pub period: u64,
}
impl std::fmt::Debug for CodeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeKey")
            .field("algorithm", &self.algorithm)
            .field("digits", &self.digits)
            .field("period", &self.period)
            .finish_non_exhaustive()
    }
}

const NOT_A_KEY: &str = "That is not an authenticator setup key. Paste the key the site shows \
                         under \"Can't scan the code?\", or the otpauth:// link.";

impl CodeKey {
    /// A key as a site gives it: its letters, in groups or not, or an
    /// `otpauth://totp/` link.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.len() > 4096 {
            return Err(NOT_A_KEY.into());
        }
        let scheme = text.split_once(':').map(|(s, _)| s);
        if scheme.is_some_and(|s| s.eq_ignore_ascii_case("otpauth")) {
            return Self::from_link(text);
        }
        Self::new(decode(text).ok_or(NOT_A_KEY)?, Algorithm::Sha1, 6, 30)
    }

    fn from_link(text: &str) -> Result<Self, String> {
        let link = url::Url::parse(text).map_err(|_| NOT_A_KEY.to_owned())?;
        match link.host_str() {
            Some(kind) if kind.eq_ignore_ascii_case("totp") => {}
            Some(kind) if kind.eq_ignore_ascii_case("hotp") => {
                return Err("That key makes counter-based codes. Øphio makes only the \
                            time-based codes most sites use."
                    .into());
            }
            _ => return Err(NOT_A_KEY.into()),
        }
        let (mut secret, mut algorithm, mut digits, mut period) = (None, Algorithm::Sha1, 6, 30);
        for (name, value) in link.query_pairs() {
            match name.to_ascii_lowercase().as_str() {
                "secret" if secret.is_none() => secret = Some(Zeroizing::new(value.into_owned())),
                "secret" => return Err(NOT_A_KEY.into()),
                "algorithm" => {
                    algorithm = match value.to_ascii_uppercase().as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        // Never the value: a link holds the key, and what
                        // was pasted wrongly may be it.
                        _ => {
                            return Err(
                                "Øphio makes codes with SHA1, SHA256 or SHA512 only.".into()
                            );
                        }
                    }
                }
                "digits" => digits = value.parse().map_err(|_| NOT_A_KEY.to_owned())?,
                "period" => period = value.parse().map_err(|_| NOT_A_KEY.to_owned())?,
                _ => {}
            }
        }
        let key = secret.as_deref().and_then(|s| decode(s)).ok_or(NOT_A_KEY)?;
        Self::new(key, algorithm, digits, period)
    }

    fn new(
        key: Zeroizing<Vec<u8>>,
        algorithm: Algorithm,
        digits: u32,
        period: u64,
    ) -> Result<Self, String> {
        if key.len() < SHORTEST || key.len() > LONGEST {
            return Err(NOT_A_KEY.into());
        }
        if !(6..=8).contains(&digits) {
            return Err("Øphio makes codes of 6, 7 or 8 digits.".into());
        }
        if !(1..=300).contains(&period) {
            return Err("Øphio makes codes that last up to five minutes.".into());
        }
        Ok(Self {
            key,
            algorithm,
            digits,
            period,
        })
    }

    /// As it is kept: an `otpauth://` link naming nothing but the key and
    /// how codes are made.
    pub fn to_kept(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "otpauth://totp/Ophio?secret={}&algorithm={}&digits={}&period={}",
            self.letters().as_str(),
            self.algorithm.name(),
            self.digits,
            self.period
        ))
    }

    /// The key in letters, without padding.
    pub fn letters(&self) -> Zeroizing<String> {
        encode(&self.key)
    }

    /// The code for `unix` seconds since 1970.
    pub fn code_at(&self, unix: u64) -> Zeroizing<String> {
        let counter = unix / self.period;
        let key = hmac::Key::new(self.algorithm.hmac(), &self.key);
        let tag = hmac::sign(&key, &counter.to_be_bytes());
        let mac = tag.as_ref();
        let at = usize::from(mac[mac.len() - 1] & 0x0f);
        let bits =
            u32::from_be_bytes([mac[at], mac[at + 1], mac[at + 2], mac[at + 3]]) & 0x7fff_ffff;
        let code = bits % 10u32.pow(self.digits);
        Zeroizing::new(format!("{code:0width$}", width = self.digits as usize))
    }

    /// Seconds the code for `unix` still lasts.
    pub fn lasts(&self, unix: u64) -> u64 {
        self.period - unix % self.period
    }
}

/// Base32 letters, in any case, grouped by spaces or dashes, padded or not.
fn decode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8));
    let (mut bits, mut held) = (0u32, 0u32);
    let mut padded = false;
    for c in text.bytes() {
        match c {
            b' ' | b'-' | b'\t' if !padded => continue,
            b'=' => {
                padded = true;
                continue;
            }
            _ if padded => return None,
            _ => {}
        }
        let value = ALPHABET.iter().position(|&a| a == c.to_ascii_uppercase())?;
        held = (held << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((held >> bits) as u8);
            held &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn encode(bytes: &[u8]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(bytes.len().div_ceil(5) * 8));
    let (mut bits, mut held) = (0u32, 0u32);
    for &b in bytes {
        held = (held << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((held >> bits) & 31) as usize] as char);
        }
        held &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((held << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(secret: &[u8], algorithm: Algorithm) -> CodeKey {
        CodeKey::new(Zeroizing::new(secret.to_vec()), algorithm, 8, 30).unwrap()
    }

    #[test]
    fn makes_the_codes_of_rfc_6238() {
        let sha1 = key(b"12345678901234567890", Algorithm::Sha1);
        for (at, code) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(sha1.code_at(at).as_str(), code, "at {at}");
        }
        let sha256 = key(b"12345678901234567890123456789012", Algorithm::Sha256);
        assert_eq!(sha256.code_at(59).as_str(), "46119246");
        assert_eq!(sha256.code_at(1_111_111_109).as_str(), "68084774");
        let sha512 = key(
            b"1234567890123456789012345678901234567890123456789012345678901234",
            Algorithm::Sha512,
        );
        assert_eq!(sha512.code_at(59).as_str(), "90693936");
        assert_eq!(sha512.code_at(1_111_111_109).as_str(), "25091201");
    }

    #[test]
    fn reads_a_key_however_a_site_writes_it() {
        // "12345678901234567890" in base32.
        let letters = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        for text in [
            letters.to_owned(),
            letters.to_ascii_lowercase(),
            "gezd gnbv gy3t qojq gezd gnbv gy3t qojq".to_owned(),
            format!("  {letters}  "),
            format!("otpauth://totp/Example:alice@example.com?secret={letters}&issuer=Example"),
            format!("OTPAUTH://TOTP/x?Secret={letters}&digits=6"),
        ] {
            let key = CodeKey::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(key.letters().as_str(), letters);
            assert_eq!(key.code_at(59).as_str(), "287082", "{text}");
        }
        let link = format!("otpauth://totp/x?secret={letters}&algorithm=sha256&digits=8&period=60");
        let key = CodeKey::parse(&link).unwrap();
        assert_eq!(
            (key.algorithm, key.digits, key.period),
            (Algorithm::Sha256, 8, 60)
        );
    }

    #[test]
    fn what_is_kept_reads_back_the_same() {
        let link = "otpauth://totp/Site:me?secret=JBSWY3DPEHPK3PXPJBSWY3DP&algorithm=SHA512&digits=7&period=45&issuer=Site";
        let key = CodeKey::parse(link).unwrap();
        let kept = key.to_kept();
        assert!(!kept.contains("Site"), "{}", kept.as_str());
        let again = CodeKey::parse(&kept).unwrap();
        assert_eq!(again.letters().as_str(), "JBSWY3DPEHPK3PXPJBSWY3DP");
        assert_eq!(
            (again.algorithm, again.digits, again.period),
            (Algorithm::Sha512, 7, 45)
        );
        assert_eq!(again.code_at(1000).as_str(), key.code_at(1000).as_str());
    }

    #[test]
    fn refuses_what_is_not_a_usable_key() {
        for text in [
            "",
            "hunter2",
            "JBSWY3DPEHPK3PX",      // 9 bytes
            "JBSWY3DPEHPK3PXP=1",   // after padding
            "JBSWY3DPEHPK3PXP0189", // not base32
            "otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP&counter=1",
            "otpauth://totp/x?issuer=nothing",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=4",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=9",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=0",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=MD5",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=JBSWY3DPEHPK3PXP",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&secret=GEZDGNBVGY3TQOJQ",
            "https://example.com/?secret=JBSWY3DPEHPK3PXP",
        ] {
            // What was pasted may be the key, so no refusal repeats any of it.
            let refusal = CodeKey::parse(text).expect_err(text);
            for part in ["JBSWY3DP", "GEZDGNBV", "MD5", "hunter2", "example.com"] {
                assert!(!refusal.contains(part), "{text}: {refusal}");
            }
        }
    }

    #[test]
    fn says_how_long_a_code_lasts() {
        let key = key(b"12345678901234567890", Algorithm::Sha1);
        assert_eq!(key.lasts(59), 1);
        assert_eq!(key.lasts(60), 30);
    }

    #[test]
    fn a_key_is_never_printed() {
        let key = CodeKey::parse("JBSWY3DPEHPK3PXPJBSW").unwrap();
        assert!(!format!("{key:?}").contains("JBSW"));
    }
}
