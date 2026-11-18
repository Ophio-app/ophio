# Install Øphio on Windows

The Windows download is experimental. It is not a beta support promise:
Øphio's beta is for Android phones and Linux computers. The 0.3.0 release
includes an unsigned Windows 11 x86_64 ZIP.
Windows may warn about it or refuse to run it. Read the warning instructions
below before unpacking or starting it. Paid signing is optional, not a
requirement for this experimental download or for personal builds.

This archive is cross-compiled on Linux with LLVM/MinGW. The initial 0.3.0
host-only candidate passed fresh ordinary-user installation and connected
checks on Windows 11 24H2 (build 26100) in KVM with a signed Android 0.2.0
emulator. Physical PCs, hardware GPUs, Windows 10, real-provider turns and a
matching 0.3.0 phone app remain unverified.
See [Windows qualification](../../docs/qualification/windows-macos.md).

## Windows warnings and how to continue

### Check the download first

Download the ZIP and `SHA256SUMS` from the same published release. In
PowerShell, calculate the ZIP's hash and compare it with its entry in
`SHA256SUMS`:

```powershell
Get-FileHash -LiteralPath .\ophio-0.3.0-x86_64-windows.zip -Algorithm SHA256
```

All 64 characters must match, ignoring upper- and lowercase letters. If they
do not, stop and download again. A
matching hash checks the downloaded bytes; it does not give this unsigned
program a verified publisher or Windows reputation.

### "Windows protected your PC" — Microsoft Defender SmartScreen

SmartScreen can show this warning because the program is unsigned and not
widely downloaded. Microsoft's
[SmartScreen guide](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
explains that each unsigned version must build its own reputation. If you
trust the release and have checked its hash:

1. In the **Windows protected your PC** box, choose **More info**.
2. Check that the app is the Øphio program you meant to start, then choose
   **Run anyway**, if that button is offered.
3. Continue the installation below. If Windows blocked a terminal command,
   run that command again after allowing the program.

Some computers do not offer **Run anyway** because of their security policy.
Do not assume another warning is SmartScreen: Smart App Control, described
below, has no per-app **Run anyway** option.

For a verified ZIP you trust, an alternative is to remove its internet
download mark **before extracting it**. Right-click the ZIP in File Explorer,
choose **Properties**, select **Unblock** if present, then **Apply**. The
PowerShell equivalent is:

```powershell
Unblock-File -LiteralPath .\ophio-0.3.0-x86_64-windows.zip
Expand-Archive -LiteralPath .\ophio-0.3.0-x86_64-windows.zip -DestinationPath .\ophio-0.3.0-windows-unpacked
```

Use a new destination folder, not the installed program folder. If you
already extracted a marked ZIP, unpack the cleared ZIP into another new
folder. This only removes the download mark; it does not add a signature,
make the program trusted, or override Smart App Control or an administrator's
application-control policy.

### Smart App Control — blocked apps without a per-app exception

On Windows 11, Smart App Control can refuse an unsigned program when it
cannot establish that it is safe. An earlier unsigned Øphio build working
does not mean the next one will be allowed. Microsoft says there is
currently no way to bypass Smart App Control for an individual app. Neither
**Unblock** nor SmartScreen's **Run anyway** changes that.

To check which protection is involved, open **Start › Windows Security ›
App & browser control › Smart App Control settings** and read its state.
If Smart App Control is **On** and is blocking Øphio, you have a choice:

- **Keep it on.** Do not run this unsigned build on that computer. Use a
  Linux computer or wait for a suitably trusted Windows build instead.
- **Turn it off yourself.** This removes Smart App Control's screening for
  **all apps**, not just Øphio. Other Windows protections are separate, and
  turning this off does not prove that the download is safe. Only do this
  if you accept that loss of protection.

If you choose the second option, return to **Windows Security › App &
browser control › Smart App Control settings**, select **Off**, and confirm
any Windows prompt. Then retry the Øphio command. Do not change a managed
computer's policy yourself; ask its administrator instead. Other
application-control policies can still block the program.

Before choosing **Off**, check whether your Windows installation allows it
to be turned back on. Microsoft's
[Smart App Control FAQ](https://support.microsoft.com/en-us/windows/security/threat-malware-protection/smart-app-control-frequently-asked-questions)
says: "Recent Windows updates allow Smart App Control to be re-enabled
without requiring a clean installation." It does not name a specific update.
On updated, eligible devices, return to the same settings page and choose
**On** when offered. Older Windows installations or device conditions may
still require a Windows reset or reinstall; do not assume this choice is
reversible on your computer. No Øphio installer or qualification check
changes this setting for you.

## Optional release-maintainer signing

Ordinary packaging remains unsigned. Maintainers can separately sign an
already packaged ZIP on Windows with `scripts/sign-windows.py`, using Azure
Artifact Signing. This is optional, incurs Azure usage and requires the account
owner's approval; the script does not create an account, download tools or log
in. Personal builds and the original unsigned ZIP are left unchanged. No live
Azure signing was performed in the candidate checks described here.

Prerequisites are Python 3, the Windows SDK SignTool (10.0.2261.755 or newer),
the matching-architecture Artifact Signing client
`Azure.CodeSigning.Dlib.dll`, .NET 8 and the Visual C++ runtime. The Azure account
needs completed identity validation, a **Public Trust** certificate profile
and the **Artifact Signing Certificate Profile Signer** role for the selected
identity. A PublicTrustTest profile is not for shipping. Use the account's
regional endpoint, not an endpoint copied blindly from an example. Microsoft's
[signing integration guide](https://learn.microsoft.com/azure/artifact-signing/how-to-signing-integrations)
describes these prerequisites.

From the source checkout, with an already authenticated Azure CLI session,
replace the placeholders below with the intended input ZIP, a new output ZIP,
tool paths and the approved Azure account/profile:

```powershell
py -3 scripts/sign-windows.py unsigned.zip --output signed.zip --signtool PATH --dlib PATH --endpoint https://eus.codesigning.azure.net --account ACCOUNT --profile PROFILE
```

The default credential is that existing Azure CLI session, with no interactive
login or fallback. `--credential environment`, `--credential workload-identity`
and `--credential managed-identity` select other explicitly prepared identities.
Never put secrets in command arguments or the source checkout.

The script signs every nested `.exe` and `.dll`, updates the archive's
`SHA256SUMS`, and verifies the final ZIP's executable bytes with Windows trust
checks and a required timestamp before writing the separate output. Signing
changes the artifact hash; calculate and verify the final archive hash before
any approved publication. This does not sign Android or replace other release
signature requirements, and does not establish SmartScreen reputation or
compatibility with every application-control policy.

To verify a signed ZIP without submitting a signing request, run:

```powershell
py -3 scripts/sign-windows.py signed.zip --verify-only --signtool PATH
```

Verification uses Windows trust stores and can access revocation and timestamp
chains. It does not publish, install or qualify the package.

## Fresh installation

Extract the whole ZIP to a new folder you own, outside the installation
destination and outside Øphio's Library. Keep `ophio.exe`, `computer-mcp.exe`
and the other archive contents together, including `ophio.apk`, the phone app,
when present. The installer does not read ZIP files; extract first. Don't
extract into the `Ophio` folder in your user folder or into a project folder
you share: a paired device allowed to change Library files could then change
Øphio's own programs. `.\ophio.exe doctor` warns about that location.
Retain LICENSE, NOTICE, THIRD-PARTY-NOTICES.txt and sbom.cdx.json; the phone
app carries its own notices under Settings › Versions.

In an ordinary, non-administrator PowerShell in the extracted folder, run:

```powershell
.\ophio.exe install
```

This queues an independently copied updater and returns before it finishes.
Run `.\ophio.exe update` from the extracted folder until it reports Installed
before starting setup. This installs for your Windows account in
`%LOCALAPPDATA%\Programs\Ophio`.
The `ophio.exe` at the root of that folder is the stable launcher. The
replaceable programs live in `bin`, application files are separate in `app`,
and `previous` holds the prior version for rollback. Do not manually replace
these folders or extract a new archive over them.

For the commands below, open an ordinary, non-administrator PowerShell in
`%LOCALAPPDATA%\Programs\Ophio`, not in `bin`. Start setup with:

```powershell
.\ophio.exe setup
```

Øphio runs in the background with no window of its own. Opening `ophio.exe`
from File Explorer explains how to set it up and changes nothing.

Setup goes through the rest one step at a time: starting Øphio at every
login, how your phone reaches this computer, Claude Code and Codex,
installing the phone app and pairing it, extras such as desktop access and
voice, and a check that everything works. Any step can be left for later, and
`.\ophio.exe setup` carries on where it was left. Once an agent is connected,
it can do most steps for you as a task you watch and approve; pairing, what a
device may do and signing in stay with you. By hand, the essentials are:

```powershell
.\ophio.exe service install
.\ophio.exe pair
```

`service install` starts Øphio now and whenever you log in. It is a per-user
startup entry, not a Session-0 Windows service, and it shows in
**Settings › Apps › Startup**, where it can be turned off. Closing the
terminal does not stop Øphio. Run from an administrator terminal, it only
registers the entry, because Øphio and the agents it starts would run as
administrator too; it then starts at your next login. `.\ophio.exe service
status` says whether it is registered and running. `.\ophio.exe run` runs
Øphio in the terminal instead, until the terminal is closed.

Unless this computer is on Tailscale, Øphio accepts connections only from this
computer itself. Run Tailscale on this computer and on your phone, or, on a
network you trust, add this computer's local address (from `ipconfig`) to
`%APPDATA%\ophio\config\config.toml`:

```toml
allow_lan = true
listen = ["192.168.1.20"]
```

The first time Øphio listens there, Windows Defender Firewall asks whether to
allow it. Allow private networks only; on Windows 11 the choice of networks
is under **Show more**, and on a standard account allowing needs an
administrator. Windows treats a new network as public until told otherwise, so
set yours to **Private network** under **Settings › Network & internet**, in
that network's properties. If the question was declined or closed, Windows adds
inbound rules that block `ophio.exe`, and a blocking rule wins over any rule that
allows it: an administrator first deletes the `ophio.exe` rules marked with a
block sign under **Windows Defender Firewall with Advanced Security › Inbound
Rules**, then allows `ophio.exe` under **Windows Security › Firewall & network
protection › Allow an app through firewall**. Restart Øphio after any of these
changes: `.\ophio.exe service uninstall`, then `.\ophio.exe service install`.
If a pairing code offers only this computer's own address, `.\ophio.exe pair`
says so.

When the phone asks to pair, answer `y` only if it shows the same code as the
terminal; without a terminal, confirm with `.\ophio.exe pair confirm <code>`.
`.\ophio.exe doctor` checks the setup. Watching and controlling the desktop is
off until `.\ophio.exe desktop enable`. WGC monitor capture with DXGI fallback
and SendInput need a logged-in interactive session. Elevated apps, the UAC
secure desktop and locked sessions are outside managed input access.

If the earlier `companion.exe` is on this computer, stop it before running any
`ophio.exe` command: close its window, or end it in Task Manager. Øphio's
commands cannot reach the earlier program while it runs. Once it has stopped,
Øphio moves its settings, identity, paired devices and history to Øphio's own
folders the first time it starts.

## Old flat ZIP installations

An old installation with `ophio.exe` directly in
`%LOCALAPPDATA%\Programs\Ophio`, rather than a managed `bin` directory, can
have inherited permissions that the new installer intentionally refuses. A
normal upgrade from that layout is not qualified. Do not recursively change
permissions to force it through.

The following clean-install path passed a dedicated Windows 11 lab check,
without changing the old folder's permissions. Identity, settings, a
registered project, its files and a revoked pairing record were retained;
this was not a test of live conversation history from an actively paired
phone. It is only for that old flat layout, not a managed installation or
an interrupted update with recovery files; contact support for those cases.
Finish active work first. In an ordinary, logged-in, non-administrator
PowerShell in the **newly extracted package**, run these commands one at a
time. Do not continue to the next command if one fails:

```powershell
.\ophio.exe service uninstall
if ($LASTEXITCODE -ne 0) { throw 'Stop failed. Keep the old installation and contact support.' }
Rename-Item -LiteralPath "$env:LOCALAPPDATA\Programs\Ophio" -NewName ('Ophio-backup-' + [guid]::NewGuid().ToString('N')) -ErrorAction Stop
.\ophio.exe install
```

Stop if any command fails. The rename moves **only the program folder** to a
unique sibling backup outside the canonical `Ophio` root. Keep that backup.
Do not move or delete application configuration, data or project folders, and
do not run `uninstall-data`. Wait for `update` to report Installed, then run
`setup` from the installed root launcher as described above. Pairing identity,
configuration and projects are separate from the renamed program folder.

## Updating and recovery

For a new extracted package, use the verification and extraction precautions
above. Extract it into a new folder outside the installation destination.
In an ordinary, non-administrator PowerShell in
`%LOCALAPPDATA%\Programs\Ophio`, queue it with:

```powershell
.\ophio.exe update "C:\path\to\extracted-directory"
```

The updater waits until Øphio has been idle for one minute before replacing
the installed version. To explicitly stop Øphio and update without waiting
for that idle period, use:

```powershell
.\ophio.exe update "C:\path\to\extracted-directory" --now
```

Once an update is queued, use the stable root launcher to check its status,
cancel it before changes begin, or roll back to the retained prior version:

```powershell
.\ophio.exe update
.\ophio.exe update --cancel
.\ophio.exe update --rollback
```

Cancellation is honored before replacement of the installed programs begins;
it is not a way to undo a replacement already in progress. During replacement,
the root launcher can ask you to retry instead of showing status; the extracted
copy can still show status. Use rollback once recovery has finished. Setup lets
you choose whether Øphio starts at login. Updates do not enable or disable
that choice, and do not rewrite a startup registration belonging to another
copy.

If Windows keeps an executable locked and replacement fails, the updater
leaves its recovery files in place. On the next start, the stable root
launcher uses the helper recorded for that update to recover. Do not delete
its recovery files or manually mix old and new programs. A `run` command is
deferred until recovery finishes; other interrupted commands visibly ask
you to retry them.

If you retry an ordinary `install` or `update` from a newly downloaded,
extracted copy, it resumes an unfinished earlier job first. It does not queue
the new version until that job has recovered; retry the new operation after
recovery completes. If the stable root launcher itself is missing or damaged,
the emergency command from the extracted copy is:

```powershell
.\ophio.exe install --recover
```

This is only an emergency recovery command, not the normal install or update
procedure. Recovery refuses to proceed if the recorded installation identity
or helper is corrupt rather than guessing which files to run or replace.
Native tests cover recovery before and after replacement, including rollback.
An isolated native locked-image test and packaged recovery from a real
executable sharing error passed in Windows 11 KVM. Migration, rollback and
re-upgrade from an old personal 0.2.0 installation required a lab-only
permissions workaround; they do not qualify ordinary upgrades. None of these
checks establishes recovery from power loss.

## Removal

Finish active work. From an ordinary PowerShell in the installed root, run:

```powershell
.\ophio.exe service uninstall
```

After it succeeds, the background program is stopped and its per-user login
startup is removed. You can remove `%LOCALAPPDATA%\Programs\Ophio` without
deleting projects or application data. Removal retaining identity,
configuration and project data passed in the virtual-machine check. Do not run
`uninstall-data` if you want to keep that data.
