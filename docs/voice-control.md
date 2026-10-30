<!-- SPDX-License-Identifier: Apache-2.0 -->
# Voice control

Voice control lets you say what to do in a browser or on the desktop, and Øphio does it. TypeSafe's Jev works out what you meant from what you said and the controls on screen.

Link your own TypeSafe access on the computer with `ophio jev connect`. Enter the key at the hidden prompt, or supply it through standard input; never put it in command arguments. Øphio stores it in its protected data directory (0600 on Linux, user-bound protection on Windows). The phone receives only whether Jev is linked. The CLI explains what is sent and how it is charged before storing the key. `ophio jev status` reports the locally known state without contacting TypeSafe. `ophio jev disconnect` removes the saved key, and voice control stops until Jev is linked again. “Linked” means a protected key is stored; access is tested only when a voice command asks Jev.

Charges go to your own TypeSafe access and are separate from Claude and Codex subscriptions. The session receipt reports Jev's token usage when returned; it does not estimate a price. Consult [TypeSafe's model pricing](https://docs.typesafe.ai/models) for current rates. Requests go directly to TypeSafe. TypeSafe and its service providers process them according to [its privacy policy](https://typesafe.ai/legal/privacy-policy). TypeSafe says it does not train on input, but ordinary accounts do **not** have a zero-retention guarantee; see [data processing terms](https://typesafe.ai/legal/data-processing) and [the retention overview](https://docs.typesafe.ai/legal).

Øphio pins **TypeSafe SystemOne v1**, endpoint `https://api.typesafe.ai/v1/systemone`, and model `jev-1.13.0` together. It uses a Bearer key, refuses redirects and does not retry requests. The [direct API reference](https://docs.typesafe.ai/api) documents this contract.

## Using it

Browser voice control needs Jev linked. Desktop voice control additionally requires an accessible focused app. Each has its own switch in **Settings › Data and voice › Voice control**. Disconnecting Jev, or a failed Jev check, makes voice control unavailable until Jev is linked again.

Open Control → Voice control and choose a target, or use Voice in a display's live desktop view. The start sheet explains the separate browser profile, computer takeover and data sharing. Preview mode is selected initially and sends no input. The local speech recognizer supplies text; a typed command is also available. Install a local speech model through Data and voice to enable speech. While voice control runs, the live desktop view only watches: touch input from that device is off, and leaving or hiding the view does not end the session. Stop voice returns touch and keyboard input.

Voice interpretation sends the transcript and the names, roles and identifiers of at most 100 controls to TypeSafe. It does not send screenshots, field values or general page text. Charges use the person's own Jev access. Transcripts, validated answers, actions, results and reported Jev usage are retained in the computer's session receipt; audio is not retained. Jev usage remains separate from agent usage.

Text, web addresses and key names must come from the command itself. Ambiguous controls require a numbered choice. Destructive actions require confirmation. Voice typing refuses password and other recognized sensitive controls; use the remote keyboard for those. Each action checks the current control identity and position again. A missing accessibility snapshot prevents a new desktop voice session; the ordinary remote keyboard remains available.

Browser control uses an isolated Chromium profile and inherited CDP pipes, with no debugging TCP port. Ending automation leaves that browser window open. Closing the window removes its profile, with any cookies and sign-ins, once the browser and its helpers have exited. Profiles of windows that closed while Øphio was not running are removed when it next starts; one a running process still uses is kept. DOM observations include same-origin frames and safe accessibility labels. The driver does not automate cross-origin frames in this version.

The ordinary input lease and local Stop indicator also protect voice sessions. Stop, local Stop, lock, revocation, heartbeat expiry and ten minutes of inactivity end input authority. Sessions also stop after repeated unchanged observations, 100 recorded steps or a bounded receipt size. A cancelled native action remains tracked until its input barrier completes; missing confirmation is recorded as unknown and is never retried automatically.

Linux accessibility uses AT-SPI; Windows uses UI Automation. OCR is not implemented. Automated qualification uses synthetic accessibility replies and a real headless Chromium against loopback fixture pages. Windows native execution and a provisioned Linux desktop guest still require qualification; a successful cross-compile is not runtime evidence.
