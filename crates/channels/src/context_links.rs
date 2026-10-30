// SPDX-License-Identifier: Apache-2.0
//! Object references contain identity only. Opening one requires local unlock,
//! a paired computer, a fresh access check and a separate navigation confirmation.
use protocol::{ApprovalId, AttentionId, HostId, LibraryRootId, TaskId};

pub const OBJECT_LINK_PREFIX: &str = "ophio-object://open/v1/";

pub fn task(host: HostId, task: TaskId) -> String {
    format!("{OBJECT_LINK_PREFIX}{host}/task/{task}")
}
pub fn approval(host: HostId, approval: ApprovalId) -> String {
    format!("{OBJECT_LINK_PREFIX}{host}/approval/{approval}")
}
pub fn attention(host: HostId, attention: AttentionId) -> String {
    format!("{OBJECT_LINK_PREFIX}{host}/attention/{attention}")
}
pub fn file(host: HostId, root: LibraryRootId, path: &str) -> Option<String> {
    if path.is_empty()
        || path.len() > 2048
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return None;
    }
    let mut encoded = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(b >> 4)]));
            encoded.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    Some(format!(
        "{OBJECT_LINK_PREFIX}{host}/file/{root}?path={encoded}"
    ))
}

pub(crate) fn append(text: &mut String, link: &str) {
    text.push_str("\nItem link (copy into the app’s Settings):\n");
    text.push_str(link);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_constants_agree() {
        let ts = include_str!("../../../client/src/platform/links.ts");
        let kotlin = include_str!(
            "../../../client/src-tauri/plugins/device/android/src/main/java/AppLinks.kt"
        );
        let manifest =
            include_str!("../../../client/src-tauri/gen/android/app/src/main/AndroidManifest.xml");
        // Links made before the product was named still open on the phone.
        let earlier = "companion-object://open/v1/";
        for (name, prefix) in [
            ("OBJECT_LINK_PREFIX", OBJECT_LINK_PREFIX),
            ("EARLIER_OBJECT_LINK_PREFIX", earlier),
        ] {
            let literal = format!("{name} = \"{prefix}\"");
            assert!(ts.contains(&literal), "{literal}");
            assert!(kotlin.contains(&literal), "{literal}");
            let scheme = prefix.split("://").next().unwrap();
            assert!(manifest.contains(&format!(
                "android:scheme=\"{scheme}\" android:host=\"open\" android:pathPrefix=\"/v1/\""
            )));
        }
    }
    #[test]
    fn references_remain_copyable_after_channel_escaping() {
        let link = task(HostId::new(), TaskId::new());
        let mut text = "Task <script> & @everyone *title*".to_owned();
        append(&mut text, &link);
        let telegram = crate::core::escape_html(&text);
        assert!(telegram.contains("&lt;script&gt; &amp;"));
        assert!(telegram.ends_with(&link));
        let discord = crate::discord::discord_text(&text);
        assert!(discord.contains("\\*title\\*"));
        assert!(discord.ends_with(&link));
        assert!(!link.contains("token"));
    }
    #[test]
    fn all_object_kinds_and_safe_file_paths() {
        let host = HostId::new();
        assert!(approval(host, ApprovalId::new()).contains("/approval/"));
        assert!(attention(host, AttentionId::new()).contains("/attention/"));
        let root = LibraryRootId::new();
        let link = file(host, root, "notes/one & two.md").unwrap();
        assert!(link.ends_with("?path=notes%2Fone%20%26%20two.md"));
        for path in ["", "../secret", "/absolute", "a//b", "a\\b", "C:/a", "a\0b"] {
            assert!(file(host, root, path).is_none());
        }
    }
}
