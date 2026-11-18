# Chat and simultaneous project changes

A Chat turn allowed to change its own project reserves that project before it starts. Tasks that change the same project wait in Next up until the reply ends. Read-only chats and side questions do not reserve the project.

If another agent already reserved the project, Chat still answers immediately. The turn keeps its Ophio tools so it can suggest the requested changes as work, or explain that it can make them in a later reply after the other agent finishes. The agent hears who is changing the project, and the person sees one quiet explanation rather than another notice on every message.

For a sandboxed turn, overlapping writable folders are removed for the whole turn and remain readable. A broad whole-computer write grant cannot exclude one project, so a sandboxed turn temporarily loses that broad grant too. Other separately granted, nonoverlapping folders remain writable. These temporary restrictions never increase the person's access grant. Unrestricted Chat stays unrestricted: its instruction not to change a busy project is advisory, not enforced.

The temporary restriction lasts until the next turn, even if the other agent finishes earlier. A running read-only chat cannot gain permission to change its own project without ending that turn and continuing in a new one. The new turn reserves the project if free, or receives the temporary restriction. Existing access-change handling confirms that the previous process has ended before continuing. Changing the person's saved access does not remove a busy turn's restriction.

Reservations follow the actual turn's ownership, not its Ask or Work label. Normal completion and confirmed stops release them. Helpers still finishing and uncertain process endings retain them until the existing cleanup or reconciliation confirms they are finished. Recovery preserves uncertain chat reservations. Unlike structured Work, a cut-off chat gives the project back as soon as existing recovery checks prove every process ended; its unknown reply outcome and history remain unchanged. A chat that fails also wakes waiting tasks after releasing the project. Queued chat messages advance through the same completion handling.

## Current boundary

The reservation covers the chat's own project, including overlapping parent or child folder grants. It does not reserve every other project listed in the chat's access or reachable through whole-computer access. Coordinating those projects needs a separate multi-project ownership design; the current project and queue records each describe a single reservation. Unrestricted access cannot guarantee one agent changes files at a time.
