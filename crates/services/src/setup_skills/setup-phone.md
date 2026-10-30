---
name: Set up my phone
description: Install Øphio on an Android phone or tablet from this computer, over Wireless debugging, and pair the two.
---
Help the person get Øphio onto their Android phone and paired with this computer.

1. Run `ophio status` and `ophio devices` to see what is paired already.
2. Ask whether the phone already has Øphio. If it does, skip to step 6.
3. Run `ophio phone find`. If `adb` isn't found, it needs Google's SDK Platform-Tools: suggest the system's `android-tools` package (they approve the install; it may need their password), or Google's download, whose terms the person accepts on Google's page. Then run `ophio phone find` again.
4. If no phone is listed, the phone pairs with this computer's Wireless debugging. Tell the person: on the phone, turn on Developer options (Settings › About phone, tap Build number seven times), then Settings › Developer options › Wireless debugging, on the same Wi-Fi as this computer. Then ask them to run `ophio phone pair` in their own terminal, or use the phone step of `ophio setup`, and scan the QR code it shows from "Pair device with QR code". Wait for them, then run `ophio phone find` again. A phone paired before may only need `ophio phone connect <address>` with the address its Wireless debugging screen shows.
5. Run `ophio phone install`. It installs the app that came with this download, and only if it is signed by Øphio. If it says the phone has an Øphio signed with another key, explain what it says and leave the choice to the person; never uninstall it yourself. If the phone asks to scan the app or allow the install, the person answers on the phone.
6. Run `ophio phone link --alerts on`. It opens Øphio on the phone with this computer's pairing link. The phone shows a fingerprint and a code; the person checks them and confirms the pairing in `ophio setup`, or with `ophio pair confirm` in their own terminal. Tell them what they'll see, and wait.
7. Run `ophio phone alerts` so alerts reach the phone while the app is closed.
8. If the phone will be away from this Wi-Fi, the `setup-tailscale` skill covers reaching the computer from elsewhere; `ophio phone open tailscale` opens Tailscale on the phone, or its store page.
9. Run `ophio phone off` to turn Wireless debugging off again, and run `ophio devices` to show the new device and what it may do. Changing what it may do is the person's, with `ophio devices allow` in their own terminal.
