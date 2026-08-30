# コンソールコマンドの棚卸し

> 索引: [`../DESIGN.md`](../DESIGN.md)

シェルが受け付けるコマンドを全数列挙し、**製品として残るもの**と**足場**に
分けた資料です。コンソールの仕組みとシェルの実装は
[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)、各コマンドの正式な説明は
`src/app/shell.rs`の`HELP_ENTRIES`（実機では`help <name>`）が唯一の出典で、
この文書は説明を写し取らず**割り当てと、足場が生きている理由だけ**を持ちます。

実際の削除計画は[`COMMAND_RETIREMENT_PLAN.md`](COMMAND_RETIREMENT_PLAN.md)です。

## 分け方

軸は「誰に有用か」ではなく「**いつ不要になるか**」です。

外から見れば、DW-GDMAの調停レジスタを読むコマンドも表示を2時間焼くコマンドも
同じくらい使い道がありません。どちらも板を使う人のために書かれていないからです。
両者を分けるのは、**それを書かせた作業より長く生き残るかどうか**で、大半は
生き残りません。

| 区分 | 意味 | 数 |
| --- | --- | --- |
| 製品（`Group::Product`） | 板を操作する。残る | 41 |
| 足場（`Group::Scaffold`） | 何かを動かすため、動くと示すために書いた。片付いたら消える | 57 |

`src/app/shell.rs`の`execute`が受け付ける名前は104個、`HELP_ENTRIES`は98項目です。
差の6個は別名（後述）で、「helpにあるのに実行できない」名前はありません。
**この表の区分がそのまま`HELP_ENTRIES`の`group`です**。片方を動かしたら
もう片方も動かしてください。

以前この文書は一般／専門家／診断の3分類でしたが、寿命の軸で見ると「専門家に
有用」は独立した層になりません。`pma`はメモリマップを立ち上げるため、`membench`と
`icm`は表示帯域を詰めるため、`tls`は未認証で製品の取得手段になり得ないため——
どれも作業に紐づいた足場です。中間層は無く、2値です。

## 製品（41）

| コマンド | 位置づけ |
| --- | --- |
| `help` `clear` `echo` `about` | シェルの基本 |
| `mem` `uptime` `backlight` | 使用量、経過時間、バックライト |
| `battery` | 電圧・電流・残量の常時表示 |
| `rtc` | 時計の表示と`rtc set`（`regs`／`test`は足場側の用途） |
| `reboot` `shutdown` | 再起動と電源断 |
| `devices` `mount` `umount` `mounts` `automount` | ボリュームの着脱と一覧 |
| `cd` `pwd` `ls` `cat` | ファイルの閲覧 |
| `mkdir` `write` `append` `rm` `rmdir` `mv` | ファイルの作成・編集・整理 |
| `lsusb` `usbinfo` `usbrescan` | USB-Aの接続確認と再スキャン |
| `wifi` `wifiscan` `wificonnect` `wifistatus` `wifidisconnect` `wififorget` | 接続の操作 |
| `ipconfig` `nslookup` `ping` | アドレス設定と疎通確認 |
| `tftpget` | ファイルを実機へ取り込む経路 |
| `browser` | ハイパーテキストビューア |
| `paint` | タッチで描く。診断ではなくアプリ |

`usbinfo`は`lsusb`（引数なし）とほぼ同じ内容を出します。製品側にあるのは
重複の解消先としてで、片方に寄せる余地があります。

## 足場（57）と、生かしている作業

足場が消せるかどうかは、それを書かせた作業が閉じたかどうかで決まります。
この repo では`docs/*_PLAN.md`が作業単位なので、**各足場コマンドを計画へ
紐づけ、計画が閉じたら退役**という対応にします。判断が repo の状態から
読めるので、後から誰が見ても同じ結論になります。

計画の状態は各`*_PLAN.md`の`## 状態`から取りました（2026-08-30時点）。

| 生かしている作業 | 状態 | 足場コマンド |
| --- | --- | --- |
| [`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md) | **未着手**（Stage 0〜7すべて）。Stage 3が`fonttest`での実機比較を要求している | `fonttest` |
| [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md) | hub statusは低頻度fallbackを維持中 | `usbperiodic` `usbhub` |
| [`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md) | **凍結**（Stage 2 CBI ADSC未解決、Stage 3〜4未着手。再開予定なし） | `usbfs` |
| [`WIFI_REFACTOR_PLAN.md`](WIFI_REFACTOR_PLAN.md) | Stage 8（長時間・異常系の実機受入）未着手 | `wifilog` |
| [`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) | Stage G未着手。ただし検証用の2台目ハブが無い前提で、想定どおりの未着手 | （固有のコマンドなし） |
| **閉じた計画** | 下表 | 残り |

### 計画が閉じている足場（退役候補）

紐づく作業が完了しているものです。**それ自体が「もう消せる」の意味**ですが、
現状文書が受入の根拠として名指ししている場合は、消すとその根拠を再取得する
手段が無くなります。実際の判断は
[`COMMAND_RETIREMENT_PLAN.md`](COMMAND_RETIREMENT_PLAN.md)で行います。

| 閉じた計画 | 状態 | 足場コマンド |
| --- | --- | --- |
| [`DISPLAY_UNDERRUN_REFACTOR_PLAN.md`](DISPLAY_UNDERRUN_REFACTOR_PLAN.md) | 完了（Stage 0〜4で全受入条件合格、Stage 5〜8は不要判定） | `stress` `displaybench` `db` `dp` `di` `icm` `membench` `mix` |
| [`PPA_FILL_PLAN.md`](PPA_FILL_PLAN.md) | 完了（Stage 1〜6、実機確認済み） | `ppafill` |
| [`FLASH_XIP_MIGRATION_PLAN.md`](FLASH_XIP_MIGRATION_PLAN.md) | 全Stage完了 | `pf` `rt` `alloctest` |
| [`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)／[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)／[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md) | 読み出し・WRITE(10)安定化とも受入完了 | `ut` `usbmargin` `usbread` `usbwritetest` `usbzero` `usbmsc` `usbmbr` `usbhw` `usbvbus` |
| [`FILESYSTEM_PLAN.md`](FILESYSTEM_PLAN.md)／[`ROOT_FILESYSTEM_PLAN.md`](ROOT_FILESYSTEM_PLAN.md)／[`FILESYSTEM_WORKFLOW_PLAN.md`](FILESYSTEM_WORKFLOW_PLAN.md) | 完了（VFS、RAMルート、カレントディレクトリ、自動マウント） | `fsopen` `fsread` `fsclose` `fsverify` `fill` `blkread` `sdinfo` `sdmbr` `sdread` `sdreadn` `sdreadpsram` `sdwritetest` `sdzero` |
| [`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md) | 全Stage完了（実機確認済み） | `wifiinfo` `wifiup` `wifimac` `wifisaved` |
| [`TCPIP_PLAN.md`](TCPIP_PLAN.md)／[`DNS_PLAN.md`](DNS_PLAN.md) | 完了（実機確認済み） | `netdump` `httpget` |
| [`TLS_PLAN.md`](TLS_PLAN.md) | Stage 8まで完了（2026-08-28実機受入済み） | `tls` `entropy` |
| [`INPUT_MANAGER_PLAN.md`](INPUT_MANAGER_PLAN.md) | 完了（実機確認済み） | `touchtest` |
| （`GRAPHICS.md`へ同期済み。紐づく計画なし） | CW回転とクリッピングの確認手段として書かれた | `coordtest` |
| （[`BOOT.md`](BOOT.md)・[`PSRAM.md`](PSRAM.md)へ同期済み。紐づく計画なし） | メモリマップとコア識別の立ち上げ確認として書かれた。引き継いだPMAテーブルは全16エントリがロック済みで書き換えられないため、読む以上のことはできない | `cpuinfo` `pma` `pmp` |
| [`FONT_MIGRATION_PLAN.md`](FONT_MIGRATION_PLAN.md) | 完了（2026-08-27） | （`fonttest`は上の未着手計画が生かしている） |
| [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md) | HIDマウスの実証として書かれた | `win` |
| [`WEB_BROWSER_PLAN.md`](WEB_BROWSER_PLAN.md)／[`BROWSER_UI_PLAN.md`](BROWSER_UI_PLAN.md) | 完了（BROWSER_UIはStage 0〜10、実機確認済み） | `bt` `hs` |
| [`STARTUP_SCREEN_REFACTOR_PLAN.md`](STARTUP_SCREEN_REFACTOR_PLAN.md) | 完了（Stage 0〜8、実機確認済み） | `ui` |
| （紐づく計画なし） | BMI270の疎通確認として書かれた | `axistest` |

## 破壊的なコマンド

`sdzero` `usbzero` `sdwritetest` `usbwritetest`の4つは、ファイルシステムを
迂回してLBAへ直接書きます。破壊性は製品／足場のどちらとも直交します——製品の
`rm`も破壊的で、足場の`membench`は無害です。区分では表せないので、`help`側で
警告を出すなら`HelpEntry`に別のマーカーが要ります。

名前からは`sdwritetest`が書き込むことが読み取りにくく、`help`本文まで読まないと
分かりません。

## 別名

`HELP_ENTRIES`の`aliases`に持たせてあり、`help <本体名>`が`also:`行で出します。

| 別名 | 本体 |
| --- | --- |
| `version` | `about` |
| `reset` | `reboot` |
| `poweroff` | `shutdown` |
| `batinfo` | `battery` |
| `browsertest` | `bt` |
| `httpstream` | `hs` |

以前はこの6つが`execute`の`match`にしかなく、`help`から辿れませんでした。
名前解決を表へ一本化した経緯は[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)にあります。

## 名前空間について

`db` `dp` `di` `ui` `ut` `pf` `rt` `bt` `hs` `mix`のような打ちやすい2文字名は、
すべて足場に割り当たっています。足場を退役させるとこれらは空きます。
製品側で短い名前が欲しくなったときの原資です。
