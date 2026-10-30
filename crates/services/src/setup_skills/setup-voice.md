---
name: Set up voice
description: Download a speech model so the phone can be talked to.
---
Help the person set up voice input, which turns speech from the phone into text on this computer.

1. Run `ophio voice status`.
2. Explain the three models: `tiny.en` is smallest and fastest, `base.en` is a good balance, and `small.en` is the most accurate and the largest. All are English only. Recommend `base.en` unless the computer is slow or short of space, and ask which they want.
3. Run `ophio voice install <model>`. It downloads, checks and selects the model, and can take a few minutes.
4. Run `ophio voice status` again and say whether voice input is ready.
