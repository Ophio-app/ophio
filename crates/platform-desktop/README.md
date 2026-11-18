# platform-desktop

Desktop sharing as Windows and a Mac both do it. `ManagedDesktop` is the
shared session: it lists monitors and the combined desktop, captures frames,
fences input to the geometry the phone last saw, sequences and releases held
keys and buttons, and closes every capture and input when sharing ends. It
implements `DesktopBackend` through the objects it returns.

Each system supplies only a `Driver`: its monitors, a `Capture` timeline and
an `Injector` that puts events in the system's queue. `platform-windows` and
`platform-macos` each construct a `ManagedDesktop` with their own driver;
neither depends on the other. Linux shares its desktop through
`platform-linux` and does not use this crate.

Nothing here calls the operating system, so every test runs on any host:

```sh
cargo test -p platform-desktop
```
