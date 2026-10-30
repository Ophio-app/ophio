---
name: Set up everything I picked
description: Go through the setup the person chose, in order, one part at a time.
---
Set Øphio up for the person, one part at a time.

1. Run `ophio status` and `ophio doctor` to see what is done.
2. Ask which of these they want, recommending the ones not done yet: their phone, their projects, Tailscale, voice, Telegram, Discord, desktop access and their accounts. If the task's message already names them, use that.
3. Do each chosen part in this order: projects, phone, Tailscale, voice, desktop access, Telegram, Discord, accounts. For each, follow the matching steps:
   - Projects: find folders holding `.git` under the usual places, ask which to add, and add each with `ophio projects add <folder>`.
   - Phone: `ophio phone find`, `ophio phone install` and `ophio phone link --alerts on`; pairing the phone's Wireless debugging and confirming Øphio's pairing are the person's, in `ophio setup` or their own terminal.
   - Tailscale: they install it with your help, and start and sign in to it themselves; then check `ophio status` offers its address.
   - Voice: `ophio voice install base.en`, or the model they choose.
   - Desktop access: `ophio desktop enable`; device permissions are the person's, with `ophio devices allow`.
   - Telegram or Discord: they make their own bot and store its token themselves with `ophio channels token set <bot>`; then `ophio channels enable <bot>`, and they write to the bot and approve its code in the app under Connections › Chat.
   - Accounts: they sign in from the app, under Settings › Accounts.
4. Before each part, say in a line what it will do. After it, say what changed. Let the person skip any part.
5. Finish with `ophio doctor`.
