<!--
App Store "What's New" for the iPhone app. Write it in the change that bumps the version:
a `## <version>` section with one `### <locale>` per App Store language (zh-Hant, en-US),
listing what iPhone users notice, without emoji (the App Store rejects them; asc-submit.py drops
any left). tools/asc-submit.py reads the released version's section;
a version without one gets a generic "Bug fixes and improvements" line. Delete old sections.
-->

## 2.31.0

### zh-Hant
- 對話排法改成 Grok Bot 的樣子：Bot 的訊息比較窄、外側留白，只有你自己送出的訊息是實心泡泡，長對話更好讀。
- 進行中的那一輪，它的計畫浮在輸入框上方，隨時看得到接下來要做什麼。
- 你自己伺服器上的 Hermes gateway 可以在電腦上的 Codync 加入，Bot 在手機的「管理電腦」→「Hermes connections」看得到。那台電腦上的 Bot 能加入同一個對話、和本機的 Bot 一起工作，各自擁有自己的 Hermes profile 與記憶。
- 用量頁面說明 Hermes 不提供額度上限，不再空白。

### en-US
- The conversation is laid out like Grok Bot: a bot's messages stay narrow with room on the outside, and only your own message is a solid bubble, which reads better in a long chat.
- The running turn's plan floats above the composer, so what comes next stays in view.
- A Hermes gateway on your own server, added in Codync on your computer, shows up in Manage computers > Hermes connections. Bots running there join the same conversation as the bots on your computer, each in its own Hermes profile with its own memory.
- Usage says Hermes reports no limits, instead of showing nothing.
