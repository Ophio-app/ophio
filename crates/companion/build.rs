// SPDX-License-Identifier: Apache-2.0
//! Gives ophio.exe the Øphio icon, which Explorer shows and the local Stop
//! indicator borrows, and chooses the Android certificate the companion trusts.
use std::{env, ffi::OsString, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Every build trusts only the public release certificate, unless it is a
    // personal build for phones that still run the development one.
    println!("cargo:rustc-check-cfg=cfg(ophio_personal_signing)");
    let personal = signing_setting("OPHIO_PERSONAL_SIGNING");
    let public = signing_setting("OPHIO_PUBLIC_RELEASE");
    assert!(
        !(public && personal),
        "A public build cannot trust the personal development key; unset OPHIO_PERSONAL_SIGNING"
    );
    // The source also checks this value against cfg!, so injected --cfg flags
    // cannot silently change the certificate selected by the build setting.
    println!(
        "cargo:rustc-env=OPHIO_COMPILED_PERSONAL_SIGNING={}",
        u8::from(personal)
    );
    if personal {
        println!("cargo:rustc-cfg=ophio_personal_signing");
    }
    println!("cargo:rerun-if-env-changed=OPHIO_SOURCE_COMMIT");
    let commit = env::var("OPHIO_SOURCE_COMMIT").ok().or_else(|| {
        let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?).join("../..");
        println!("cargo:rerun-if-changed={}", root.join("crates").display());
        println!(
            "cargo:rerun-if-changed={}",
            root.join("Cargo.lock").display()
        );
        for part in ["HEAD", "refs/heads"] {
            let path = Command::new("git")
                .args(["rev-parse", "--git-path", part])
                .output()
                .ok()?;
            println!(
                "cargo:rerun-if-changed={}",
                String::from_utf8_lossy(&path.stdout).trim()
            );
        }
        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .output()
            .ok()?;
        if !status.status.success() || !status.stdout.is_empty() {
            return None;
        }
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    });
    if let Some(commit) =
        commit.filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        println!("cargo:rustc-env=OPHIO_SOURCE_COMMIT={commit}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let icon = root.join("../../packaging/icons/ophio.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by Cargo"));
    let script = out.join("ophio.rc");
    // The resource compiler reads backslashes as escapes. Resource 1 is the
    // icon Windows shows for the program; protection.rs loads it by that number.
    let path = icon.display().to_string().replace('\\', "/");
    std::fs::write(&script, format!("1 ICON \"{path}\"\n")).expect("write ophio.rc");
    let object = out.join("ophio-icon.o");
    let windres = windres();
    let status = Command::new(&windres)
        .arg("--input")
        .arg(&script)
        .args(["--input-format=rc", "--output-format=coff", "--output"])
        .arg(&object)
        .status();
    match status {
        Ok(status) if status.success() => {}
        other => panic!(
            "{} could not compile ophio.exe's icon ({other:?}); set RC to a windres",
            windres.to_string_lossy()
        ),
    }
    println!("cargo:rustc-link-arg-bin=ophio={}", object.display());
}

fn signing_setting(name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={name}");
    match env::var_os(name) {
        None => false,
        Some(value) if value == "0" => false,
        Some(value) if value == "1" => true,
        Some(value) => panic!("{name} must be 0 or 1, not {value:?}"),
    }
}

/// RC if set, else the windres beside the target's C compiler or linker, as
/// llvm-mingw installs it, else the one on PATH.
fn windres() -> OsString {
    let target = env::var("TARGET").expect("set by Cargo");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("set by Cargo");
    let name = format!("{arch}-w64-mingw32-windres");
    let lower = target.replace('-', "_");
    for key in [format!("RC_{lower}"), "RC".into()] {
        println!("cargo:rerun-if-env-changed={key}");
        if let Some(rc) = env::var_os(&key) {
            return rc;
        }
    }
    for key in [
        format!("CC_{lower}"),
        format!("CARGO_TARGET_{}_LINKER", lower.to_uppercase()),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        let beside = env::var_os(&key)
            .map(PathBuf::from)
            .and_then(|tool| Some(tool.parent()?.join(&name)));
        if let Some(beside) = beside.filter(|path| path.is_file()) {
            return beside.into();
        }
    }
    name.into()
}
