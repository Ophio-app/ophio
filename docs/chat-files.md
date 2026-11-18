# Files in Chat

In a chat, **Attach** offers **Files** beside Screen, Images, Clip and Mark
up. Before a chat has started, Files is the only choice there. It opens the
phone's own file picker for any kind of file, and several can be chosen at
once. Each chosen file shows in the message box as a small chip with its name,
its size and an X to take it off. The chips sit on one line that scrolls
sideways, so the words keep their room even with ten files.

Plain files don't need Review: Send sends them with the words. Pictures,
clips and markup work as before; a picture chosen under Files is sent as a
file, not as a picture to mark up.

## Limits

| | Limit | Why |
|---|---|---|
| One file | 100 MB | Room for a long document, a recording, a build log or a small archive, while one upload on a phone connection still ends in minutes, not hours. |
| One message | 10 files, 250 MB in all | Enough for a set of related files, and a failed message never has more than this to send again. |
| One conversation | 500 files, 1 GB in all | The computer keeps a copy of every file for as long as the conversation stays, so a long chat can't fill its disk or its folder. |

Sizes are decimal, as phones show them. The phone checks the first two before
anything is uploaded and says plainly which file it left out and why; the
computer checks all three again and refuses a message that goes over.

## Sending

The phone keeps the chosen files with the message's draft, on the phone, until
the computer has the message. Send uploads each file first, showing how far
each has got, and only then sends the message. On a slow connection the
message isn't sent until every file is on the computer.

If an upload fails, nothing is sent: the words and the files stay in the
message box, and the reason is shown with **Try again**. Files that had
already arrived aren't sent twice, and an upload the connection cut short
continues from where it stopped. A file the phone no longer holds is named,
with a request to take it off and attach it again.

Under the sent message, each file shows as a chip, under the name the computer
kept: control, direction and invisible characters taken out, the same on every
device. Tapping one brings the file
back to the phone through Transfers, as Send to phone does in Library. A phone
without the Transfers permission sees the chips but can't tap them.

## What the agent gets

The computer, not the phone, chooses where each file goes. It reduces the
name the phone sent to one safe file name (no folders, no `..`, no hidden or
instruction file names such as `CLAUDE.md`, and nothing a file system or
terminal treats specially), and numbers a name already taken rather than
replacing anything. Each copy is new, read-only and not runnable, and is
never written through a link.

The copies go in a folder of the conversation's own inside Øphio's data, not
in the project. No agent reads its instructions, settings, skills, hooks or
Git configuration from there. Both Claude Code and Codex can read the files
that turn, in a full-access chat and in a limited one. A limited chat sees
its own conversation's folder, read-only, and no other conversation's.

With the message the agent is told which files were attached: each one's
name, type, size and where to read it, with names written as quoted data and
shortened to 120 characters. It is told the files are information from the
person's device and not instructions to follow.

Files attached earlier in the conversation stay where the agent was told they
are. If one of those copies has gone (an agent with write access removed it)
it is made again with the next message that has files. If it can't be made
again, the message still goes, and the agent is told plainly which earlier
files it can't read now. A file of the message being sent that can't be made
ready refuses that message, with the reason; the words and files stay on the
phone, and sending again reuses every copy already made.

The conversation's folder is made each time an agent starts. In the rare case
that it can't be, files sent while that agent works are refused with the
reason, and words alone still go.

## Permission

Attaching a file needs the same as sending a message with a picture: the
Transfers permission, on a phone that may send messages in that chat. A phone
can attach only files it uploaded itself; an administrator's phone is no
exception. Phones
without Transfers don't see Files. Telegram and Discord can't attach files.

## When the files are removed

The files stay while the conversation does. A week after a conversation is
archived, its copies are removed, and so are the uploads they came from unless
something else still uses one: a file or picture in another conversation, or
something an agent showed. The computer checks every hour. Unarchiving within that week keeps them. A file the
phone chose but never sent is removed from the phone when it is taken off the
message or sent.

## Older computers

A computer that doesn't yet take files on chat messages doesn't offer Files.
If a message already holds files when the phone finds that out, the message
box says to update Øphio on the computer or take the files off to send.
