---
name: Give the person a checklist
description: Puts whatever the person has to do themselves on the project's checklist in Øphio, as complete steps with every link, text and file in the step that needs it, and carries on the moment they tick off what you're waiting on. Use whenever you'd otherwise give the person directions or need them to take steps: sign up or in, grant access, make a key, pay, approve, choose, install, set up or check something only they can.
---
Whenever the person has to do something themselves, it goes on the project's checklist, not into your reply. A chat scrolls away and a file gets lost; the checklist stays under New in Chat and in Tasks until it's done, and ticking off what you wait on starts you again at once.

## When

Use it whenever they have to act outside this conversation:

- sign up or in somewhere, or accept terms;
- grant access, make a key or token, or add a secret to a service;
- pay for, order or approve something;
- choose something that is theirs to choose, after looking into it;
- install, plug in, set up or check something you can't reach, such as their phone, a device or a website's settings;
- try or review something by hand.

Do yourself whatever you can do yourself. A quick question with nothing to do around it you ask as you normally would.

In your reply, say in one line what you put on it, such as "I put 3 items on the checklist; the first unblocks the deploy." Don't repeat the steps in the chat.

## Writing an item

- One item is one thing they can do in one sitting, in one place. Start the title with a verb and name the thing: "Grant Øphio access to the billing API".
- why: one line on what it unblocks.
- priority: what the checklist goes by first. blocker only when important work is stopped until it's done, such as an item you wait on for the work you're doing. high for what should be done soon, low for whenever suits them. Leave it out for medium, the usual.
- due: a day you suggest, as YYYY-MM-DD, only when there's a reason for one, such as a launch or an expiry. It orders items of the same priority, and once the day comes the item moves up to just under the blockers.
- minutes: an honest estimate.
- after: the ids of items to do first.
- steps: what to do, in order, one action each, for someone who has never done it.
  - Name buttons, menus and fields exactly as the screen shows them.
  - Put the link on the step that goes there, as url.
  - Put anything they type or paste in copy: a command, a value, a setting, a DNS record. One per step.
  - Put a file they need, such as a config, a form or a picture, in file, from the project or your scratch folder. Øphio gives it to them the way they chose: to their phone, by Taildrop, into a folder or onto a drive.
  - End with how they'll know it worked: "You should see “Access granted”."
- Everything they need is in the item. Never send them to a chat, a message, a README, a path on this computer or "the steps above".
- ask: when you need something back, such as a choice, a name, an ID or a confirmation. Offer options, each a label and a detail, when there are a few sensible ones; they can still write their own. Their answer comes to you with the tick.
- Never put a password, token, key or other secret in a step, a copy or a file, and never ask for one as an answer. When you need one, have them store it themselves where it belongs, in Øphio's Settings › Accounts, the service's own secret store or a command that asks for it in their own terminal, and say exactly where.
- Group items into sections by stage or place, such as "Accounts", "Before launch" or "On the phone". Øphio puts the most pressing first: blockers, then items whose day has come, then by priority and the day suggested, with what an item comes after just ahead of it. Otherwise it keeps your order.
- The first time, give the checklist a title and a one-line summary. If the project has a brand, such as a logo, colours in its styles or a style guide, give theme: accent and surface as #rrggbb, the font's style, the name and the logo. Otherwise leave theme out and Øphio's own look is used.

## Waiting on it

Give waiting on each item you're blocked on, saying what you'll do once it's done, such as "deploy the API with the new key", and make it a blocker when the work it stops matters. Then do everything that doesn't depend on it and end your turn with a short summary. Don't wait, sleep, poll or call wake_me for it: the moment they finish that item, Øphio starts your next turn with what they answered, even if the rest of the checklist is still open. Items nothing waits on don't need waiting.

## Your own work

The checklist is only for what can't be done now: what the person has to do, and your own work that waits on it. Not for work you are about to do, and never a reason to slow down: work that nothing blocks, just do.

When work of yours can't start until they finish items of theirs, such as deploying once they've granted access, put it on as your own item: owner "agent", with after naming their open items it needs. Øphio refuses one that names none. Give it a title, a why and, if you like, steps for your plan; leave waiting and ask out. They see it in their To-Do marked for the agent, and can send feedback on it but not tick it.

after may name your own items to do first too, but always one of theirs. The moment nothing it comes after is open, Øphio starts your next turn to do it, or says so in checklist_add's reply when what you did yourself made it ready. You keep it current: mark it done with done: true as soon as you've done it (done: false opens it again), rewrite it when the plan changes, take it off when it no longer applies, and answer their feedback with checklist_reply. A rewrite keeps the item's owner unless you give another, and an open item of yours keeps the item of theirs it comes after in after.

## Keeping it right

- Read it first with checklist_read: it has every id, what is ticked, their answers and their questions.
- Change an item's priority when things change: a blocker whose work no longer waits on it goes back to high or medium.
- The same id rewrites an item, so fix one that turned out wrong rather than adding another. A ticked item stays ticked unless you give reopen.
- Take items that no longer apply off with checklist_remove. Leave what they ticked.
- A question they ask under an item comes to you. Answer it with checklist_reply, naming the item, short and direct. If the answer changes what to do, rewrite the item too.
- When an item turns out to be something you can do, do it and take it off.

## Without Øphio's tools

Outside Øphio, as a Claude Code or Codex session of your own with no checklist_add tool, use Øphio's command. `ophio projects list` shows each project's ID and folder.

- `ophio checklist show <project>` prints the checklist with every id, as checklist_read does.
- `ophio checklist put <project> <file>`, or `-` to read standard input, takes the same JSON as checklist_add: `title`, `summary`, `theme`, `section` and `items`. Files are paths in the project's folder.
- `ophio checklist remove <project> <id>…` takes items off.

`<project>` is the project's ID or its folder. Leave waiting out: no agent of Øphio's is waiting, so tell the person in your own reply what to come back to you with. Their questions go to a new task in Øphio, which answers them. Nothing starts you for your own items, and feedback on them stays under them, where `show` prints it; mark them done with done: true yourself.
