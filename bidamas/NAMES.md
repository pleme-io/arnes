# arnes's bidama name ledger

arnes's own distribution: the bidamas the kernel hosts by default
(`docs/kernel.md`). Same discipline as blue's public `bidamas/NAMES.md`: every
package here has a row, and every row records the sweep. Names follow the goi
(語彙) rule, the plain Japanese noun for the subject.

Sweep: `rg -l --no-ignore -w -i <word>` over `~/code/github/pleme-io`, control
`shikumi` = 1772 files.

| name | kanji | gloss | swept | verdict | note |
|---|---|---|---|---|---|
| `meirei` | 命令 | command: arnes's built-in slash commands | 2026-09-28 | KEEP | `meirei` 0 files; `命令` 9, all vendored zh/ja locale strings (adminer, yargs, ansible-ui, awx, minio docs), not claims; near-homophone `meire` 2, faker pt locales. 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `joutai` | 状態 | status: the status line's segments | 2026-09-28 | KEEP | `joutai` 0 files; `状態` 1, adminer's ja locale; `joutei` 0. 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `kenban` | 鍵盤 | keyboard: arnes's default key bindings | 2026-09-28 | KEEP | `kenban` 0 files; `鍵盤` 0 (control `shikumi` = 1772). Rejected first: `kagi` (鍵, key), 908 files. 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `yushutsu` | 輸出 | export: the rows a hosted bidama tells arnes it serves | 2026-09-28 | KEEP | `yushutsu` 0 files; `輸出` 8, vendored zh-tw locale strings and a faker ja lorem file, not claims. 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `kenkou` | 健康 | health: arnes's MCP health vigy | 2026-09-28 | KEEP | `kenkou` 0 files; `健康` 19, all corpus or locale data (a zh subtitle benchmark, ansible-ui zh translations, a public suffix list, vendored node_modules), not claims. 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `sanshou` | 参照 | reference: `@path` mentions attached to a prompt | 2026-09-28 | KEEP | `sanshou` 0 files, `参照` 0, `sansho` 0 (control `shikumi` = 1772). 0 repo dirs. Unclaimed in blue's `NAMES.md` and CATALOG. |
| `wariate` | 割当 | assignment: each prompt assigned a model route, the choose-route policy over chokkan's Jev judgment, with the `@label` directive and `/route` | 2026-10-08 | KEEP_WITH_NOTE | `wariate` 0 files, `割当` 0, `wariai` 0 (control `shikumi` = 1592, `caixa` dirs = 138). 0 repo dirs, 0 theory files, 0 keyword or org.yaml rows. Chosen over `furiwake` (振り分け, routing): it shares `fur-` with the shipped `furui` (篩, ranking) and the same sorting sense, the adjacent-sense hazard. Embedded, not hosted by default: host it with `:bidamas ("wariate")` and `routing.enabled`. |
