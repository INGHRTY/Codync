<!--
Google Play release notes for the Android app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per Play language (zh-Hant, published as zh-TW;
en-US), listing what Android users notice, each under 500 characters (Play's limit;
tools/play-submit.py cuts longer notes at the last whole line). tools/play-submit.py reads the
released version's section; a version without one gets the PRs' "What's New: Android" bullets,
else a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.31.0

### zh-Hant
- 對話排法改成 Grok Bot 的樣子：Bot 的訊息較窄、外側留白，只有你自己的訊息是實心泡泡。
- 進行中的那一輪，計畫浮在輸入框上方，隨時看得到接下來要做什麼。
- 你自己伺服器上的 Hermes gateway 在電腦上的 Codync 加入後，「管理電腦」→「Hermes connections」看得到；那台電腦的 Bot 能加入同一個對話，各自擁有自己的 Hermes profile。
- 用量頁面說明 Hermes 不提供額度上限，不再空白。

### en-US
- The conversation is laid out like Grok Bot: a bot's messages stay narrow with room on the outside, and only your own message is solid.
- The running turn's plan floats above the composer, so what comes next stays in view.
- A Hermes gateway on your own server, added in Codync on your computer, shows up in Manage computers > Hermes connections. Its bots join the same conversation, each in its own profile.
- Usage says Hermes reports no limits, instead of showing nothing.
