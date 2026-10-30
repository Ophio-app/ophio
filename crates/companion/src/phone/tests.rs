// SPDX-License-Identifier: Apache-2.0
use super::*;
use sha2::{Digest, Sha256};

/// `adb` as a script: each call gets the first answer written for its
/// arguments, and the last one for them stays to answer again.
#[derive(Default)]
struct Script {
    answers: Vec<(Vec<String>, Output)>,
    calls: Vec<(Vec<String>, Option<String>)>,
}
impl Script {
    fn on(mut self, args: &[&str], stdout: &str) -> Self {
        self.answers.push((
            args.iter().map(|a| (*a).to_owned()).collect(),
            Output {
                ok: true,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        ));
        self
    }
    fn failing(mut self, args: &[&str], stderr: &str) -> Self {
        self.answers.push((
            args.iter().map(|a| (*a).to_owned()).collect(),
            Output {
                ok: false,
                stdout: String::new(),
                stderr: stderr.into(),
            },
        ));
        self
    }
}
impl Adb for Script {
    async fn adb(
        &mut self,
        args: &[&str],
        input: Option<&str>,
        _limit: Duration,
    ) -> Result<Output> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        self.calls.push((args.clone(), input.map(str::to_owned)));
        let matching: Vec<_> = self
            .answers
            .iter()
            .enumerate()
            .filter(|(_, (wanted, _))| {
                wanted.len() == args.len()
                    && wanted.iter().zip(&args).all(|(w, a)| w == ANY || w == a)
            })
            .map(|(at, _)| at)
            .collect();
        let Some(&at) = matching.first() else {
            panic!("adb was run with arguments nobody expected: {args:?}");
        };
        Ok(if matching.len() > 1 {
            self.answers.remove(at).1
        } else {
            self.answers[at].1.clone()
        })
    }
}

/// An argument that matches any.
const ANY: &str = "<any>";

fn phones(script: Script) -> Phones<Script> {
    Phones::new(script)
}

const DEVICES: &[&str] = &["devices", "-l"];
const MDNS: &[&str] = &["mdns", "services"];

fn listing(lines: &[&str]) -> String {
    format!("List of devices attached\n{}\n\n", lines.join("\n"))
}

fn device(serial: &str) -> Device {
    Device {
        serial: serial.into(),
        state: "device".into(),
        model: Some("Galaxy S24".into()),
    }
}

#[test]
fn reads_adb_s_listings() {
    let devices = parse_devices(&listing(&[
        "* daemon started successfully",
        "192.168.1.20:41235     device product:e3q model:Galaxy_S24 device:e3q transport_id:3",
        "R5CT10ABCDE            unauthorized usb:1-2 transport_id:4",
        "emulator-5570          offline transport_id:5",
    ]));
    assert_eq!(
        devices,
        [
            device("192.168.1.20:41235"),
            Device {
                serial: "R5CT10ABCDE".into(),
                state: "unauthorized".into(),
                model: None,
            },
            Device {
                serial: "emulator-5570".into(),
                state: "offline".into(),
                model: None,
            },
        ]
    );
    assert!(devices[0].wireless() && devices[0].ready());
    assert!(!devices[1].wireless() && !devices[1].ready());
    assert_eq!(devices[1].name(), "R5CT10ABCDE");
    assert!(
        Device {
            serial: "adb-R5CT10ABCDE-x1y2z3._adb-tls-connect._tcp".into(),
            state: "device".into(),
            model: None,
        }
        .wireless()
    );

    let services = parse_services(
        "List of discovered mdns services\n\
         adb-R5CT10ABCDE-x1y2z3\t_adb-tls-connect._tcp.\t192.168.1.20:41235\n\
         ophio-abc\t_adb-tls-pairing._tcp\t192.168.1.20:37123\n\
         printer\t_ipp._tcp.\t192.168.1.9:631\n",
    );
    assert_eq!(
        services,
        [
            Service {
                name: "adb-R5CT10ABCDE-x1y2z3".into(),
                kind: ServiceKind::Connect,
                address: "192.168.1.20:41235".into(),
            },
            Service {
                name: "ophio-abc".into(),
                kind: ServiceKind::Pairing,
                address: "192.168.1.20:37123".into(),
            },
        ]
    );
}

#[test]
fn addresses_are_an_ip_and_a_port() {
    assert_eq!(address_host("192.168.1.20:37123"), Some("192.168.1.20"));
    assert_eq!(address_host("[fe80::1%wlan0]:37123"), Some("fe80::1%wlan0"));
    assert_eq!(address_host("phone.local:37123"), None);
    assert_eq!(address_host("192.168.1.20"), None);
    assert_eq!(address_host("192.168.1.20:99999"), None);
    assert!(check_address("192.168.1.20:5555; reboot").is_err());
}

#[test]
fn the_qr_code_keeps_its_password_out_of_logs() {
    let pairing = QrPairing::new().unwrap();
    assert!(pairing.name.starts_with("ophio-"));
    assert_eq!(pairing.password.len(), 12);
    assert!(pairing.password.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_eq!(
        pairing.code(),
        format!("WIFI:T:ADB;S:{};P:{};;", pairing.name, pairing.password)
    );
    assert!(!format!("{pairing:?}").contains(&pairing.password));
    assert_ne!(QrPairing::new().unwrap(), pairing);
}

#[tokio::test(start_paused = true)]
async fn pairs_with_the_phone_that_scanned_the_code() {
    let pairing = QrPairing::fixed("ophio-abc", "secret12");
    let script = Script::default()
        .on(MDNS, "")
        .on(
            MDNS,
            "ophio-abc\t_adb-tls-pairing._tcp.\t192.168.1.20:37123\n",
        )
        .on(
            &["pair", "192.168.1.20:37123"],
            "Successfully paired to 192.168.1.20:37123 [guid=adb-R5CT10ABCDE-x1y2z3]\n",
        )
        .on(MDNS, "")
        .on(
            MDNS,
            "adb-R5CT10ABCDE-x1y2z3\t_adb-tls-connect._tcp.\t192.168.1.20:41235\n",
        )
        .on(
            &["connect", "192.168.1.20:41235"],
            "connected to 192.168.1.20:41235\n",
        )
        .on(
            DEVICES,
            &listing(&["192.168.1.20:41235 device product:e3q model:Galaxy_S24 transport_id:3"]),
        );
    let mut phones = phones(script);
    assert_eq!(phones.pair_qr(&pairing).await.unwrap(), None);
    assert_eq!(
        phones.pair_qr(&pairing).await.unwrap(),
        Some(device("192.168.1.20:41235"))
    );
    // The password went in on standard input, and on no command line.
    let calls = &phones.adb.calls;
    let pair = calls.iter().find(|(args, _)| args[0] == "pair").unwrap();
    assert_eq!(pair.1.as_deref(), Some("secret12\n"));
    assert!(
        calls
            .iter()
            .all(|(args, _)| !args.concat().contains("secret12"))
    );
}

#[tokio::test(start_paused = true)]
async fn says_why_pairing_failed() {
    let mut phones = phones(
        Script::default()
            .failing(
                &["pair", "192.168.1.20:37123"],
                "Failed: Wrong password or connection was dropped.",
            )
            .on(
                MDNS,
                "ophio-abc\t_adb-tls-pairing._tcp.\t192.168.1.20:37123\n",
            ),
    );
    for code in ["12345", "12345a", "1234567"] {
        let error = phones
            .pair_code(Some("192.168.1.20:37123"), code)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("six digits"), "{error}");
    }
    let error = phones.pair_code(None, "123 456").await.unwrap_err();
    assert!(error.to_string().contains("refused the code"), "{error}");
    assert_eq!(
        phones.adb.calls.last().unwrap().1.as_deref(),
        Some("123456\n")
    );

    // Paired, but the phone never offers a connection.
    let mut phones = self::phones(
        Script::default()
            .on(
                &["pair", "192.168.1.20:37123"],
                "Successfully paired to 192.168.1.20:37123\n",
            )
            .on(
                MDNS,
                "adb-OTHER\t_adb-tls-connect._tcp.\t192.168.1.30:41235\n",
            ),
    );
    let error = phones
        .pair_code(Some("192.168.1.20:37123"), "123456")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("phone connect"), "{error}");
    let looks = phones
        .adb
        .calls
        .iter()
        .filter(|(args, _)| args[0] == "mdns")
        .count();
    assert_eq!(looks, CONNECT_LOOKS as usize);
}

#[tokio::test]
async fn pairs_by_code_with_the_only_phone_asking() {
    let mut phones = phones(Script::default().on(MDNS, ""));
    let error = phones.pair_code(None, "123456").await.unwrap_err();
    assert!(error.to_string().contains("No phone is asking"), "{error}");

    let mut phones = self::phones(Script::default().on(
        MDNS,
        "a\t_adb-tls-pairing._tcp.\t192.168.1.20:37123\nb\t_adb-tls-pairing._tcp.\t192.168.1.21:37123\n",
    ));
    let error = phones.pair_code(None, "123456").await.unwrap_err();
    assert!(error.to_string().contains("Several phones"), "{error}");
}

#[tokio::test]
async fn chooses_the_phone_to_use() {
    let ready = "192.168.1.20:41235 device model:Galaxy_S24";
    let waiting = "R5CT10ABCDE unauthorized usb:1-2";
    let other = "emulator-5570 device model:sdk_gphone64";

    let mut phones = phones(Script::default().on(DEVICES, &listing(&[])));
    let error = phones.choose(None).await.unwrap_err();
    assert!(
        error.to_string().contains("No phone is connected"),
        "{error}"
    );

    let mut phones = self::phones(Script::default().on(DEVICES, &listing(&[ready, waiting])));
    assert_eq!(
        phones.choose(None).await.unwrap(),
        device("192.168.1.20:41235")
    );

    let mut phones = self::phones(Script::default().on(DEVICES, &listing(&[waiting])));
    let error = phones.choose(None).await.unwrap_err();
    assert!(error.to_string().contains("allow debugging"), "{error}");

    let mut phones = self::phones(Script::default().on(DEVICES, &listing(&[ready, other])));
    let error = phones.choose(None).await.unwrap_err();
    assert!(error.to_string().contains("--device"), "{error}");
    assert_eq!(
        phones.choose(Some("emulator-5570")).await.unwrap().serial,
        "emulator-5570"
    );
    let error = phones.choose(Some("nothing")).await.unwrap_err();
    assert!(error.to_string().contains("No phone nothing"), "{error}");
}

fn app_file(dir: &Path, certificate: &[u8]) -> PathBuf {
    let path = dir.join("ophio.apk");
    std::fs::write(&path, apk::tests::signed_app(certificate)).unwrap();
    path
}

fn release() -> String {
    apk::hex(&Sha256::digest(b"release certificate").into())
}

#[test]
fn trusts_the_public_certificate_unless_built_for_personal_phones() {
    assert_ne!(PUBLIC_CERT_SHA256, PERSONAL_CERT_SHA256);
    if cfg!(ophio_personal_signing) {
        assert_eq!(RELEASE_CERT_SHA256, PERSONAL_CERT_SHA256);
    } else {
        assert_eq!(
            RELEASE_CERT_SHA256,
            "2df838f528a3edf7e6450eeac2b584e86820640876448cb5e2844c1cdd4b9516"
        );
    }
}

#[tokio::test]
async fn installs_only_the_release_signed_app() {
    let dir = tempfile::tempdir().unwrap();
    let phone = device("192.168.1.20:41235");

    // Signed otherwise: adb is never asked.
    let mut phones = phones(Script::default());
    phones.release = release();
    let other = app_file(dir.path(), b"someone else's certificate");
    let error = phones.install(&phone, &other).await.unwrap_err();
    assert!(error.to_string().contains("isn't signed with"), "{error}");
    assert!(phones.adb.calls.is_empty());
    let missing = phones
        .install(&phone, &dir.path().join("none.apk"))
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("There is no app"), "{missing}");
    // The real release key is the default.
    assert_eq!(self::phones(Script::default()).release, RELEASE_CERT_SHA256);

    let path = app_file(dir.path(), b"release certificate");
    let install = [
        "-s",
        "192.168.1.20:41235",
        "install",
        "-r",
        "--user",
        "10",
        ANY,
    ];
    let user = ["-s", "192.168.1.20:41235", "shell", "am get-current-user"];
    let sdk = [
        "-s",
        "192.168.1.20:41235",
        "shell",
        "getprop ro.build.version.sdk",
    ];
    let cases = [
        (
            "Performing Streamed Install\nSuccess\n",
            true,
            Some(Installed::Done),
        ),
        (
            "adb: failed to install: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Existing package app.ophio signatures do not match newer version; ignoring!]",
            false,
            Some(Installed::OtherKey),
        ),
        (
            "Failure [INSTALL_FAILED_VERSION_DOWNGRADE]",
            false,
            Some(Installed::Newer),
        ),
        (
            "Failure [INSTALL_FAILED_ABORTED: User rejected permissions]",
            false,
            Some(Installed::Refused("INSTALL_FAILED_ABORTED".into())),
        ),
        ("Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]", false, None),
    ];
    for (said, ok, outcome) in cases {
        let script = Script::default().on(&sdk, "34\n").on(&user, "10\n");
        let script = if ok {
            script.on(&install, said)
        } else {
            script.failing(&install, said)
        };
        let mut phones = self::phones(script);
        phones.release = release();
        match (phones.install(&phone, &path).await, outcome) {
            (Ok(got), Some(wanted)) => assert_eq!(got, wanted),
            (Err(error), None) => assert!(
                error.to_string().contains("INSUFFICIENT_STORAGE"),
                "{error}"
            ),
            (got, wanted) => panic!("{said}: {got:?}, not {wanted:?}"),
        }
        assert!(
            phones
                .adb
                .calls
                .iter()
                .all(|(args, _)| args[0] != "uninstall")
        );
        // A private copy was installed, and is gone.
        let (args, _) = phones.adb.calls.last().unwrap();
        let installed = Path::new(args.last().unwrap());
        assert_ne!(installed, path);
        assert!(!installed.exists());
    }

    // A phone older than Android 10 would check only the JAR signature.
    let mut phones = self::phones(Script::default().on(&sdk, "28\n"));
    phones.release = release();
    let error = phones.install(&phone, &path).await.unwrap_err();
    assert!(error.to_string().contains("older than 10"), "{error}");
    assert!(
        phones
            .adb
            .calls
            .iter()
            .all(|(args, _)| args[2] != "install")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn installs_no_app_through_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let real = app_file(dir.path(), b"release certificate");
    let link = dir.path().join("link.apk");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut phones = phones(Script::default());
    phones.release = release();
    let error = phones
        .install(&device("192.168.1.20:41235"), &link)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a link"), "{error}");
    let error = phones
        .install(&device("192.168.1.20:41235"), dir.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("isn't a file"), "{error}");
    assert!(phones.adb.calls.is_empty());
}

#[test]
fn prints_what_phones_and_the_network_say_as_text() {
    let named = Device {
        serial: "192.168.1.20:41235".into(),
        state: "device".into(),
        model: Some("Pixel\x1b]0;owned\x07 9\u{202e}".into()),
    };
    assert!(
        !named.name().contains(['\x1b', '\x07', '\u{202e}']),
        "{}",
        named.name()
    );
    let unnamed = Device {
        serial: "adb-\x1b[2J._adb-tls-connect._tcp".into(),
        state: "device".into(),
        model: None,
    };
    assert!(!unnamed.label().contains('\x1b'), "{}", unnamed.label());
    let output = Output {
        ok: false,
        stdout: "error:\x1b[31m closed\n".into(),
        stderr: String::new(),
    };
    assert_eq!(said(&output), "error:\\u{1b}[31m closed");
    let error = check_address("phone\x1b[2J").unwrap_err().to_string();
    assert!(
        error.contains("isn't an IP address") && !error.contains('\x1b'),
        "{error}"
    );
}

fn shell(command: &str) -> [&str; 4] {
    ["-s", "emulator-5570", "shell", command]
}

#[tokio::test]
async fn opens_the_app_or_tailscale() {
    let phone = device("emulator-5570");
    let start = shell("am start -W -n app.ophio/.MainActivity");
    let list = shell("pm list packages com.tailscale.ipn");
    let launch = shell("monkey -p com.tailscale.ipn -c android.intent.category.LAUNCHER 1");
    let store =
        shell("am start -a android.intent.action.VIEW -d 'market://details?id=com.tailscale.ipn'");

    let mut phones = phones(Script::default().on(&start, "Status: ok\nLaunchState: COLD\n"));
    assert_eq!(phones.open(&phone, Open::App).await.unwrap(), Opened::App);

    let mut phones = self::phones(Script::default().on(
        &start,
        "Error type 3\nError: Activity class {app.ophio/app.ophio.MainActivity} does not exist.\n",
    ));
    let error = phones.open(&phone, Open::App).await.unwrap_err();
    assert!(error.to_string().contains("phone install"), "{error}");

    let mut phones = self::phones(
        Script::default()
            .on(&list, "package:com.tailscale.ipn\n")
            .on(&launch, "Events injected: 1\n"),
    );
    assert_eq!(
        phones.open(&phone, Open::Tailscale).await.unwrap(),
        Opened::Tailscale
    );

    let mut phones = self::phones(Script::default().on(&list, "").on(
        &store,
        "Starting: Intent { act=android.intent.action.VIEW }\n",
    ));
    assert_eq!(
        phones.open(&phone, Open::Tailscale).await.unwrap(),
        Opened::Store
    );
}

#[tokio::test]
async fn sends_the_pairing_link_on_standard_input() {
    let phone = device("emulator-5570");
    let link = "ophio://pair?c=ABCDEF234567&alerts=on";
    let mut phones = phones(Script::default().on(
        &["-s", "emulator-5570", "shell"],
        "Starting: Intent { act=android.intent.action.VIEW dat=ophio://pair/... }\nStatus: ok\n",
    ));
    phones.link(&phone, link).await.unwrap();
    let (args, input) = &phones.adb.calls[0];
    assert!(!args.concat().contains("ABCDEF"));
    assert_eq!(
        input.as_deref(),
        Some(
            "am start -W -a android.intent.action.VIEW -d 'ophio://pair?c=ABCDEF234567&alerts=on' -n app.ophio/.MainActivity\nexit\n"
        )
    );
    for unsafe_link in [
        "https://example.com/pair?c=A",
        "ophio://pair?c=A'; reboot; '",
        "ophio://pair?c=A\nreboot",
        "ophio://open/v1/x",
    ] {
        assert!(
            phones.link(&phone, unsafe_link).await.is_err(),
            "{unsafe_link}"
        );
    }
    assert_eq!(phones.adb.calls.len(), 1);
}

#[tokio::test]
async fn lets_alerts_through() {
    let phone = device("emulator-5570");
    let user = shell("am get-current-user");
    let grant = shell("pm grant --user 0 app.ophio android.permission.POST_NOTIFICATIONS");
    let exempt = shell("dumpsys deviceidle whitelist +app.ophio");

    let mut phones = phones(
        Script::default()
            .on(&user, "0\n")
            .on(&grant, "")
            .on(&exempt, "Added: app.ophio\n"),
    );
    let allowed = phones.alerts(&phone).await.unwrap();
    assert_eq!(
        allowed,
        AlertsAllowed {
            notifications: true,
            battery: true
        }
    );
    assert!(alerts_said(&phone, allowed).contains("without battery limits"));

    // Before Android 13 there is nothing to grant.
    let mut phones = self::phones(
        Script::default()
            .on(&user, "0\n")
            .failing(&grant, "Exception occurred while executing 'grant':\njava.lang.IllegalArgumentException: Unknown permission: android.permission.POST_NOTIFICATIONS")
            .on(&exempt, ""),
    );
    assert_eq!(
        phones.alerts(&phone).await.unwrap(),
        AlertsAllowed {
            notifications: true,
            battery: false
        }
    );

    let mut phones = self::phones(
        Script::default()
            .on(&user, "0\n")
            .failing(&grant, "Exception occurred while executing 'grant':\njava.lang.IllegalArgumentException: Unknown package: app.ophio"),
    );
    let error = phones.alerts(&phone).await.unwrap_err();
    assert!(error.to_string().contains("isn't installed"), "{error}");
}

#[tokio::test]
async fn turns_wireless_debugging_off() {
    let wireless = device("192.168.1.20:41235");
    let put = [
        "-s",
        "192.168.1.20:41235",
        "shell",
        "settings put global adb_wifi_enabled 0",
    ];
    let get = [
        "-s",
        "192.168.1.20:41235",
        "shell",
        "settings get global adb_wifi_enabled",
    ];
    let disconnect = ["disconnect", "192.168.1.20:41235"];
    let ended = |script: Script| {
        script
            .failing(&get, "error: closed")
            .on(&disconnect, "disconnected 192.168.1.20:41235\n")
    };
    // The phone took the setting, then ended the connection.
    let mut phones = phones(ended(Script::default().on(&put, "")));
    assert_eq!(phones.off(&wireless).await.unwrap(), Off::Done);
    assert_eq!(phones.adb.calls.len(), 3);
    // The connection ended before it answered: maybe still on.
    let mut phones = self::phones(ended(Script::default().failing(&put, "error: closed")));
    assert_eq!(phones.off(&wireless).await.unwrap(), Off::Unsure);
    // It still answers, and says it is off.
    let mut phones = self::phones(
        Script::default()
            .failing(&put, "error: closed")
            .on(&get, "0\n")
            .on(&disconnect, ""),
    );
    assert_eq!(phones.off(&wireless).await.unwrap(), Off::Done);
    // It refused, or says it is still on.
    let mut phones = self::phones(ended(Script::default().failing(
        &put,
        "Security exception: Permission denial: \x1b]0;title\x07writing to settings",
    )));
    let Off::Failed(why) = phones.off(&wireless).await.unwrap() else {
        panic!("not refused");
    };
    assert!(
        why.contains("Permission denial") && !why.contains('\x1b'),
        "{why}"
    );
    let mut phones = self::phones(
        Script::default()
            .on(&put, "")
            .on(&get, "1\n")
            .on(&disconnect, ""),
    );
    assert!(matches!(
        phones.off(&wireless).await.unwrap(),
        Off::Failed(why) if why.contains("still on")
    ));

    let mut phones = self::phones(Script::default());
    assert_eq!(
        phones.off(&device("R5CT10ABCDE")).await.unwrap(),
        Off::Cable
    );
    assert!(phones.adb.calls.is_empty());
}

#[test]
fn explains_a_phone_with_another_key() {
    let said = other_key(&device("x"));
    assert!(said.contains("different key") && said.contains("Uninstall"));
}
