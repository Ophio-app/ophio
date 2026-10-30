// SPDX-License-Identifier: Apache-2.0
//! Unlocking the computer with its account's password. The password is
//! checked through PAM against a stack of Ophio's own, which gives it to
//! pam_unix and nothing else, and only a right one releases the lock. PAM's
//! copy is wiped as the check ends, Ophio's own as it lets go of them.
//!
//! libpam is loaded when Ophio starts rather than linked, so building Ophio
//! needs no PAM headers and a computer without PAM only loses unlocking.
#![allow(unsafe_code)]

use crate::native::{SessionUnlocker, valid_session};
use async_trait::async_trait;
use ports::desktop::{PlatformError, UnlockOutcome};
use protocol::accounts::Secret;
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString, OsStr, c_char, c_int, c_void},
    fs::Metadata,
    io::ErrorKind,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, Mutex, TryLockError},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

/// What Ophio's stack is called, in the directory of its own it is read from.
const SERVICE: &CStr = c"ophio-unlock";
/// First asks pam_faillock, where the computer has it, whether the account is
/// locked after wrong passwords, and stops there if it is. It only looks: a
/// try from Ophio is never counted toward that lockout, nor clears it. Given
/// no settings, it reads the computer's own from faillock.conf, so it follows
/// the computer's lockout only where every stack leaves it to that file
/// and keeps no other (`lockout_as_configured`).
const LOCKOUT: &str =
    "-auth [success=ok ignore=ignore module_unknown=ignore default=die] pam_faillock.so preauth";
/// Then asks once for the password the computer keeps for the account, and
/// passes only when it is right.
const PASSWORD: &str = "auth [success=ok default=die] pam_unix.so";
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// How long letting go of the lock may take, every way of it together.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
/// The password is the one hidden answer Ophio has. A stack asking for a
/// second is asking for something else (a code, a new password), and the
/// password must not go in its place.
const MAX_PROMPTS: u32 = 1;
/// Screen lockers that let go when sent SIGUSR1.
const LOCKERS: [&str; 2] = ["hyprlock", "swaylock"];
/// Where the computer keeps its PAM stacks, the vendor's included, besides
/// where the loaded libpam was built to read them from.
const PAM_CONFIG: [&str; 5] = [
    "/etc/pam.d",
    "/usr/lib/pam.d",
    "/usr/etc/pam.d",
    "/usr/share/pam.d",
    "/etc/pam.conf",
];
/// Where Linux-PAM's modules are kept: in `security` under one of these, or
/// under an architecture's directory in one, as Debian's are. The loaded
/// libpam's own directory is added to them.
const PAM_MODULE_ROOTS: [&str; 6] = [
    "/usr/lib",
    "/usr/lib64",
    "/usr/lib32",
    "/lib",
    "/lib64",
    "/lib32",
];
/// What the computer's pam_faillock lines may say for Ophio's to follow
/// them: which step each is and how it reports, never the lockout itself.
const LOCKOUT_WORDS: [&str; 7] = [
    "preauth",
    "authfail",
    "authsucc",
    "silent",
    "audit",
    "no_log_info",
    "nodelay",
];
/// More stack files than any computer has.
const MAX_PAM_FILES: usize = 1024;

// From <security/_pam_types.h>.
const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_MAXTRIES: c_int = 11;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x1;
const PAM_MAX_NUM_MSG: c_int = 32;

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}
#[repr(C)]
struct Response {
    resp: *mut c_char,
    retcode: c_int,
}
type ConvFn =
    unsafe extern "C" fn(c_int, *const *const Message, *mut *mut Response, *mut c_void) -> c_int;
#[repr(C)]
struct Conv {
    conv: ConvFn,
    appdata: *mut c_void,
}
type StartConfdirFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const Conv,
    *const c_char,
    *mut *mut c_void,
) -> c_int;
type AuthenticateFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type EndFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

struct Pam {
    start_confdir: StartConfdirFn,
    authenticate: AuthenticateFn,
    end: EndFn,
    // The functions above are only valid while this stays loaded.
    _library: libloading::Library,
}
impl std::fmt::Debug for Pam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pam { .. }")
    }
}
impl Pam {
    fn load() -> Result<Self, PlatformError> {
        let missing = |_| PlatformError::Unsupported("PAM isn’t available on this computer".into());
        // SAFETY: libpam is the system's own library and its initialisers
        // have no preconditions on the loading program.
        let library = unsafe { libloading::Library::new("libpam.so.0") }.map_err(missing)?;
        // SAFETY: every symbol is given its type from <security/pam_appl.h>,
        // and the copied pointers are kept beside the library that owns them.
        unsafe {
            // Linux-PAM 1.4 and later, the only way to read a stack from a
            // directory of Ophio's own.
            let start_confdir = *library
                .get::<StartConfdirFn>(b"pam_start_confdir\0")
                .map_err(|_| {
                    PlatformError::Unsupported(
                        "This computer’s PAM is too old for Ophio to check a password with".into(),
                    )
                })?;
            let authenticate = *library
                .get::<AuthenticateFn>(b"pam_authenticate\0")
                .map_err(missing)?;
            let end = *library.get::<EndFn>(b"pam_end\0").map_err(missing)?;
            Ok(Self {
                start_confdir,
                authenticate,
                end,
                _library: library,
            })
        }
    }
}

/// What PAM made of the password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checked {
    Right,
    Wrong,
    Refused,
}

/// What the conversation saw; PAM reaches it through the `appdata` pointer.
#[derive(Debug)]
struct ConvState {
    password: *const c_char,
    prompts: u32,
    told_error: bool,
    asked_other: bool,
}

/// Answers the request for the password with it, and anything else with an
/// error, so a stack wanting more than the password fails.
unsafe extern "C" fn conversation(
    count: c_int,
    messages: *const *const Message,
    replies: *mut *mut Response,
    appdata: *mut c_void,
) -> c_int {
    if count <= 0
        || count > PAM_MAX_NUM_MSG
        || messages.is_null()
        || replies.is_null()
        || appdata.is_null()
    {
        return PAM_CONV_ERR;
    }
    // SAFETY: `appdata` is the `ConvState` that `check` gave pam_start; it
    // outlives pam_authenticate, which is the only caller.
    let state = unsafe { &mut *appdata.cast::<ConvState>() };
    let count = count as usize;
    // SAFETY: PAM frees the answers, and each answer's text, with free().
    let answers =
        unsafe { libc::calloc(count, std::mem::size_of::<Response>()) }.cast::<Response>();
    if answers.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..count {
        // SAFETY: Linux-PAM passes `count` message pointers.
        let message = unsafe { *messages.add(i) };
        if message.is_null() {
            // SAFETY: `answers` holds `count` answers, filled only by strdup.
            return unsafe { discard(answers, count, PAM_CONV_ERR) };
        }
        // SAFETY: checked non-null above; PAM owns the message.
        match unsafe { (*message).style } {
            PAM_PROMPT_ECHO_OFF if state.prompts < MAX_PROMPTS => {
                state.prompts += 1;
                // SAFETY: `password` is NUL-terminated and alive (see `check`).
                let copy = unsafe { libc::strdup(state.password) };
                if copy.is_null() {
                    // SAFETY: as above.
                    return unsafe { discard(answers, count, PAM_BUF_ERR) };
                }
                // SAFETY: `i < count`, inside the calloc'd answers.
                unsafe { (*answers.add(i)).resp = copy };
            }
            PAM_ERROR_MSG => state.told_error = true,
            PAM_TEXT_INFO => {}
            _ => {
                state.asked_other = true;
                // SAFETY: as above.
                return unsafe { discard(answers, count, PAM_CONV_ERR) };
            }
        }
    }
    // SAFETY: checked non-null above; PAM takes ownership of the answers.
    unsafe { *replies = answers };
    PAM_SUCCESS
}

/// Wipe and free answers PAM will never receive.
///
/// # Safety
/// `answers` must be a calloc'd array of `count` answers whose texts are
/// null or strdup'd.
unsafe fn discard(answers: *mut Response, count: usize, status: c_int) -> c_int {
    for i in 0..count {
        // SAFETY: the caller's contract.
        let text = unsafe { (*answers.add(i)).resp };
        if !text.is_null() {
            // SAFETY: a strdup'd, NUL-terminated copy Ophio owns.
            unsafe {
                std::slice::from_raw_parts_mut(text.cast::<u8>(), libc::strlen(text)).zeroize();
                libc::free(text.cast());
            }
        }
    }
    // SAFETY: the caller's contract.
    unsafe { libc::free(answers.cast()) };
    status
}

/// Read PAM's answer, never its words: those are the stack's own and could
/// say anything.
fn verdict(status: c_int, state: &ConvState) -> Result<Checked, PlatformError> {
    match status {
        // Even when the stack let a module that asked go unanswered.
        _ if state.asked_other => Err(PlatformError::Unsupported(
            "This computer’s password check asks for more than a password, so unlock it at the computer"
                .into(),
        )),
        PAM_SUCCESS if state.prompts > 0 => Ok(Checked::Right),
        // A check that never asked for the password proves nothing.
        PAM_SUCCESS => Err(unavailable()),
        // Turned away before it was asked for, the password can't have been
        // wrong: the computer has locked the account.
        PAM_AUTH_ERR if state.prompts == 0 => Ok(Checked::Refused),
        PAM_MAXTRIES | PAM_PERM_DENIED => Ok(Checked::Refused),
        // A stack that explains itself is turning the password away for a
        // reason, most often too many wrong ones.
        _ if state.told_error => Ok(Checked::Refused),
        PAM_AUTH_ERR => Ok(Checked::Wrong),
        _ => Err(unavailable()),
    }
}

fn unavailable() -> PlatformError {
    PlatformError::TemporarilyUnavailable("The computer couldn’t check the password".into())
}

/// The stack the password is checked against: Ophio's own, never the
/// computer's `login` one. A login stack can ask for a code, a key's PIN or a
/// fingerprint where the password is expected, or let a module that turned
/// the password away be outvoted, in more ways than reading it beforehand
/// can rule out. This one gives the password to pam_unix alone.
fn stack() -> String {
    format!("{LOCKOUT}\n{PASSWORD}\n")
}

/// Writes `stack` as Ophio's service into a new directory under `scratch`
/// that only this account can reach, removed when the returned guard goes.
fn write_stack(scratch: &Path, stack: &str) -> Result<(tempfile::TempDir, CString), PlatformError> {
    let dir = tempfile::Builder::new()
        .prefix("ophio-unlock-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(scratch)
        .map_err(|_| unavailable())?;
    std::fs::write(
        dir.path().join(OsStr::from_bytes(SERVICE.to_bytes())),
        stack,
    )
    .map_err(|_| unavailable())?;
    let path = CString::new(dir.path().as_os_str().as_bytes()).map_err(|_| unavailable())?;
    Ok((dir, path))
}

/// Where the computer keeps its PAM stacks and the modules they load.
#[derive(Debug)]
struct PamFiles {
    /// Its stacks' directories, and `/etc/pam.conf`.
    config: Vec<PathBuf>,
    /// See `PAM_MODULE_ROOTS`.
    module_roots: Vec<PathBuf>,
}
impl PamFiles {
    /// The usual places and those the loaded `pam` was built with, which
    /// must be found for the stacks it reads to be known.
    fn computers(pam: &Pam) -> Result<Self, PlatformError> {
        loaded_object(pam.start_confdir as usize)
            .and_then(|library| Self::usual_and(&library))
            .ok_or_else(|| {
                PlatformError::Unsupported(
                    "Ophio can’t tell where this computer’s PAM keeps its login settings, so unlock it at the computer"
                        .into(),
                )
            })
    }

    /// The usual places and those the loaded `library` names, if it does.
    fn usual_and(library: &[u8]) -> Option<Self> {
        let built = Self::built_into(library)?;
        let mut files = Self {
            config: PAM_CONFIG.iter().map(PathBuf::from).collect(),
            module_roots: PAM_MODULE_ROOTS.iter().map(PathBuf::from).collect(),
        };
        files.config.extend(built.config);
        files.module_roots.extend(built.module_roots);
        let (mut config, mut roots) = (HashSet::new(), HashSet::new());
        files.config.retain(|place| config.insert(place.clone()));
        files.module_roots.retain(|root| roots.insert(root.clone()));
        Some(files)
    }

    /// The places `library`, a Linux-PAM libpam, names: its stacks'
    /// directories (`…/pam.d/%s`), its `pam.conf`, and the directory it
    /// loads a module named without a path from (`…/security/`). None
    /// unless it names a stacks' directory and that one.
    fn built_into(library: &[u8]) -> Option<Self> {
        let path = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
        let (mut config, mut module_roots, mut stacks) = (Vec::new(), Vec::new(), false);
        for string in library.split(|&c| c == 0).filter(|s| s.starts_with(b"/")) {
            if let Some(dir) = string
                .strip_suffix(b"/%s")
                .filter(|dir| dir.ends_with(b"/pam.d"))
            {
                config.push(path(dir));
                stacks = true;
            } else if string.ends_with(b"/pam.conf") {
                config.push(path(string));
            } else if let Some(root) = string
                .strip_suffix(b"security/")
                .filter(|root| root.ends_with(b"/"))
            {
                module_roots.push(path(root));
            }
        }
        (stacks && !module_roots.is_empty()).then_some(Self {
            config,
            module_roots,
        })
    }
}

/// The readable, non-writable segments of the loaded object containing
/// `address`. Read the object itself, not its pathname: a replacement file
/// need not be the library still loaded, even when its inode is the same.
fn loaded_object(address: usize) -> Option<Vec<u8>> {
    #[cfg(target_pointer_width = "32")]
    use libc::Elf32_Phdr as ProgramHeader;
    #[cfg(target_pointer_width = "64")]
    use libc::Elf64_Phdr as ProgramHeader;

    struct Search {
        address: usize,
        found: Option<Vec<u8>>,
    }
    unsafe extern "C" fn each(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: dl_iterate_phdr supplies a live info and passes back the
        // exclusive Search pointer for the duration of this callback.
        let (info, search) = unsafe { (&*info, &mut *data.cast::<Search>()) };
        if info.dlpi_phdr.is_null() || info.dlpi_phnum == 0 {
            return 0;
        }
        // SAFETY: the loader owns these dlpi_phnum headers and holds its
        // lock throughout this callback, so no object can be unloaded here.
        let headers =
            unsafe { std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) };
        let range = |header: &ProgramHeader| {
            let start = usize::try_from(info.dlpi_addr)
                .ok()?
                .checked_add(usize::try_from(header.p_vaddr).ok()?)?;
            let len = usize::try_from(header.p_memsz).ok()?;
            (len <= isize::MAX as usize).then_some(())?;
            Some(start..start.checked_add(len)?)
        };
        if !headers.iter().any(|header| {
            header.p_type == libc::PT_LOAD
                && range(header).is_some_and(|range| range.contains(&search.address))
        }) {
            return 0;
        }
        search.found = (|| {
            let mut bytes = Vec::new();
            for header in headers.iter().filter(|header| {
                header.p_type == libc::PT_LOAD
                    && header.p_flags & libc::PF_R != 0
                    && header.p_flags & libc::PF_W == 0
            }) {
                let range = range(header)?;
                if range.is_empty() {
                    continue;
                }
                // SAFETY: a readable, non-writable PT_LOAD segment of this
                // live object. The loader keeps it mapped while copied;
                // writable segments (and their mutable state) are excluded.
                let segment =
                    unsafe { std::slice::from_raw_parts(range.start as *const u8, range.len()) };
                bytes.extend_from_slice(segment);
                // Unrelated segments cannot join into a configuration path.
                bytes.push(0);
            }
            (!bytes.is_empty()).then_some(bytes)
        })();
        1
    }
    let mut search = Search {
        address,
        found: None,
    };
    // SAFETY: the callback only borrows Search during this synchronous call.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut search).cast()) };
    search.found
}

/// A module that locks an account after wrong passwords, in order of how
/// little Ophio can follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Lockout {
    /// pam_faillock, which may leave its settings to faillock.conf.
    Faillock,
    /// pam_tally or pam_tally2, which keep their own.
    Tally,
}

/// Not there, as Linux-PAM finds when it looks for a file.
fn missing(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Whether every stack the computer has leaves pam_faillock's settings to
/// faillock.conf, as Ophio's does, and keeps no other lockout, so a lockout
/// at the computer is one at Ophio too. A stack giving it a threshold, a
/// pause, a tally of its own or another conf file locks the account where
/// Ophio wouldn't see it, and a stack or module Ophio can't read may.
fn lockout_as_configured(files: &PamFiles) -> Result<(), PlatformError> {
    let unfollowed = || {
        PlatformError::Unsupported(
            "This computer’s login sets its own lockout after wrong passwords, which Ophio can’t follow, so unlock it at the computer"
                .into(),
        )
    };
    // Where a module named without a path may be loaded from.
    let mut modules = Vec::new();
    for root in &files.module_roots {
        modules.push(root.join("security"));
        match std::fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|_| unfollowed())?;
                    if entry
                        .file_name()
                        .as_bytes()
                        .windows(6)
                        .any(|w| w == b"-linux")
                    {
                        modules.push(entry.path().join("security"));
                    }
                }
            }
            Err(e) if missing(&e) => {}
            Err(_) => return Err(unfollowed()),
        }
    }
    // Linux-PAM's lockouts as installed there, so a stack loading one by
    // another name, through a link or a copy, is known by what it loads.
    let mut installed = Vec::new();
    for dir in &modules {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if missing(&e) => continue,
            Err(_) => return Err(unfollowed()),
        };
        for entry in entries {
            let path = entry.map_err(|_| unfollowed())?.path();
            let Some(lockout) = lockout_named(&path) else {
                continue;
            };
            match std::fs::metadata(&path) {
                Ok(meta) => installed.push((path, meta, lockout)),
                Err(e) if missing(&e) => {}
                Err(_) => return Err(unfollowed()),
            }
        }
    }
    // Each stack, and whether its lines start with the service they're for,
    // as `/etc/pam.conf`'s do.
    let mut stacks = Vec::new();
    // Where a stack included without a path is read from.
    let mut dirs = Vec::new();
    for place in &files.config {
        match std::fs::read_dir(place) {
            Ok(entries) => {
                dirs.push(place.clone());
                for entry in entries {
                    stacks.push((entry.map_err(|_| unfollowed())?.path(), false));
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) if e.kind() == ErrorKind::NotADirectory => stacks.push((place.clone(), true)),
            Err(_) => return Err(unfollowed()),
        }
    }
    let mut seen = HashSet::new();
    let mut known = HashMap::new();
    while let Some((stack, with_service)) = stacks.pop() {
        // Known by the file itself, however it was named.
        let stack = match std::fs::canonicalize(&stack) {
            Ok(stack) => stack,
            Err(e) if missing(&e) => continue,
            Err(_) => return Err(unfollowed()),
        };
        if !seen.insert((stack.clone(), with_service)) {
            continue;
        }
        if seen.len() > MAX_PAM_FILES {
            return Err(unfollowed());
        }
        let text = match std::fs::read(&stack) {
            Ok(text) => text,
            Err(e) if missing(&e) || e.kind() == ErrorKind::IsADirectory => continue,
            Err(_) => return Err(unfollowed()),
        };
        // Byte for byte, as Linux-PAM reads it, so a name is the one it opens.
        for line in pam_lines(&text) {
            let words = pam_words(&line);
            // A type, a control, then the module and what it's given.
            let fields = words.get(usize::from(with_service)..).unwrap_or_default();
            let word = |at: usize| fields.get(at).map(|w| w.to_ascii_lowercase());
            let included = if word(0).as_deref() == Some(b"@include") {
                fields.get(1)
            } else if matches!(word(1).as_deref(), Some(b"include" | b"substack")) {
                fields.get(2)
            } else {
                None
            };
            // Linux-PAM reads one named without a path from the first of its
            // directories that has it, `../` and all, so from any of them.
            if let Some(included) = included {
                let included = Path::new(OsStr::from_bytes(included));
                if included.is_absolute() {
                    stacks.push((included.to_owned(), false));
                } else {
                    stacks.extend(dirs.iter().map(|dir| (dir.join(included), false)));
                }
            }
            let Some(module) = fields.get(2) else {
                continue;
            };
            let lockout = match known.get(module) {
                Some(lockout) => *lockout,
                None => {
                    let lockout =
                        module_lockout(module, &modules, &installed).map_err(|_| unfollowed())?;
                    known.insert(module.clone(), lockout);
                    lockout
                }
            };
            match lockout {
                Some(Lockout::Faillock)
                    if fields[3..].iter().all(|option| {
                        LOCKOUT_WORDS
                            .iter()
                            .any(|word| word.as_bytes() == option.as_slice())
                    }) => {}
                Some(_) => return Err(unfollowed()),
                None => {}
            }
        }
    }
    Ok(())
}

/// What `module`, as a stack line names it, is: known by its name, the name
/// of the file it is, or being that file or a copy of one of the `installed`
/// lockouts. A module named without a path is looked for in each of
/// `modules`. Fails where Ophio can't read what Linux-PAM would load, or
/// can't know where it would load it from.
fn module_lockout(
    module: &[u8],
    modules: &[PathBuf],
    installed: &[(PathBuf, Metadata, Lockout)],
) -> std::io::Result<Option<Lockout>> {
    if module.windows(4).any(|w| w == b"$ISA") {
        return Err(ErrorKind::Unsupported.into());
    }
    let module = Path::new(OsStr::from_bytes(module));
    let paths: Vec<PathBuf> = if module.is_absolute() {
        vec![module.into()]
    } else {
        modules.iter().map(|dir| dir.join(module)).collect()
    };
    let mut lockout = lockout_named(module);
    for path in paths {
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) if missing(&e) => continue,
            Err(e) => return Err(e),
        };
        // Read as Linux-PAM must read it to load it.
        std::fs::File::open(&path)?;
        lockout = lockout.max(lockout_named(&std::fs::canonicalize(&path)?));
        for (other, known, installed) in installed {
            if (meta.dev(), meta.ino()) == (known.dev(), known.ino())
                || (meta.is_file()
                    && known.is_file()
                    && meta.len() == known.len()
                    && std::fs::read(&path)? == std::fs::read(other)?)
            {
                lockout = lockout.max(Some(*installed));
            }
        }
    }
    Ok(lockout)
}

/// Which lockout a module is, by the name of its file.
fn lockout_named(path: &Path) -> Option<Lockout> {
    let name = path.file_name().map_or(&b""[..], OsStrExt::as_bytes);
    if name.starts_with(b"pam_faillock") {
        Some(Lockout::Faillock)
    } else if name.starts_with(b"pam_tally") {
        Some(Lockout::Tally)
    } else {
        None
    }
}

/// A file's lines as Linux-PAM reads them: everything from a `#` on is left
/// out, and a line ending in a backslash, but for spaces and tabs, goes on
/// into the next unless it has a `#`, until a blank one.
fn pam_lines(text: &[u8]) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut joined = Vec::new();
    for raw in text.split(|&c| c == b'\n') {
        let end = trim_end(raw, b" \t");
        if let Some(part) = end.strip_suffix(b"\\").filter(|_| !raw.contains(&b'#')) {
            joined.extend_from_slice(trim_start(part, b" \t"));
            joined.push(b' ');
            continue;
        }
        let part = raw.split(|&c| c == b'#').next().unwrap_or_default();
        joined.extend_from_slice(trim_start(part, b" \t"));
        let line = std::mem::take(&mut joined);
        if !trim_start(&line, b" \t").is_empty() {
            lines.push(line);
        }
    }
    if !trim_start(&joined, b" \t").is_empty() {
        lines.push(joined);
    }
    lines
}

fn trim_start<'a>(bytes: &'a [u8], set: &[u8]) -> &'a [u8] {
    let start = bytes.iter().position(|c| !set.contains(c));
    &bytes[start.unwrap_or(bytes.len())..]
}

fn trim_end<'a>(bytes: &'a [u8], set: &[u8]) -> &'a [u8] {
    let end = bytes.iter().rposition(|c| !set.contains(c));
    &bytes[..end.map_or(0, |end| end + 1)]
}

/// A line's words as Linux-PAM splits them: at spaces and tabs, except that
/// a word opening with `[` runs to the first `]` not after a backslash,
/// spaces and all, and loses its brackets.
fn pam_words(line: &[u8]) -> Vec<Vec<u8>> {
    let mut words = Vec::new();
    let mut rest = line;
    loop {
        rest = trim_start(rest, b" \t\n");
        if rest.is_empty() {
            return words;
        }
        if let Some(inner) = rest.strip_prefix(b"[") {
            let mut word = Vec::new();
            let mut end = inner.len();
            let mut bytes = inner.iter().enumerate();
            while let Some((at, &c)) = bytes.next() {
                match c {
                    b']' => {
                        end = at + 1;
                        break;
                    }
                    b'\\' if inner.get(at + 1) == Some(&b']') => {
                        word.push(b']');
                        bytes.next();
                    }
                    c => word.push(c),
                }
            }
            words.push(word);
            rest = &inner[end..];
        } else {
            let end = rest
                .iter()
                .position(|c| b" \t\n".contains(c))
                .unwrap_or(rest.len());
            words.push(rest[..end].to_vec());
            rest = &rest[end..];
        }
    }
}

/// Checks `password` against the stack in `confdir`.
fn check(
    pam: &Pam,
    confdir: &CStr,
    user: &CStr,
    password: &CStr,
) -> Result<Checked, PlatformError> {
    let mut state = ConvState {
        password: password.as_ptr(),
        prompts: 0,
        told_error: false,
        asked_other: false,
    };
    let conv = Conv {
        conv: conversation,
        appdata: (&raw mut state).cast(),
    };
    let mut handle = ptr::null_mut();
    // SAFETY: NUL-terminated strings and a conversation that outlive the
    // handle, which pam_end below releases.
    let started = unsafe {
        (pam.start_confdir)(
            SERVICE.as_ptr(),
            user.as_ptr(),
            &conv,
            confdir.as_ptr(),
            &mut handle,
        )
    };
    if started != PAM_SUCCESS || handle.is_null() {
        return Err(unavailable());
    }
    // Never PAM_SILENT: a stack refusing for a reason says so, and that is
    // how a lockout is told from a wrong password.
    // SAFETY: a handle pam_start_confdir returned and nothing has ended.
    let status = unsafe { (pam.authenticate)(handle, PAM_DISALLOW_NULL_AUTHTOK) };
    // SAFETY: as above; the handle is not used again.
    unsafe { (pam.end)(handle, status) };
    verdict(status, &state)
}

/// Under no_new_privs the setuid helper pam_unix reads the computer's
/// passwords through (unix_chkpwd) runs without its privileges, so every
/// password would be turned away as wrong.
fn able_to_read_passwords(no_new_privs: bool, shadow: &Path) -> Result<(), PlatformError> {
    if no_new_privs && std::fs::File::open(shadow).is_err() {
        return Err(PlatformError::Unsupported(
            "Ophio runs without the privileges the computer’s password check needs".into(),
        ));
    }
    Ok(())
}

/// Whether `/proc/self/status` says this process can gain no privileges.
fn no_new_privs(status: &str) -> bool {
    status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .any(|(key, value)| key == "NoNewPrivs" && value.trim() == "1")
}

/// Whether `passwd` lists the account, as it does every account whose
/// password the computer keeps itself. One from a directory or systemd-homed
/// keeps it where pam_unix can't check it, and every password would be
/// called wrong.
fn local_account(passwd: &[u8], user: &CStr, uid: u32) -> Result<(), PlatformError> {
    let uid = uid.to_string();
    let listed = passwd.split(|&byte| byte == b'\n').any(|line| {
        let mut fields = line.split(|&byte| byte == b':');
        fields.next() == Some(user.to_bytes()) && fields.nth(1) == Some(uid.as_bytes())
    });
    if !listed {
        return Err(PlatformError::Unsupported(
            "This computer keeps the account’s password where Ophio can’t check it, so unlock it at the computer"
                .into(),
        ));
    }
    Ok(())
}

/// The account's own name, which the password belongs to.
fn account_name(uid: u32) -> Result<CString, PlatformError> {
    let mut size = 1024;
    loop {
        let mut buffer = vec![0 as c_char; size];
        // SAFETY: an all-zero passwd is valid: its pointers are null.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found = ptr::null_mut();
        // SAFETY: the buffer and its length are this call's own.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && size < 1 << 20 {
            size *= 2;
            continue;
        }
        if status != 0 || found.is_null() || entry.pw_name.is_null() {
            return Err(PlatformError::Unsupported(
                "This computer’s account has no name to check a password for".into(),
            ));
        }
        // SAFETY: getpwuid_r filled `entry` with strings inside `buffer`.
        return Ok(unsafe { CStr::from_ptr(entry.pw_name) }.to_owned());
    }
}

/// Lets go of the screen lock once the password was right.
#[async_trait]
trait Release: Send + Sync + std::fmt::Debug + 'static {
    async fn release(&self);
}

#[derive(Debug)]
struct SystemRelease {
    session: Option<String>,
    /// The Wayland display's socket, which names one display however many
    /// sessions the account has.
    socket: Option<PathBuf>,
    uid: u32,
}
#[async_trait]
impl Release for SystemRelease {
    async fn release(&self) {
        // One deadline for every way of letting go, after which no locker is
        // signalled, so a slow one never keeps the command, or its password,
        // waiting.
        let deadline = Instant::now() + RELEASE_TIMEOUT;
        let _ = tokio::time::timeout(RELEASE_TIMEOUT, self.release_by(deadline)).await;
    }
}
impl SystemRelease {
    async fn release_by(&self, deadline: Instant) {
        // GNOME's and KDE's lock screens let go when logind says so.
        if let Some(session) = &self.session {
            let _ = tokio::process::Command::new("loginctl")
                .arg("unlock-session")
                .arg(session)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status()
                .await;
        }
        // hyprlock and swaylock don't listen to logind, but let go on SIGUSR1.
        // A thread of its own rather than the runtime's blocking pool, where
        // the scan could wait its turn behind others.
        if let Some(socket) = self.socket.clone() {
            let uid = self.uid;
            let (done, finished) = tokio::sync::oneshot::channel();
            let spawned = std::thread::Builder::new()
                .name("ophio-release".into())
                .spawn(move || {
                    let proc_root = Path::new("/proc");
                    for pid in lockers(proc_root, uid, &socket) {
                        signal_locker(proc_root, pid, uid, &socket, deadline);
                    }
                    let _ = done.send(());
                });
            if spawned.is_ok() {
                let _ = finished.await;
            }
        }
    }
}

/// The socket a Wayland display name stands for, as libwayland finds it: a
/// path of its own, or a name in the runtime directory.
fn wayland_socket(display: &[u8], runtime_dir: Option<&[u8]>) -> Option<PathBuf> {
    let display = Path::new(OsStr::from_bytes(display));
    if display.as_os_str().is_empty() {
        return None;
    }
    if display.is_absolute() {
        return Some(display.to_owned());
    }
    let runtime_dir = Path::new(OsStr::from_bytes(runtime_dir?));
    runtime_dir.is_absolute().then(|| runtime_dir.join(display))
}

/// The screen lockers of this account on this display.
fn lockers(proc_root: &Path, uid: u32, socket: &Path) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut found: Vec<i32> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|&pid| pid > 0 && is_locker(proc_root, pid, uid, socket))
        .collect();
    found.sort_unstable();
    found
}

fn is_locker(proc_root: &Path, pid: i32, uid: u32, socket: &Path) -> bool {
    let dir = proc_root.join(pid.to_string());
    if !std::fs::metadata(&dir).is_ok_and(|m| m.uid() == uid) {
        return false;
    }
    let Ok(name) = std::fs::read_to_string(dir.join("comm")) else {
        return false;
    };
    if !LOCKERS.contains(&name.trim_end_matches('\n')) {
        return false;
    }
    let Ok(environ) = std::fs::read(dir.join("environ")) else {
        return false;
    };
    let value = |name: &[u8]| {
        environ
            .split(|&byte| byte == 0)
            .find_map(|entry| entry.strip_prefix(name)?.strip_prefix(b"="))
    };
    value(b"WAYLAND_DISPLAY")
        .and_then(|display| wayland_socket(display, value(b"XDG_RUNTIME_DIR")))
        .is_some_and(|found| found == socket)
}

/// Signals the locker `pid` to let go, unless `deadline` has passed.
fn signal_locker(proc_root: &Path, pid: i32, uid: u32, socket: &Path, deadline: Instant) -> bool {
    use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
    let Some(id) = Pid::from_raw(pid) else {
        return false;
    };
    let Ok(process) = pidfd_open(id, PidfdFlags::empty()) else {
        return false;
    };
    // Checked again now that the descriptor holds the process, so a pid
    // reused since the scan is never signalled.
    is_locker(proc_root, pid, uid, socket)
        && Instant::now() < deadline
        && pidfd_send_signal(&process, Signal::USR1).is_ok()
}

/// Checks the account's password through PAM and, when it is right, tells
/// the screen locker to let go.
#[derive(Debug)]
pub struct PasswordUnlocker {
    pam: Arc<Pam>,
    /// What the password is checked against.
    stack: Arc<str>,
    /// The computer's own PAM stacks, whose lockout Ophio's must follow.
    pam_files: Arc<PamFiles>,
    /// Where each check's stack is written, in a directory of its own.
    scratch: PathBuf,
    user: CString,
    // One check at a time: PAM modules aren't all safe to run side by side.
    gate: Arc<Mutex<()>>,
    timeout: Duration,
    release: Box<dyn Release>,
    /// Lets a test keep the worker alive after its caller has the answer.
    #[cfg(test)]
    after_answer: Option<Arc<std::sync::Barrier>>,
}
impl PasswordUnlocker {
    /// For the login `session` and the Wayland `display` Ophio runs in, whose
    /// socket a name finds in `runtime_dir`; fails where nothing could check
    /// the password or release the lock.
    pub fn new(
        session: Option<String>,
        display: Option<String>,
        runtime_dir: Option<String>,
    ) -> Result<Self, PlatformError> {
        let session = session.filter(|s| valid_session(s));
        let socket = display.and_then(|display| {
            wayland_socket(
                display.as_bytes(),
                runtime_dir.as_deref().map(str::as_bytes),
            )
        });
        if session.is_none() && socket.is_none() {
            return Err(PlatformError::Unsupported(
                "No login session or display to unlock".into(),
            ));
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        able_to_read_passwords(no_new_privs(&status), Path::new("/etc/shadow"))?;
        let uid = rustix::process::getuid().as_raw();
        let user = account_name(uid)?;
        local_account(
            &std::fs::read("/etc/passwd").unwrap_or_default(),
            &user,
            uid,
        )?;
        let pam = Pam::load()?;
        let pam_files = Arc::new(PamFiles::computers(&pam)?);
        lockout_as_configured(&pam_files)?;
        let scratch = runtime_dir
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(std::env::temp_dir);
        Ok(Self {
            pam: Arc::new(pam),
            stack: Arc::from(stack()),
            pam_files,
            scratch,
            user,
            gate: Arc::new(Mutex::new(())),
            timeout: CHECK_TIMEOUT,
            release: Box::new(SystemRelease {
                session,
                socket,
                uid,
            }),
            #[cfg(test)]
            after_answer: None,
        })
    }
}
#[async_trait]
impl SessionUnlocker for PasswordUnlocker {
    async fn unlock(
        &self,
        password: &Secret,
        allowed: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<UnlockOutcome, PlatformError> {
        let typed = password.expose().as_bytes();
        if typed.contains(&0) {
            return Ok(UnlockOutcome::Wrong);
        }
        // Sized up front so the copy is never moved, leaving one behind.
        let mut copy = Zeroizing::new(Vec::with_capacity(typed.len() + 1));
        copy.extend_from_slice(typed);
        copy.push(0);
        let (pam, stack, pam_files, scratch, user, gate) = (
            self.pam.clone(),
            self.stack.clone(),
            self.pam_files.clone(),
            self.scratch.clone(),
            self.user.clone(),
            self.gate.clone(),
        );
        let (answer, checked) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        let after_answer = self.after_answer.clone();
        let check = move || {
            let password = CStr::from_bytes_with_nul(&copy).map_err(|_| unavailable())?;
            // Again for each check, since a stack may have changed since.
            lockout_as_configured(&pam_files)?;
            // Written for this check alone and removed after it, so there is
            // none lying about between checks to be changed or cleared away.
            let (_dir, confdir) = write_stack(&scratch, &stack)?;
            check(&pam, &confdir, &user, password)
        };
        // A thread of its own rather than the runtime's blocking pool, so a
        // module that never returns can't keep Ophio from stopping. It keeps
        // the gate through checking and password cleanup, even if its
        // caller has already timed out, so no check starts beside it.
        std::thread::Builder::new()
            .name("ophio-pam".into())
            .spawn(move || {
                let turn = match gate.try_lock() {
                    Ok(turn) => turn,
                    Err(TryLockError::Poisoned(turn)) => turn.into_inner(),
                    Err(TryLockError::WouldBlock) => {
                        let _ = answer.send(Err(PlatformError::TemporarilyUnavailable(
                            "The computer is still checking the last password".into(),
                        )));
                        return;
                    }
                };
                let checked = check();
                // Wipes the copy of the password now rather than as the
                // thread ends.
                drop(check);
                // Receiving the answer permits an immediate retry; let it
                // acquire the gate even before this worker exits.
                drop(turn);
                let _ = answer.send(checked);
                #[cfg(test)]
                if let Some(after_answer) = after_answer {
                    after_answer.wait();
                    after_answer.wait();
                }
            })
            .map_err(|_| unavailable())?;
        let checked = tokio::time::timeout(self.timeout, checked)
            .await
            .map_err(|_| {
                PlatformError::TemporarilyUnavailable(
                    "The computer took too long to check the password".into(),
                )
            })?
            .map_err(|_| unavailable())??;
        Ok(match checked {
            // Asked once the password is found right, since whoever gave it
            // may have lost the right to unlock while it was checked.
            Checked::Right if !allowed() => UnlockOutcome::Withheld,
            Checked::Right => {
                self.release.release().await;
                UnlockOutcome::Released
            }
            Checked::Wrong => UnlockOutcome::Wrong,
            Checked::Refused => UnlockOutcome::Refused,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const RIGHT: &str = "correct horse battery";
    /// Locks the account after three wrong passwords, as many computers do.
    const LOCKS: &str = "deny=3 unlock_time=600 fail_interval=900";

    /// Whoever gave the password may still unlock the computer.
    fn allowed() -> bool {
        true
    }

    #[derive(Debug, Default)]
    struct Counted(Arc<AtomicU32>);
    #[async_trait]
    impl Release for Counted {
        async fn release(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Ophio's stack, with pam_exec checking against `RIGHT` where pam_unix
    /// would check the account's password and a lockout tally of its own: no
    /// test asks for the account's real password, nor reads or counts toward
    /// its real lockout.
    struct Stacks {
        dir: tempfile::TempDir,
        user: CString,
        released: Arc<AtomicU32>,
    }
    impl Stacks {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("tally")).unwrap();
            let ran = dir.path().join("ran");
            // Read by sh rather than run itself, so no test's fork can be
            // holding it open for writing when it starts.
            for (name, wait) in [("check", ""), ("slow", "sleep 1\n")] {
                std::fs::write(
                    dir.path().join(name),
                    format!(
                        "PATH=/usr/bin:/bin\necho >> '{}'\n{wait}pw=$(tr -d '\\000')\n[ \"$pw\" = '{RIGHT}' ]\n",
                        ran.display()
                    ),
                )
                .unwrap();
            }
            Self {
                dir,
                user: account_name(rustix::process::getuid().as_raw()).unwrap(),
                released: Arc::default(),
            }
        }
        /// pam_exec running `script`, given `options`.
        fn exec(&self, options: &str, script: &str) -> String {
            format!(
                "pam_exec.so expose_authtok {options} /bin/sh {}",
                self.dir.path().join(script).display()
            )
        }
        /// Ophio's stack with `module` checking the password, and
        /// pam_faillock, given `lockout`, keeping its tally here. pam_exec
        /// fails as no other module does, so its answer is passed on through
        /// pam_deny and pam_permit, as pam_unix gives it.
        fn ophio(&self, module: &str, lockout: &str) -> String {
            format!(
                "{LOCKOUT} dir={} {lockout}\nauth [success=1 default=ignore] {module}\nauth requisite pam_deny.so\nauth required pam_permit.so\n",
                self.dir.path().join("tally").display(),
            )
        }
        fn checking(&self, script: &str) -> PasswordUnlocker {
            self.unlocker(self.ophio(&self.exec("quiet", script), LOCKS))
        }
        fn unlocker(&self, stack: String) -> PasswordUnlocker {
            PasswordUnlocker {
                pam: Arc::new(Pam::load().unwrap()),
                stack: Arc::from(stack),
                pam_files: Arc::new(PamFiles {
                    config: vec![self.dir.path().join("pam.d")],
                    module_roots: vec![self.dir.path().join("lib")],
                }),
                scratch: self.dir.path().to_owned(),
                user: self.user.clone(),
                gate: Arc::new(Mutex::new(())),
                timeout: CHECK_TIMEOUT,
                release: Box::new(Counted(self.released.clone())),
                after_answer: None,
            }
        }
        fn released(&self) -> u32 {
            self.released.load(Ordering::SeqCst)
        }
        /// How many times a password reached the check.
        fn ran(&self) -> usize {
            std::fs::read(self.dir.path().join("ran")).map_or(0, |ran| ran.len())
        }
        fn tally(&self) -> PathBuf {
            self.dir
                .path()
                .join("tally")
                .join(OsStr::from_bytes(self.user.to_bytes()))
        }
        /// Writes a tally of `count` wrong passwords, all just now, as
        /// pam_faillock keeps it.
        fn failures(&self, count: usize) -> Vec<u8> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let mut tally = Vec::new();
            for _ in 0..count {
                let mut record = [0_u8; 64];
                record[..3].copy_from_slice(b"tty");
                record[54..56].copy_from_slice(&1_u16.to_ne_bytes());
                record[56..].copy_from_slice(&now.to_ne_bytes());
                tally.extend_from_slice(&record);
            }
            std::fs::write(self.tally(), &tally).unwrap();
            tally
        }
        /// Whether a check left its stack behind.
        fn left_behind(&self) -> bool {
            std::fs::read_dir(self.dir.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().as_bytes().starts_with(b"ophio-unlock-"))
        }
    }

    #[tokio::test]
    async fn only_the_right_password_releases_the_lock() {
        let stacks = Stacks::new();
        let unlocker = stacks.checking("check");
        assert_eq!(
            unlocker
                .unlock(&Secret::new("battery horse"), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Wrong
        );
        assert_eq!(
            unlocker
                .unlock(&Secret::new(format!("{RIGHT}\0")), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Wrong
        );
        assert_eq!(stacks.released(), 0);
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
        assert_eq!(stacks.released(), 1);
        // One cut short at a NUL never reached it.
        assert_eq!(stacks.ran(), 2);
        // Ophio's tries never count toward the computer's lockout, and no
        // check leaves its stack behind.
        assert!(!stacks.tally().exists());
        assert!(!stacks.left_behind());
    }

    /// Receiving a completed check permits a retry even while the worker
    /// that delivered it is still alive.
    #[tokio::test]
    async fn a_completed_check_accepts_a_retry_before_its_worker_exits() {
        for (password, outcome, releases) in [
            ("battery horse", UnlockOutcome::Wrong, 1),
            (RIGHT, UnlockOutcome::Released, 2),
        ] {
            let stacks = Stacks::new();
            let mut unlocker = stacks.checking("check");
            let after_answer = Arc::new(std::sync::Barrier::new(2));
            unlocker.after_answer = Some(after_answer.clone());
            let first = unlocker.unlock(&Secret::new(password), &allowed).await;
            // The first worker has sent its answer and cannot exit until
            // the second rendezvous, after the retry below.
            after_answer.wait();
            unlocker.after_answer = None;
            let retry = unlocker.unlock(&Secret::new(RIGHT), &allowed).await;
            after_answer.wait();

            assert_eq!(first.unwrap(), outcome);
            assert_eq!(retry.unwrap(), UnlockOutcome::Released);
            assert_eq!(stacks.ran(), 2);
            assert_eq!(stacks.released(), releases);
            assert!(!stacks.left_behind());
        }
    }

    /// Whether whoever gave it may still unlock the computer is asked only
    /// once the password is found right, and if not, nothing is released.
    #[tokio::test]
    async fn a_right_password_releases_nothing_once_its_giver_may_not_unlock() {
        let stacks = Stacks::new();
        let unlocker = stacks.checking("check");
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let ran = std::sync::atomic::AtomicUsize::new(0);
        let withdrawn = || {
            asked.fetch_add(1, Ordering::SeqCst);
            ran.store(stacks.ran(), Ordering::SeqCst);
            false
        };
        assert_eq!(
            unlocker
                .unlock(&Secret::new("battery horse"), &withdrawn)
                .await
                .unwrap(),
            UnlockOutcome::Wrong
        );
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &withdrawn)
                .await
                .unwrap(),
            UnlockOutcome::Withheld
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        assert_eq!(stacks.released(), 0);
    }

    /// Ophio's lockout follows the computer's only where every stack leaves
    /// pam_faillock's settings to faillock.conf and keeps no other lockout.
    #[test]
    fn the_computers_lockout_is_followed_only_where_faillock_conf_sets_it() {
        let dir = tempfile::tempdir().unwrap();
        let (pam_d, pam_conf) = (dir.path().join("pam.d"), dir.path().join("pam.conf"));
        let files = PamFiles {
            config: vec![pam_d.clone(), pam_conf.clone()],
            module_roots: vec![dir.path().join("lib")],
        };
        let followed = || lockout_as_configured(&files).is_ok();
        // None at all.
        assert!(followed());
        std::fs::create_dir(&pam_d).unwrap();
        // This computer's, word for word.
        let arch = "#%PAM-1.0\n\nauth       required                    pam_faillock.so      preauth\n\
            -auth      [success=2 default=ignore]  pam_systemd_home.so\n\
            auth       [success=1 default=bad]     pam_unix.so          try_first_pass nullok\n\
            auth       [default=die]               pam_faillock.so      authfail\n\
            auth       optional                    pam_permit.so\n\
            auth       required                    pam_env.so\n\
            auth       required                    pam_faillock.so      authsucc\n\
            account    required                    pam_faillock.so\n";
        std::fs::write(pam_d.join("system-auth"), arch).unwrap();
        std::fs::write(
            pam_d.join("login"),
            "auth requisite pam_faillock.so preauth silent audit # deny=1\n\
             auth [default=die] /usr/lib/security/pam_faillock.so authfail \\\n  no_log_info\n",
        )
        .unwrap();
        assert!(followed());
        let outside = dir.path().join("outside");
        for (file, text) in [
            (
                "login",
                "auth required pam_faillock.so preauth deny=1 unlock_time=0\n".to_owned(),
            ),
            (
                "login",
                "auth required pam_faillock.so preauth [dir=/var/lib/my tally]\n".into(),
            ),
            (
                "login",
                "auth required pam_faillock.so preauth \\\n even_deny_root\n".into(),
            ),
            (
                "login",
                "AUTH REQUIRED /lib/security/pam_faillock.so preauth unlock_time=0\n".into(),
            ),
            ("login", "-auth required pam_tally2.so deny=3\n".into()),
            ("login", format!("auth substack {}\n", outside.display())),
            ("login", format!("@include {}\n", outside.display())),
            (
                "pam.conf",
                "login auth required pam_faillock.so preauth conf=/etc/other.conf\n".into(),
            ),
        ] {
            let path = if file == "pam.conf" {
                pam_conf.clone()
            } else {
                pam_d.join(file)
            };
            std::fs::write(&outside, "auth required pam_faillock.so authfail deny=1\n").unwrap();
            std::fs::write(&path, &text).unwrap();
            assert!(
                matches!(
                    lockout_as_configured(&files),
                    Err(PlatformError::Unsupported(_))
                ),
                "{text}"
            );
            std::fs::remove_file(&path).unwrap();
        }
        assert!(followed());
        // A blank line ends a line going on, and a `#` stops one going on.
        std::fs::write(
            pam_d.join("login"),
            "auth required pam_faillock.so preauth \\\n\ndeny=1\n\
             auth required pam_faillock.so authfail # \\\nunlock_time=0\n",
        )
        .unwrap();
        assert!(followed());
        // A stack Ophio can't read may lock the account.
        if rustix::process::geteuid().as_raw() != 0 {
            let hidden = pam_d.join("hidden");
            std::fs::write(&hidden, "").unwrap();
            std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(!followed());
        }
    }

    /// Linux-PAM reads a stack included without a path from its directories,
    /// and loads a module named without one from its own, so a lockout is
    /// found wherever a stack is included from and whatever its module is
    /// called.
    #[test]
    fn a_lockout_is_found_however_its_stack_or_module_is_named() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let (pam_d, lib) = (dir.path().join("pam.d"), dir.path().join("lib"));
        let (modules, debian) = (
            lib.join("security"),
            lib.join("x86_64-linux-gnu").join("security"),
        );
        for made in [&pam_d.join("policies"), &modules, &debian] {
            std::fs::create_dir_all(made).unwrap();
        }
        let files = PamFiles {
            config: vec![pam_d.clone()],
            module_roots: vec![lib.clone()],
        };
        let followed = || lockout_as_configured(&files).is_ok();
        // Of the same size, only one of them a lockout.
        std::fs::write(modules.join("pam_faillock.so"), "faillock module").unwrap();
        std::fs::write(modules.join("pam_unix.so"), "password module").unwrap();
        std::fs::write(debian.join("pam_tally2.so"), "tally module").unwrap();
        symlink(modules.join("pam_faillock.so"), dir.path().join("alias.so")).unwrap();
        symlink("pam_faillock.so", modules.join("pam_lockout.so")).unwrap();
        std::fs::hard_link(modules.join("pam_faillock.so"), modules.join("pam_hard.so")).unwrap();
        std::fs::write(dir.path().join("copy.so"), "faillock module").unwrap();
        symlink("pam_tally2.so", debian.join("pam_count.so")).unwrap();
        let (login, policy) = (pam_d.join("login"), pam_d.join("policies").join("lockout"));
        let outside = dir.path().join("outside");
        let alias = dir.path().join("alias.so");
        let copy = dir.path().join("copy.so");
        // Linked to from outside every module directory.
        let elsewhere = dir.path().join("elsewhere.so");
        std::fs::create_dir(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("other/pam_faillock.so"), "built apart").unwrap();
        symlink(dir.path().join("other/pam_faillock.so"), &elsewhere).unwrap();
        // Leaving its settings to faillock.conf, by any name.
        for text in [
            "auth include policies/lockout\n".to_owned(),
            format!("auth required {} preauth\n", alias.display()),
            "auth required pam_lockout.so authfail\nauth required pam_unix.so deny=1\n".into(),
        ] {
            std::fs::write(&policy, "auth required pam_faillock.so preauth\n").unwrap();
            std::fs::write(&login, &text).unwrap();
            assert!(followed(), "{text}");
        }
        for (text, included) in [
            ("auth include policies/lockout\n".to_owned(), &policy),
            ("auth substack ../outside\n".into(), &outside),
            ("@include policies/../policies/lockout\n".into(), &policy),
            (
                format!("auth required {} preauth deny=1\n", alias.display()),
                &policy,
            ),
            (
                "auth required pam_lockout.so unlock_time=0\n".into(),
                &policy,
            ),
            (
                "auth required pam_hard.so preauth even_deny_root\n".into(),
                &policy,
            ),
            (
                format!("auth required {} authfail deny=1\n", copy.display()),
                &policy,
            ),
            ("auth required pam_count.so\n".into(), &policy),
            ("auth required $ISA/pam_unix.so\n".into(), &policy),
            (
                "auth required /usr/lib/$ISA/security/pam_faillock.so preauth\n".into(),
                &policy,
            ),
            (
                format!("auth required {} deny=1\n", elsewhere.display()),
                &policy,
            ),
        ] {
            std::fs::write(&policy, "auth required pam_faillock.so preauth\n").unwrap();
            std::fs::write(&outside, "auth required pam_unix.so\n").unwrap();
            std::fs::write(included, "auth required pam_faillock.so preauth deny=1\n").unwrap();
            std::fs::write(&login, &text).unwrap();
            assert!(!followed(), "{text}");
        }
        // Names that aren't UTF-8 are the files Linux-PAM opens by them.
        std::fs::write(&policy, "auth required pam_faillock.so preauth\n").unwrap();
        std::fs::write(
            pam_d.join("policies").join(OsStr::from_bytes(b"lock\xFF")),
            "auth required pam_faillock.so preauth deny=1\n",
        )
        .unwrap();
        symlink(
            "pam_faillock.so",
            modules.join(OsStr::from_bytes(b"pam_\xFF.so")),
        )
        .unwrap();
        for text in [
            &b"auth include policies/lock\xFF\n"[..],
            b"auth required pam_\xFF.so preauth deny=1\n",
        ] {
            std::fs::write(&login, text).unwrap();
            assert!(!followed(), "{}", text.escape_ascii());
        }
        std::fs::write(&login, b"# r\xE9sum\xE9\nauth include policies/lockout\n").unwrap();
        assert!(followed());
        // An included stack, or a module, Ophio can't read may lock it.
        if rustix::process::geteuid().as_raw() != 0 {
            std::fs::write(&policy, "auth required pam_faillock.so preauth\n").unwrap();
            std::fs::write(&login, "auth include policies/lockout\n").unwrap();
            assert!(followed());
            std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(!followed());
            std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
            let hidden = dir.path().join("hidden");
            std::fs::create_dir(&hidden).unwrap();
            std::fs::write(
                &login,
                format!("auth required {}\n", hidden.join("module.so").display()),
            )
            .unwrap();
            std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(!followed());
            std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o700)).unwrap();
            // Of a size no lockout has, but unread all the same.
            let sealed = hidden.join("sealed.so");
            std::fs::write(&sealed, "a module of some other length").unwrap();
            std::fs::write(&login, format!("auth required {}\n", sealed.display())).unwrap();
            assert!(followed());
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert!(!followed());
        }
    }

    #[test]
    fn pams_places_are_those_its_library_was_built_with() {
        // Linux-PAM built for `/usr/local`, as its library names them.
        let built = PamFiles::built_into(
            b"pam_start\0/usr/local/etc/pam.d/%s\0/usr/local/lib/pam.d/%s\0\
              /usr/local/etc/pam.conf\0/usr/local/lib/security/\0../../lib/security\0%s%s\0",
        )
        .unwrap();
        assert_eq!(
            built.config,
            [
                PathBuf::from("/usr/local/etc/pam.d"),
                "/usr/local/lib/pam.d".into(),
                "/usr/local/etc/pam.conf".into(),
            ]
        );
        assert_eq!(built.module_roots, [PathBuf::from("/usr/local/lib/")]);
        // Without its stacks' directory or its modules' Ophio can't know
        // what it reads.
        assert!(PamFiles::built_into(b"/etc/pam.conf\0/usr/lib/security/\0").is_none());
        assert!(PamFiles::built_into(b"/etc/pam.d/%s\0../../lib/security\0").is_none());
        // Read besides the usual ones, and only where the loaded library
        // names its own.
        let custom = b"/usr/local/etc/pam.d/%s\0/usr/local/lib/security/\0";
        let both = PamFiles::usual_and(custom).unwrap();
        for place in ["/usr/local/etc/pam.d", "/etc/pam.d", "/etc/pam.conf"] {
            assert!(both.config.contains(&place.into()), "{place}");
        }
        for root in ["/usr/local/lib", "/usr/lib", "/lib64"] {
            assert!(both.module_roots.contains(&root.into()), "{root}");
        }
        assert!(PamFiles::usual_and(&[]).is_none());
        assert!(PamFiles::usual_and(b"/etc/pam.conf\0").is_none());

        // A lockout set only where such a build looks is found there.
        let dir = tempfile::tempdir().unwrap();
        let (pam_d, modules) = (
            dir.path().join("etc/pam.d"),
            dir.path().join("lib/security"),
        );
        std::fs::create_dir_all(&pam_d).unwrap();
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("pam_faillock.so"), "faillock module").unwrap();
        std::fs::write(modules.join("pam_lock.so"), "faillock module").unwrap();
        let prefix = dir.path().as_os_str().as_bytes();
        let files = PamFiles::built_into(
            &[prefix, b"/etc/pam.d/%s\0", prefix, b"/lib/security/\0"].concat(),
        )
        .unwrap();
        std::fs::write(pam_d.join("login"), "auth required pam_lock.so preauth\n").unwrap();
        assert!(lockout_as_configured(&files).is_ok());
        std::fs::write(pam_d.join("login"), "auth required pam_lock.so deny=1\n").unwrap();
        assert!(lockout_as_configured(&files).is_err());
    }

    #[test]
    fn pams_places_are_read_from_the_object_that_supplies_its_function() {
        let pam = Pam::load().unwrap();
        let library = loaded_object(pam.start_confdir as usize).unwrap();
        let built = PamFiles::built_into(&library).unwrap();
        assert!(built.config.iter().any(|place| place.is_dir()));
        assert!(
            built
                .module_roots
                .iter()
                .any(|root| root.join("security").is_dir())
        );
        assert!(PamFiles::computers(&pam).is_ok());
        assert!(loaded_object(0).is_none());
        assert!(loaded_object(usize::MAX).is_none());

        // Load a private copy with a different compiled stack directory,
        // so selecting some other loaded libpam cannot pass this test.
        // SAFETY: dladdr fills this plain output struct; the library owning
        // the address and the returned name stays loaded through `pam`.
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { libc::dladdr(pam.start_confdir as *const c_void, &mut info) },
            0
        );
        assert!(!info.dli_fname.is_null());
        // SAFETY: dladdr returned a non-null, NUL-terminated library name.
        let path = unsafe { CStr::from_ptr(info.dli_fname) };
        let mut file = std::fs::read(Path::new(OsStr::from_bytes(path.to_bytes()))).unwrap();
        let original = built
            .config
            .iter()
            .find(|path| path.ends_with("pam.d"))
            .unwrap();
        let pattern = [original.as_os_str().as_bytes(), b"/%s\0"].concat();
        let at = file
            .windows(pattern.len())
            .position(|bytes| bytes == pattern)
            .unwrap();
        // Only change a prefix character, never the size or the /pam.d/%s
        // suffix. No authentication is run against this copied library.
        let mut changed = pattern.clone();
        changed[1] = if changed[1] == b'x' { b'y' } else { b'x' };
        file[at..at + pattern.len()].copy_from_slice(&changed);
        let changed_dir = PathBuf::from(OsStr::from_bytes(&changed[..changed.len() - 4]));
        let dir = tempfile::tempdir().unwrap();
        // An arbitrary name must work; only the function's object matters.
        let copy = dir.path().join("private-pam.so");
        std::fs::write(&copy, file).unwrap();
        // SAFETY: a copy of the system library, with only a path constant
        // changed, and the symbol has its type from <security/pam_appl.h>.
        let copied = unsafe { libloading::Library::new(&copy) }.unwrap();
        let start = unsafe {
            *copied
                .get::<StartConfdirFn>(b"pam_start_confdir\0")
                .unwrap()
        };
        assert_ne!(start as usize, pam.start_confdir as usize);
        let loaded = loaded_object(start as usize).unwrap();
        let copied_places = PamFiles::built_into(&loaded).unwrap();
        assert!(copied_places.config.contains(&changed_dir));
        assert!(!built.config.contains(&changed_dir));

        // Replacing the pathname cannot change which places Ophio reads.
        // Rename, never truncate a live mapping's backing file.
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"/elsewhere/pam.d/%s\0/elsewhere/security/\0").unwrap();
        std::fs::rename(&replacement, &copy).unwrap();
        assert_eq!(loaded_object(start as usize).unwrap(), loaded);
        std::fs::remove_file(&copy).unwrap();
        assert_eq!(loaded_object(start as usize).unwrap(), loaded);
        // Keep both libraries alive through their last inspection; no
        // authentication or real lock release was called here.
        drop(copied);
    }

    #[tokio::test]
    async fn a_lockout_set_in_the_computers_stacks_leaves_the_password_unchecked() {
        let stacks = Stacks::new();
        let unlocker = stacks.checking("check");
        std::fs::create_dir(stacks.dir.path().join("pam.d")).unwrap();
        std::fs::write(
            stacks.dir.path().join("pam.d").join("login"),
            "auth required pam_faillock.so preauth deny=1\n",
        )
        .unwrap();
        assert!(matches!(
            unlocker.unlock(&Secret::new(RIGHT), &allowed).await,
            Err(PlatformError::Unsupported(_))
        ));
        assert_eq!(stacks.ran(), 0);
        assert_eq!(stacks.released(), 0);
        assert!(!stacks.left_behind());
    }

    #[tokio::test]
    async fn a_stack_that_explains_itself_is_refusing() {
        let stacks = Stacks::new();
        let unlocker = stacks.unlocker(stacks.ophio(&stacks.exec("", "check"), LOCKS));
        assert_eq!(
            unlocker
                .unlock(&Secret::new("battery horse"), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Refused
        );
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
    }

    #[tokio::test]
    async fn a_locked_account_is_refused_before_its_password_is_asked_for() {
        let stacks = Stacks::new();
        for lockout in [LOCKS.to_owned(), format!("{LOCKS} silent")] {
            let unlocker = stacks.unlocker(stacks.ophio(&stacks.exec("quiet", "check"), &lockout));
            let tally = stacks.failures(3);
            for password in [RIGHT, "battery horse"] {
                assert_eq!(
                    unlocker
                        .unlock(&Secret::new(password), &allowed)
                        .await
                        .unwrap(),
                    UnlockOutcome::Refused
                );
            }
            assert_eq!(stacks.ran(), 0);
            assert_eq!(std::fs::read(stacks.tally()).unwrap(), tally);
        }
        // Short of the lockout, the password is checked, and neither a wrong
        // one nor a right one changes the tally.
        let unlocker = stacks.checking("check");
        let tally = stacks.failures(2);
        assert_eq!(
            unlocker
                .unlock(&Secret::new("battery horse"), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Wrong
        );
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
        assert_eq!(std::fs::read(stacks.tally()).unwrap(), tally);
        assert_eq!(stacks.released(), 1);
    }

    #[tokio::test]
    async fn ophios_own_stack_passes_a_right_password_and_stops_at_a_lockout() {
        let stacks = Stacks::new();
        // As written, with only a right password, which pam_exec can pass.
        let stack = stack()
            .replace(
                "pam_faillock.so preauth",
                &format!(
                    "pam_faillock.so preauth dir={} {LOCKS}",
                    stacks.dir.path().join("tally").display()
                ),
            )
            .replace("pam_unix.so", &stacks.exec("quiet", "check"));
        let unlocker = stacks.unlocker(stack);
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
        stacks.failures(3);
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Refused
        );
        assert_eq!(stacks.ran(), 1);
    }

    #[tokio::test]
    async fn a_computer_without_a_lockout_still_has_the_password_checked() {
        let stacks = Stacks::new();
        let unlocker = stacks.unlocker(
            stacks
                .ophio(&stacks.exec("quiet", "check"), LOCKS)
                .replace("pam_faillock.so", "pam_ophio_no_such_module.so"),
        );
        assert_eq!(
            unlocker
                .unlock(&Secret::new("battery horse"), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Wrong
        );
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
    }

    #[tokio::test]
    async fn a_check_that_asks_for_no_password_or_cannot_run_unlocks_nothing() {
        let stacks = Stacks::new();
        for stack in [
            "auth required pam_permit.so\n",
            "auth required pam_ophio_no_such_module.so\n",
        ] {
            assert!(matches!(
                stacks
                    .unlocker(stack.into())
                    .unlock(&Secret::new(RIGHT), &allowed)
                    .await,
                Err(PlatformError::TemporarilyUnavailable(_))
            ));
        }
        // Nowhere to write the stack.
        let mut unlocker = stacks.checking("check");
        unlocker.scratch = stacks.dir.path().join("missing");
        assert!(matches!(
            unlocker.unlock(&Secret::new(RIGHT), &allowed).await,
            Err(PlatformError::TemporarilyUnavailable(_))
        ));
        assert_eq!(stacks.released(), 0);
        assert_eq!(stacks.ran(), 0);
    }

    // Holding the gate across the attempt is what this tests.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn one_check_runs_at_a_time() {
        let stacks = Stacks::new();
        let unlocker = stacks.checking("check");
        let _held = unlocker.gate.lock().unwrap();
        assert!(matches!(
            unlocker.unlock(&Secret::new(RIGHT), &allowed).await,
            Err(PlatformError::TemporarilyUnavailable(_))
        ));
        assert_eq!(stacks.released(), 0);
        assert_eq!(stacks.ran(), 0);
    }

    /// Waits for the check given up on to end, once it holds the gate.
    async fn settled(unlocker: &PasswordUnlocker) {
        assert!(matches!(
            unlocker.unlock(&Secret::new(RIGHT), &allowed).await,
            Err(PlatformError::TemporarilyUnavailable(m)) if m.contains("still checking")
        ));
        for _ in 0..200 {
            if unlocker.gate.try_lock().is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the check never ended");
    }

    #[tokio::test]
    async fn a_check_given_up_on_unlocks_nothing_and_holds_the_next_until_it_ends() {
        let stacks = Stacks::new();
        let mut unlocker = stacks.checking("slow");
        unlocker.timeout = Duration::from_millis(100);
        assert!(matches!(
            unlocker.unlock(&Secret::new(RIGHT), &allowed).await,
            Err(PlatformError::TemporarilyUnavailable(m)) if m.contains("too long")
        ));
        settled(&unlocker).await;
        assert_eq!(stacks.released(), 0);
        assert!(!stacks.left_behind());
        unlocker.timeout = CHECK_TIMEOUT;
        assert_eq!(
            unlocker
                .unlock(&Secret::new(RIGHT), &allowed)
                .await
                .unwrap(),
            UnlockOutcome::Released
        );
    }

    fn converse(styles: &[c_int], state: &mut ConvState) -> (c_int, Vec<Option<String>>) {
        let texts: Vec<Message> = styles
            .iter()
            .map(|&style| Message {
                style,
                text: c"Password: ".as_ptr(),
            })
            .collect();
        let pointers: Vec<*const Message> = texts.iter().map(|m| m as *const Message).collect();
        let mut replies: *mut Response = ptr::null_mut();
        // SAFETY: well-formed messages and a live state, as PAM would pass.
        let status = unsafe {
            conversation(
                styles.len() as c_int,
                pointers.as_ptr(),
                &mut replies,
                (state as *mut ConvState).cast(),
            )
        };
        let mut answers = Vec::new();
        if !replies.is_null() {
            for i in 0..styles.len() {
                // SAFETY: the conversation calloc'd `styles.len()` answers.
                let text = unsafe { (*replies.add(i)).resp };
                answers.push((!text.is_null()).then(|| {
                    // SAFETY: a strdup'd string, freed here as PAM would.
                    let answer = unsafe { CStr::from_ptr(text) }.to_str().unwrap().to_owned();
                    unsafe { libc::free(text.cast()) };
                    answer
                }));
            }
            // SAFETY: as above.
            unsafe { libc::free(replies.cast()) };
        }
        (status, answers)
    }

    fn state(password: &CStr) -> ConvState {
        ConvState {
            password: password.as_ptr(),
            prompts: 0,
            told_error: false,
            asked_other: false,
        }
    }

    #[test]
    fn the_conversation_gives_the_password_only_where_it_is_asked_for() {
        let password = c"hunter2";
        let mut seen = state(password);
        let (status, answers) = converse(
            &[PAM_TEXT_INFO, PAM_PROMPT_ECHO_OFF, PAM_ERROR_MSG],
            &mut seen,
        );
        assert_eq!(status, PAM_SUCCESS);
        assert_eq!(answers, vec![None, Some("hunter2".to_owned()), None]);
        assert_eq!(
            (seen.prompts, seen.told_error, seen.asked_other),
            (1, true, false)
        );

        // A visible prompt asks for something other than the password.
        let mut seen = state(password);
        let (status, answers) = converse(&[PAM_PROMPT_ECHO_OFF, 2], &mut seen);
        assert_eq!(status, PAM_CONV_ERR);
        assert!(answers.is_empty());
        assert!(seen.asked_other);

        // So does asking for a second hidden answer, such as a code; the
        // password is not given in its place.
        let mut seen = state(password);
        let (status, answers) = converse(&[PAM_PROMPT_ECHO_OFF; 2], &mut seen);
        assert_eq!(status, PAM_CONV_ERR);
        assert!(answers.is_empty());
        assert_eq!(seen.prompts, 1);
        assert!(seen.asked_other);
        let mut seen = state(password);
        assert_eq!(converse(&[PAM_PROMPT_ECHO_OFF], &mut seen).0, PAM_SUCCESS);
        let (status, answers) = converse(&[PAM_PROMPT_ECHO_OFF], &mut seen);
        assert_eq!(status, PAM_CONV_ERR);
        assert!(answers.is_empty());
        assert!(seen.asked_other);

        let mut seen = state(password);
        assert_eq!(converse(&[], &mut seen).0, PAM_CONV_ERR);
        assert_eq!(converse(&[PAM_TEXT_INFO; 33], &mut seen).0, PAM_CONV_ERR);
        assert_eq!(converse(&[PAM_TEXT_INFO; 32], &mut seen).0, PAM_SUCCESS);
    }

    #[test]
    fn pam_answers_are_read_without_its_words() {
        let password = c"x";
        let mut asked = state(password);
        asked.prompts = 1;
        let quiet = state(password);
        assert_eq!(verdict(PAM_SUCCESS, &asked).unwrap(), Checked::Right);
        assert!(matches!(
            verdict(PAM_SUCCESS, &quiet),
            Err(PlatformError::TemporarilyUnavailable(_))
        ));
        assert_eq!(verdict(PAM_AUTH_ERR, &asked).unwrap(), Checked::Wrong);
        // Turned away before the password was asked for.
        assert_eq!(verdict(PAM_AUTH_ERR, &quiet).unwrap(), Checked::Refused);
        assert_eq!(verdict(PAM_MAXTRIES, &asked).unwrap(), Checked::Refused);
        assert_eq!(verdict(PAM_PERM_DENIED, &asked).unwrap(), Checked::Refused);
        let mut told = state(password);
        told.prompts = 1;
        told.told_error = true;
        assert_eq!(verdict(PAM_AUTH_ERR, &told).unwrap(), Checked::Refused);
        let mut other = state(password);
        other.asked_other = true;
        other.told_error = true;
        assert!(matches!(
            verdict(PAM_CONV_ERR, &other),
            Err(PlatformError::Unsupported(_))
        ));
        // A stack that goes on without the answer it asked for is still
        // asking for more than the password.
        other.prompts = 1;
        assert!(matches!(
            verdict(PAM_SUCCESS, &other),
            Err(PlatformError::Unsupported(_))
        ));
        assert!(matches!(
            verdict(4, &asked),
            Err(PlatformError::TemporarilyUnavailable(_))
        ));
    }

    #[test]
    fn only_an_account_the_computer_keeps_itself_is_checked() {
        let passwd = b"root:x:0:0::/root:/bin/sh\nme:x:1000:1000::/srv/me:/bin/sh\nyou:x:10000:10000::/srv/you:/bin/sh";
        local_account(passwd, c"me", 1000).unwrap();
        local_account(passwd, c"you", 10000).unwrap();
        for (user, uid) in [
            (c"me", 1001),
            (c"me", 100),
            (c"m", 1000),
            (c"you", 1000),
            (c"ghost", 1000),
        ] {
            assert!(matches!(
                local_account(passwd, user, uid),
                Err(PlatformError::Unsupported(_))
            ));
        }
        assert!(local_account(b"", c"me", 1000).is_err());
    }

    #[test]
    fn no_locker_is_signalled_once_the_release_is_given_up_on() {
        use std::os::unix::process::ExitStatusExt;
        // A process of the test's own, made to look like a locker.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(child.id().to_string());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("comm"), "hyprlock\n").unwrap();
        std::fs::write(
            dir.join("environ"),
            b"WAYLAND_DISPLAY=/run/user/7/wayland-1\0",
        )
        .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let uid = rustix::process::getuid().as_raw();
        let socket = Path::new("/run/user/7/wayland-1");
        let passed = Instant::now();
        let signalled = signal_locker(root.path(), pid, uid, socket, passed);
        if signalled {
            let _ = child.kill();
        }
        assert!(!signalled);
        assert!(child.try_wait().unwrap().is_none());
        let signalled = signal_locker(
            root.path(),
            pid,
            uid,
            socket,
            Instant::now() + RELEASE_TIMEOUT,
        );
        if !signalled {
            let _ = child.kill();
        }
        assert!(signalled);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGUSR1));
    }

    #[test]
    fn only_this_accounts_lockers_on_this_display_are_released() {
        let root = tempfile::tempdir().unwrap();
        let process = |pid: &str, name: &str, environ: Option<&[u8]>| {
            let dir = root.path().join(pid);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("comm"), format!("{name}\n")).unwrap();
            if let Some(environ) = environ {
                std::fs::write(dir.join("environ"), environ).unwrap();
            }
        };
        let here: &[u8] = b"XDG_RUNTIME_DIR=/run/user/7\0";
        let on = |display: &[u8]| [here, display].concat();
        process(
            "100",
            "hyprlock",
            Some(&[b"HOME=/h\0", &on(b"WAYLAND_DISPLAY=wayland-1\0")[..]].concat()),
        );
        process(
            "101",
            "swaylock",
            Some(b"WAYLAND_DISPLAY=/run/user/7/wayland-1\0"),
        );
        process("102", "hyprlock", Some(&on(b"WAYLAND_DISPLAY=wayland-0\0")));
        process("103", "Hyprland", Some(&on(b"WAYLAND_DISPLAY=wayland-1\0")));
        process("104", "hyprlock", None);
        process(
            "105",
            "hyprlock",
            Some(&on(b"WAYLAND_DISPLAY=wayland-10\0")),
        );
        // The same name in another session's runtime directory, or in none.
        process(
            "106",
            "hyprlock",
            Some(b"XDG_RUNTIME_DIR=/run/user/8\0WAYLAND_DISPLAY=wayland-1\0"),
        );
        process("107", "hyprlock", Some(b"WAYLAND_DISPLAY=wayland-1\0"));
        process(
            "108",
            "swaylock",
            Some(b"WAYLAND_DISPLAY=wayland-1\0XDG_RUNTIME_DIR=/run/user/7/\0"),
        );
        process(
            "109",
            "hyprlock",
            Some(&on(b"WAYLAND_DISPLAY_WAS=wayland-1\0")),
        );
        process(
            "self",
            "hyprlock",
            Some(&on(b"WAYLAND_DISPLAY=wayland-1\0")),
        );
        let uid = rustix::process::getuid().as_raw();
        let socket = Path::new("/run/user/7/wayland-1");
        assert_eq!(lockers(root.path(), uid, socket), vec![100, 101, 108]);
        assert!(lockers(root.path(), uid.wrapping_add(1), socket).is_empty());
        assert!(lockers(&root.path().join("missing"), uid, socket).is_empty());
    }

    #[test]
    fn a_display_is_its_socket() {
        let socket = |display: &str, runtime_dir: Option<&str>| {
            wayland_socket(display.as_bytes(), runtime_dir.map(str::as_bytes))
        };
        assert_eq!(
            socket("wayland-1", Some("/run/user/7")),
            Some(PathBuf::from("/run/user/7/wayland-1"))
        );
        assert_eq!(socket("/tmp/w", None), Some(PathBuf::from("/tmp/w")));
        assert_eq!(socket("wayland-1", None), None);
        assert_eq!(socket("wayland-1", Some("run/user/7")), None);
        assert_eq!(socket("", Some("/run/user/7")), None);
    }

    #[test]
    fn a_process_that_cannot_read_passwords_checks_none() {
        let dir = tempfile::tempdir().unwrap();
        let readable = dir.path().join("shadow");
        std::fs::write(&readable, "").unwrap();
        let missing = dir.path().join("missing");
        assert!(matches!(
            able_to_read_passwords(true, &missing),
            Err(PlatformError::Unsupported(_))
        ));
        able_to_read_passwords(true, &readable).unwrap();
        able_to_read_passwords(false, &missing).unwrap();
        assert!(no_new_privs("Name:\tophio\nNoNewPrivs:\t1\nSeccomp:\t0\n"));
        assert!(!no_new_privs("Name:\tophio\nNoNewPrivs:\t0\n"));
        assert!(!no_new_privs(""));
    }

    #[test]
    fn nothing_to_release_means_nothing_to_unlock() {
        assert!(matches!(
            PasswordUnlocker::new(None, None, None),
            Err(PlatformError::Unsupported(_))
        ));
        assert!(matches!(
            PasswordUnlocker::new(
                Some("-bad".into()),
                Some(String::new()),
                Some("/run/user/7".into())
            ),
            Err(PlatformError::Unsupported(_))
        ));
        // A display whose socket can't be found.
        assert!(matches!(
            PasswordUnlocker::new(None, Some("wayland-1".into()), None),
            Err(PlatformError::Unsupported(_))
        ));
    }
}
