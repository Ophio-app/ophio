# Dependency licenses

Direct dependencies of the Library and terminal adapters, verified from downloaded
Cargo package manifests. Versions below are the resolved versions in Cargo.lock;
Cargo.toml contains explicit compatible version requirements.

| Dependency | License selected / declared |
| --- | --- |
| cap-std 4.0.3, cap-fs-ext 4.0.3 | Apache-2.0 (also MIT / LLVM exception options) |
| rustix 1.1.5 | Apache-2.0 (also MIT / LLVM exception options) |
| sha2 0.10.9 | Apache-2.0 OR MIT |
| same-file 1.0.6, ignore 0.4.33 | MIT (also Unlicense option) |
| notify 8.2.0 | CC0-1.0 |
| portable-pty 0.9.0, nix 0.29.0 | MIT |
| windows-sys 0.61.2 | Apache-2.0 OR MIT |
| tempfile 3.27.0 (Library tests) | Apache-2.0 OR MIT |
| serde 1.0.229, serde_json 1.0.151 | Apache-2.0 OR MIT |
| uuid 1.26.1 | Apache-2.0 OR MIT |
| bytes 1.12.1 | MIT |

None is GPL- or LGPL-licensed. notify's CC0 dedication permits distribution
with Apache-2.0 product-owned code; it is not relicensed by this project.
