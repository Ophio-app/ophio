---
name: Connect Telegram
description: Connect the person's own Telegram bot, so their chats can start and follow Øphio tasks.
---
Help the person connect their own Telegram bot to Øphio.

1. Run `ophio channels status`.
2. If there is no token yet, the person makes a new bot of their own with Telegram's @BotFather: they send it `/newbot`, choose a name and a username, and get a token. That happens in Telegram, signed in as themselves; walk them through it, and wait. A token another program already uses won't do.
3. They store the token themselves, in their own terminal, with `ophio channels token set telegram`, which asks for it without showing it. Never ask for the token in this chat, and never look for it anywhere. Wait until they say it's done.
4. Run `ophio channels enable telegram`, then `ophio channels status` to check the bot connected.
5. The person writes to the bot. It answers with a short code, which they approve in the Øphio app under Connections › Chat, and there they choose the project that chat works in. Every person in a group links this way, and each group needs its own project.
6. Explain `ophio channels exposure`: by default the bot posts task states only; `titles` also posts task titles, summaries and approval details, which Telegram and everyone in the chat can read. Change it only if they ask.
7. Run `ophio channels status` again and say what is ready.
