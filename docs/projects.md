# Projects on a computer

Each paired computer keeps its own list of explicitly registered project
folders. A Git repository, a folder in your Projects directory, or a project in
another app is not automatically registered. Git is optional.

## Home

Every computer has **Home**, its home folder, first in the project list. It
is where a conversation or task starts when it isn't about one project, and
it is chosen until the person picks a project. It can't be removed, and it
follows the home folder if the computer's account moves it. Files in it
aren't sent to or from the phone; choose a project's folder or Library's for
that. In Library, Home lists everything, and choosing a project narrows its
agents, skills and files to that project's own, with Øphio's agents and
skills after its own as every project's defaults.

The project list opens from the top of the screen with a search field above
it. The field isn't focused when the list opens, so the keyboard stays down
until it's tapped; typing narrows the list by name or folder. A companion
from before Home lists its projects under a **Home** row asking for an
update.

## Add folders from the phone

1. Tap the project name at the top of any tab, then **Add project**.
2. Open **Projects**, **Home**, or a computer drive. Available starting places
   depend on the account running the companion and which folders exist.
3. Tap a folder name to look inside. Use the location buttons to go back up.
   Check a folder to select it, or choose **Select this folder** for the folder
   you are inside. **Already added** folders cannot be selected again.
4. Select several folders, or **Select folders on this page**, then choose
   **Review selection**. Check the full computer-side paths before confirming.
5. Choose **Add projects**. Files stay where they are and each new project's
   queue starts paused. Adding a project does not start a task.

Adding a parent folder does not register all its children as separate projects.
To add several existing projects, open their common parent and select the child
folders. Discovery is one directory at a time, not a recursive scan.

**Filter folders** searches names in the current directory and offers **Show
hidden folders**. Large listings have **Next folders** and **First page**;
selections carry across pages and locations. Up to 100 folders can be selected
for one confirmation.

**Enter a path instead** keeps manual entry and an optional project name
available. Older companions without folder browsing show this fallback and an
update notice; both the phone app and the companion must support the feature.

When a computer has no projects, the project list explains how to get started.
Chat's first-run screen uses the same manual entry form: it checks that the path
is absolute and keeps the folder and optional name on this device if the app
closes. The draft is shared with manual entry from the project list: edits there
appear in Chat when the sheet closes. It clears after the computer confirms the
addition. Adding requires a connection to the computer and this device's
**Manage** permission.

## Access and failures

Browsing and adding require the device's **Manage** permission. Browsing lists
folder names and paths, not file contents, and does not add Library roots or
change permissions. The computer account's filesystem access still applies.
Whole disks, the whole home folder, and Ophio's private folders cannot be
registered as projects. Ophio's configured private directories cannot be opened
in the picker. Ordinary child symlinks and Windows junctions/reparse points are
omitted, as are paths the picker cannot represent.

A listing can become stale if folders move or drives disconnect. Retry or choose
another location. Very large/slow listings are refused rather than read
indefinitely; manual entry can still add a specific known project folder.

Bulk addition sends ordinary project-registration requests one at a time. It
is **not atomic**: already-added projects remain if a later addition fails.
Closing the picker, losing the connection, or losing Manage permission stops
remaining additions; a request already sent may still finish. The command
feedback shows unconfirmed results and reconnect checks them. The picker never
automatically retries an uncertain addition or resumes the rest of a batch.

## Protocol and bounds

The additive `project_folders` feature advertises a Manage-only
`project_folders` query. A null `path` returns starting locations (directory
filters and cursors apply only to a directory path). Responses include the
current folder, its ancestors, a page of folders, existing project IDs and
selection restrictions. `next_after` is an exact filename cursor; keep the same
path/filter for the next page. Pages are live listings, not a frozen filesystem
snapshot. Registration continues to use `add_project` and its existing checks.

The companion keeps 101 directory candidates for a 100-folder page, stops scans
at 100,000 entries or a three-second scan deadline, and bounds replies to 512
KiB. Requests allow paths up to 4,096 bytes, cursors up to 1,024 bytes and filters
up to 256 bytes. At most two blocking readers run at once, with a five-second
response timeout. An OS disk call cannot necessarily be cancelled: a timed-out
reader retains its slot until that call returns. Open-directory path checks and
private-directory exclusions are defensive checks, not a filesystem sandbox
against a hostile local process that can rename or remount directories.
