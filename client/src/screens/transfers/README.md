# Transfer composer files

Transfers views in Library and Tasks share a persisted composer, while each
mounted view holds its own upload Blob. Filename and size do not identify the
contents: two revisions of `notes.txt` can have identical metadata.

Every selection gets a new `fileId`. The file picker, Capture inbox, and
**Send again…** store the bytes at `composerFileKey(profileId, fileId)` before
publishing that identity through `chooseComposerFile`. Views enable Send only
when their held bytes match the composer's current identity. Late storage reads
cannot replace a newer selection. Replaced and successfully sent files release
their stored bytes; drafts from before file identities still use the legacy key.
An unread composer keeps earlier bytes until recovery; a failed composer write
keeps the bytes still named on disk. Recovery copies distinguish equal-metadata
files by their identities and retain their bytes.

If storage fails during a local pick, that view retains the selected bytes in
memory and tells the person to keep the app open. Completion of an earlier
upload must not discard that selection. Choosing Text during the pending save
must keep Text selected. Publishing the saved file while Text is selected does
not advance the active draft's revision: text submitted from another view still
clears when its upload completes, and the selected file stays available. Actual
edits to the text, name, destination, or source continue to protect a newer draft
from that completion, including after an app restart.
Recovering an unread draft advances its revision beyond any upload started during
the outage, including a File upload that discovers an unsent Text draft.

Run `npm test -- src/screens/transfers` from `client/`. The composer byte tests
cover equal-metadata replacements through Inbox and Send again, multiple mounted
views, delayed reads and writes, storage failure, upload completion, and legacy
drafts. Storage is mocked in these tests; physical phones and native file
pickers are not exercised. This logic is shared by Android and iPhone.

Capture inbox asks before replacing an unsent note or chosen file. Cancel keeps
the draft; Keep opens Transfers with it; Replace hands over the shared item.
Empty slots, identical items, and drafts already submitted can pass through.
Confirmation covers the revision and file identity, since choosing an inactive
file leaves the active text revision unchanged. If the draft changes during
confirmation or an asynchronous storage operation, the handoff checks it again
before publishing a replacement. The Inbox handoff tests cover these cases.
