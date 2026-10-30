// SPDX-License-Identifier: Apache-2.0
fn main() {
    #[cfg(target_os = "linux")]
    match sandbox::shim::main() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("sandbox shim: {error}");
            std::process::exit(125);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("sandbox shim requires Linux");
        std::process::exit(125);
    }
}
