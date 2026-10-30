---
name: Sign in my accounts
description: Explain which accounts Øphio can give tasks, and guide the person through signing them in.
---
Help the person connect the accounts their tasks use, such as GitHub or Google, through Øphio's Accounts.

1. Run `ophio accounts` to list the accounts Øphio knows and their state. Keys are never shown.
2. Ask which services their tasks need, and explain that Øphio keeps each account's keys in this computer's keyring and gives a task only the accounts it is allowed.
3. Signing in is the person's own: they add a service and sign in from the app, under Settings › Accounts, and their browser or password manager fills in their details. Never sign in for them, never open sign-in pages yourself, and never ask for a password, key or code in this chat.
4. If a Google service needs its own OAuth client, the person creates a Desktop app client in their Google Cloud console and downloads its JSON; then `ophio accounts set-client google <file>` stores it, after which the downloaded file can be deleted.
5. Run `ophio accounts` again and say which accounts are ready.
