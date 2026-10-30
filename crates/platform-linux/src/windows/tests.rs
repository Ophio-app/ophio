// SPDX-License-Identifier: Apache-2.0
use super::*;
#[test]
fn recorded_hyprland_sway_kwin_and_ewmh_replies() {
    let hypr = parse_hyprland(r#"[{"address":"0x1234","mapped":true,"pid":100,"title":"Editor — notes","class":"editor","workspace":{"name":"2"},"monitor":1}]"#, r#"{"address":"0x1234"}"#).unwrap();
    assert!(hypr[0].info.focused);
    assert_eq!(hypr[0].pid, Some(100));
    assert!(
        parse_hyprland(r#"[{"address":"0x1; exec bad"}]"#, "{}")
            .unwrap()
            .is_empty()
    );
    let sway = parse_sway(r#"{"type":"root","nodes":[{"type":"output","name":"DP-1","nodes":[{"type":"workspace","name":"2","floating_nodes":[{"id":42,"type":"floating_con","app_id":"editor","name":"Notes","pid":20,"focused":true}]}]}]}"#).unwrap();
    assert_eq!(sway[0].native, "42");
    assert_eq!(sway[0].info.monitor.as_deref(), Some("DP-1"));
    assert_eq!(sway[0].info.workspace.as_deref(), Some("2"));
    let kwin = parse_kwin(r#"[{"id":"uuid","title":"Notes","app":"editor","workspace":"Desktop 2","monitor":"DP-1","focused":false,"pid":40}]"#).unwrap();
    assert_eq!(kwin[0].info.app, "editor");
    assert!(!kwin[0].info.focused);
    assert_eq!(
        x_ids("_NET_CLIENT_LIST(WINDOW): window id # 0x12, 0xAB"),
        ["0x12", "0xAB"]
    );
    let x = parse_x11(
        "0x12",
        "_NET_WM_NAME(UTF8_STRING) = \"Notes\"\nWM_CLASS(STRING) = \"editor\"\n_NET_WM_PID(CARDINAL) = 42\n_NET_WM_DESKTOP(CARDINAL) = 1",
        true,
    );
    assert_eq!(x.info.title, "Notes");
    assert_eq!(x.pid, Some(42));
    assert!(x.info.focused);
}
#[test]
fn desktop_entry_discovery_overrides_hidden_entries_and_revalidates_contents() {
    let root = tempfile::tempdir().unwrap();
    let user = root.path().join("user");
    let system = root.path().join("system");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    let text = "[Desktop Entry]\nType=Application\nName=Notes\nExec=editor %U\n";
    std::fs::write(system.join("notes.desktop"), text).unwrap();
    std::fs::write(user.join("notes.desktop"), "[Desktop Entry]\nHidden=true\n").unwrap();
    std::fs::write(
        system.join("hidden.desktop"),
        format!("{text}NoDisplay=true\n"),
    )
    .unwrap();
    std::fs::write(
        system.join("missing.desktop"),
        format!("{text}TryExec=missing\n"),
    )
    .unwrap();
    std::fs::write(system.join("visible.desktop"), text).unwrap();
    let entries = apps::discover(&[user, system.clone()], &BTreeMap::new()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Notes");
    apps::revalidate(&entries[0]).unwrap();
    std::fs::write(
        system.join("visible.desktop"),
        format!("{text}Comment=Changed\n"),
    )
    .unwrap();
    assert!(apps::revalidate(&entries[0]).is_err());
}
#[tokio::test]
async fn gnome_is_honest_and_construction_has_no_desktop_effects() {
    let backend = LinuxWindows::new(
        Backend::Gnome,
        BTreeMap::new(),
        vec![],
        Arc::new(ports::SystemClock),
    );
    let c = backend.catalog("").await.unwrap();
    assert_eq!(c.capabilities.backend, "gnome");
    assert!(matches!(
        c.capabilities.windows,
        Availability::Unsupported { .. }
    ));
    assert!(c.windows.is_empty());
    assert!(
        backend
            .act(
                &WindowAction::Focus {
                    window_id: "unknown".into()
                },
                Arc::new(Permit)
            )
            .await
            .is_err()
    );
}

struct Permit;
impl ports::windows::WindowAuthority for Permit {
    fn check(&self) -> Result<(), PlatformError> {
        Ok(())
    }
}
