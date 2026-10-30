# Opening item references

The object reference format is
`ophio-object://open/v1/<computer-id>/<kind>/<object-id>`.
Kinds are `task`, `approval`, `attention` and `file`. A file uses its Library root
ID as the object ID and one URL-encoded relative `path` query parameter. References
contain identifiers only, never pairing proofs, credentials or authorization.
References made before the product was named start with
`companion-object://open/v1/` instead. They may still sit in notes and chat
history, so the app opens them the same way.

Android accepts these references through ACTION_VIEW. With app lock configured,
local authentication comes first. The app checks for a paired computer, asks that
computer for the current accessible object, and shows a preview with an explicit
Open item button. Unknown computers offer no pairing or remote actions. Missing
and settled objects explain their state. Opening a reference never grants input,
approves a request, starts work or bypasses the computer's access checks.

Chat adapters include a copyable reference. Paste it into Settings → Open an item
link. Telegram documents HTTP and `tg://` URLs for inline URL buttons, rather than
arbitrary application schemes. Discord documents a URL for link buttons but does
not guarantee that every client accepts an arbitrary application scheme. This
build therefore does not claim tappable custom-scheme links in either channel.
See the [Telegram button documentation](https://core.telegram.org/bots/api#inlinekeyboardbutton)
and [Discord button documentation](https://docs.discord.com/developers/components/reference#button).

Reliable tappable links need an owner-selected HTTPS domain and verified Android
App Links association. This build registers no HTTPS host and makes no claim to
any domain. See [Android's link guidance](https://developer.android.com/training/app-links/create-deeplinks).
The channel reference remains copyable even when the client does not turn it into
a link. Existing chat approval buttons are separate actions with their own checks;
these object references carry no such action.
