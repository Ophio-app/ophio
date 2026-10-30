---
name: Connect Discord
description: Connect the person's own Discord bot, so their direct messages and server channels can start and follow Øphio tasks.
---
Help the person connect their own Discord bot to Øphio.

1. Run `ophio channels status`.
2. If there is no token yet, the person makes an application with a bot in the Discord Developer Portal (https://discord.com/developers/applications), signed in as themselves, and copies the bot's token. Privileged intents stay off. To use it in a server, they invite it with the OAuth2 URL generator's `bot` and `applications.commands` scopes and the View Channel and Send Messages permissions. Walk them through it, and wait.
3. They store the token themselves, in their own terminal, with `ophio channels token set discord`, which asks for it without showing it. Never ask for the token in this chat, and never look for it anywhere. Wait until they say it's done.
4. Run `ophio channels enable discord`, then `ophio channels status` to check the bot connected.
5. The person writes to the bot: ordinary messages in a direct message, slash commands in a server. It answers with a short code, which they approve in the Øphio app under Connections › Chat, and there they choose the project that chat works in. Every person links this way, and each channel and thread needs its own project.
6. Explain `ophio channels exposure`: by default the bot posts task states only; `titles` also posts task titles, summaries and approval details, which Discord and everyone in the channel can read. Change it only if they ask.
7. Run `ophio channels status` again and say what is ready.
