# コンソールコマンドの棚卸し

> 索引: [`../DESIGN.md`](../DESIGN.md)

2026-09-04時点の棚卸しを基に、シェルが受け付けるコマンドを1行ずつ並べ、**残すか消すか**を示した表です。
コンソールの仕組みとシェルの実装は[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)、各コマンドの
説明は`src/app/shell.rs`の`HELP_ENTRIES`（実機では`help <name>`）が唯一の出典で、
この文書は説明を写し取らず**割り当てと根拠だけ**を持ちます。

削除・feature化の実施計画は[`COMMAND_RETIREMENT_PLAN.md`](COMMAND_RETIREMENT_PLAN.md)です。

## GUI統合後の差分

`battery`／`batinfo`は廃止した。`win`は通常GUIデスクトップを開く製品コマンドに変更した。Wi-Fiは`wifi <subcommand>`へ統合し、
旧単独名は廃止。以下の個数と区分は元の棚卸し時点の記録であり、現行の受付名は
`HELP_ENTRIES`と[`WIFI.md`](WIFI.md)を参照する。

## 分け方

区分の軸は「誰に有用か」ではなく「**いつ不要になるか**」です。

外から見れば、DW-GDMAの調停レジスタを読むコマンドも表示を2時間焼くコマンドも同じくらい
使い道がありません。どちらも板を使う人のために書かれていないからです。両者を分けるのは
**それを書かせた作業より長く生き残るかどうか**で、大半は生き残りません。

| 区分 | 意味 | 数 |
| --- | --- | --- |
| 製品（`Group::Product`） | 板を操作する。残る | 41 |
| 足場（`Group::Scaffold`） | 何かを動かすため、動くと示すために書いた | 62 |

**この区分がそのまま`HELP_ENTRIES`の`group`です。**片方を動かしたらもう片方も
動かしてください。

処遇は足場をさらに3つに分けたものです。足場が消せるかどうかは、それを書かせた作業が
閉じたかどうかで決まります。この repo では`docs/*_PLAN.md`が作業単位なので、
**計画が閉じたら退役**という対応にしています。

| 処遇 | 意味 | 数 |
| --- | --- | --- |
| **残す** | 製品41個と、まだ作業が閉じていない足場1個 | 42 |
| feature | 紐づく作業は閉じたが、受入の根拠がそこでしか再取得できない。`#[cfg(feature = "diag")]`で落とす | 50 |
| 削除 | 紐づく作業が閉じ、結果も現状文書へ同期済み | 11 |

紐づく計画の状態は各`*_PLAN.md`の`## 状態:`行から取りました（2026-09-04時点）。
ただし**状態行は人が手で更新するのでコードに遅れます**。2026-09-04には
`USB_WRITE_STABILITY_PLAN.md`が「未解決」のままなのに`MAX_WRITE_BLOCKS`が既に`8`で、
状態行だけを見て誤分類しました。コードと突き合わせて確かめてください。

## 一覧（103コマンド）

「破壊」の⚠はデータを失わせ得るものです。`sdzero`／`usbzero`／`sdwritetest`／
`usbwritetest`／`usbmultiwrite`／`usbrawcheck`はファイルシステムを迂回してLBAへ直接
書きます。破壊性は区分とも処遇とも直交します——製品の`rm`も破壊的で、足場の`membench`は
無害です。

| コマンド | 別名 | 区分 | 処遇 | 破壊 | 根拠 |
| --- | --- | --- | --- | --- | --- |
| `about` | `version` | 製品 | **残す** |  | シェルの基本 |
| `alloctest` | — | 足場 | 削除 |  | PSRAMヒープ確保の検証。`mix`と起動時のヒープ確立が同じことを見ている |
| `append` | — | 製品 | **残す** |  | ファイル追記 |
| `automount` | — | 製品 | **残す** |  | USBの自動マウント切り替え |
| `axistest` | — | 足場 | feature |  | センサー・入力の実証。`INPUT_MANAGER_PLAN`／`USB_HOST_PLAN`完了 |
| `backlight` | — | 製品 | **残す** |  | バックライト |
| `battery` | `batinfo` | 製品 | 廃止済み |  | Battery detailsへ統合 |
| `blkread` | — | 足場 | feature |  | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `browser` | — | 製品 | **残す** |  | ハイパーテキストビューア |
| `bt` | `browsertest` | 足場 | feature |  | ブラウザ取得経路。`WEB_BROWSER_PLAN`／`BROWSER_UI_PLAN`完了 |
| `cat` | — | 製品 | **残す** |  | ファイル表示 |
| `cd` | — | 製品 | **残す** |  | カレントディレクトリ移動 |
| `clear` | — | 製品 | **残す** |  | シェルの基本 |
| `coordtest` | — | 足場 | 削除 |  | CW回転とクリッピングの目視確認。`GRAPHICS.md`へ同期済み |
| `cpuinfo` | — | 足場 | 削除 |  | コア識別CSRの立ち上げ確認。`BOOT.md`へ同期済み |
| `db` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `devices` | — | 製品 | **残す** |  | ブロックデバイス一覧 |
| `di` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `displaybench` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `dp` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `echo` | — | 製品 | **残す** |  | シェルの基本 |
| `entropy` | — | 足場 | 削除 |  | TLSのCSPRNG導入時の検証。`TLS_PLAN`完了 |
| `fill` | — | 足場 | feature |  | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `fonttest` | — | 足場 | **残す** |  | `SCALABLE_PROPORTIONAL_FONT_PLAN`が全Stage未着手。Stage 3が`fonttest`での実機比較を要求 |
| `fsclose` | — | 足場 | feature |  | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `fsopen` | — | 足場 | feature |  | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `fsread` | — | 足場 | feature |  | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `fsverify` | — | 足場 | feature |  | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `fswritetest` | — | 足場 | feature | ⚠ | ファイルシステム。`FILESYSTEM_*_PLAN`完了 |
| `help` | — | 製品 | **残す** |  | シェルの基本 |
| `hs` | `httpstream` | 足場 | feature |  | ブラウザ取得経路。`WEB_BROWSER_PLAN`／`BROWSER_UI_PLAN`完了 |
| `httpget` | — | 足場 | feature |  | ネットワーク。`TCPIP_PLAN`／`DNS_PLAN`／`TLS_PLAN`完了 |
| `icm` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `ipconfig` | — | 製品 | **残す** |  | IPv4設定 |
| `ls` | — | 製品 | **残す** |  | ディレクトリ一覧 |
| `lsusb` | — | 製品 | **残す** |  | USB接続機器のツリー表示 |
| `mem` | — | 製品 | **残す** |  | PSRAM／RAM使用量 |
| `membench` | — | 足場 | 削除 |  | 帯域実測。数値は`DISPLAY_BANDWIDTH.md`／`PSRAM.md`にある |
| `mix` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `mkdir` | — | 製品 | **残す** |  | ディレクトリ作成 |
| `mount` | — | 製品 | **残す** |  | ボリュームの接続 |
| `mounts` | — | 製品 | **残す** |  | マウント一覧 |
| `mv` | — | 製品 | **残す** | ⚠ | 改名・移動 |
| `netdump` | — | 足場 | feature |  | ネットワーク。`TCPIP_PLAN`／`DNS_PLAN`／`TLS_PLAN`完了 |
| `nslookup` | — | 製品 | **残す** |  | 名前解決 |
| `paint` | — | 製品 | **残す** |  | タッチで描くアプリ |
| `pf` | — | 足場 | feature |  | PSRAMプロファイルと再起動。`FLASH_XIP_MIGRATION_PLAN`完了 |
| `ping` | — | 製品 | **残す** |  | 疎通確認 |
| `pma` | — | 足場 | 削除 |  | 物理メモリ属性の立ち上げ確認。全16エントリがロック済みで読む以外にできることがない |
| `pmp` | — | 足場 | 削除 |  | 物理メモリ保護の立ち上げ確認。同上 |
| `ppafill` | — | 足場 | 削除 |  | PPAとCPUの損益分岐点。閾値は`DISPLAY_BANDWIDTH.md`にある |
| `pwd` | — | 製品 | **残す** |  | カレントディレクトリ表示 |
| `reboot` | `reset` | 製品 | **残す** |  | 再起動 |
| `rm` | — | 製品 | **残す** | ⚠ | ファイル削除 |
| `rmdir` | — | 製品 | **残す** | ⚠ | 空ディレクトリ削除 |
| `rt` | — | 足場 | feature |  | PSRAMプロファイルと再起動。`FLASH_XIP_MIGRATION_PLAN`完了 |
| `rtc` | — | 製品 | **残す** |  | 時計の表示と`rtc set` |
| `sdinfo` | — | 足場 | feature |  | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `sdmbr` | — | 足場 | feature |  | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `sdread` | — | 足場 | feature |  | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `sdreadn` | — | 足場 | feature |  | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `sdreadpsram` | — | 足場 | 削除 |  | SD→PSRAMのDMA検証。実運用の経路がブロック層で同じことを通る |
| `sdwritetest` | — | 足場 | feature | ⚠ | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `sdzero` | — | 足場 | feature | ⚠ | ストレージ低レベルI/O。`FILESYSTEM_PLAN`／`SD_CARD_PLAN`完了 |
| `shutdown` | `poweroff` | 製品 | **残す** |  | 電源断 |
| `stress` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `tftpget` | — | 製品 | **残す** |  | TFTP取得 |
| `tls` | — | 足場 | feature |  | ネットワーク。`TCPIP_PLAN`／`DNS_PLAN`／`TLS_PLAN`完了 |
| `touchtest` | — | 足場 | feature |  | センサー・入力の実証。`INPUT_MANAGER_PLAN`／`USB_HOST_PLAN`完了 |
| `touchcheck` | — | 足場 | **残す** |  | 通常GUIのtap／hold／drag受入が実機未確認 |
| `ui` | — | 足場 | feature |  | 表示帯域とアンダーラン。`DISPLAY_UNDERRUN_REFACTOR_PLAN`完了 |
| `umount` | — | 製品 | **残す** |  | ボリュームの切断 |
| `uptime` | — | 製品 | **残す** |  | 起動からの経過時間 |
| `usbcachefail` | — | 足場 | feature |  | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbcheck` | — | 足場 | feature |  | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbfs` | — | 足場 | feature |  | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbhub` | — | 足場 | feature |  | USB割り込み駆動。`USB_INTERRUPT_REFACTOR_PLAN`完了 |
| `usbhw` | — | 足場 | feature |  | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbinfo` | — | 製品 | **残す** |  | USB接続機器一覧。`lsusb`と重複、片方に寄せる余地あり |
| `usbmargin` | — | 足場 | feature |  | USB読み出しと起動マージン。`USB_MSC_PLAN`／`USB_MSC_BOOT_MARGIN_PLAN`／`USB_HOST_PLAN`完了 |
| `usbmbr` | — | 足場 | feature |  | USB読み出しと起動マージン。`USB_MSC_PLAN`／`USB_MSC_BOOT_MARGIN_PLAN`／`USB_HOST_PLAN`完了 |
| `usbmsc` | — | 足場 | feature |  | USB読み出しと起動マージン。`USB_MSC_PLAN`／`USB_MSC_BOOT_MARGIN_PLAN`／`USB_HOST_PLAN`完了 |
| `usbmultiwrite` | — | 足場 | feature | ⚠ | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbperiodic` | — | 足場 | feature |  | USB割り込み駆動。`USB_INTERRUPT_REFACTOR_PLAN`完了 |
| `usbrawcheck` | — | 足場 | feature | ⚠ | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbread` | — | 足場 | feature |  | USB読み出しと起動マージン。`USB_MSC_PLAN`／`USB_MSC_BOOT_MARGIN_PLAN`／`USB_HOST_PLAN`完了 |
| `usbrescan` | — | 製品 | **残す** |  | USB再スキャン |
| `usbvbus` | — | 足場 | feature |  | USB読み出しと起動マージン。`USB_MSC_PLAN`／`USB_MSC_BOOT_MARGIN_PLAN`／`USB_HOST_PLAN`完了 |
| `usbwritetest` | — | 足場 | feature | ⚠ | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `usbzero` | — | 足場 | feature | ⚠ | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `ut` | — | 足場 | feature |  | USB書き込みとBOT/HCD受入。`USB_BOT_HCD_REFACTOR_PLAN`／`USB_WRITE_STABILITY_PLAN`完了 |
| `wifi` | — | 製品 | **残す** |  | サブコマンドの入口 |
| `wifi connect` | — | 製品 | **残す** |  | AP接続 |
| `wifi disconnect` | — | 製品 | **残す** |  | 切断 |
| `wifi forget` | — | 製品 | **残す** |  | 保存プロファイル削除 |
| `wifi info` | — | 足場 | 削除 |  | C6をSDIOカードとして立ち上げる段階の名残。`wifi`が内部で通る |
| `wifi log` | — | 足場 | feature |  | Wi-Fi。`WIFI_C6_PLAN`／`WIFI_REFACTOR_PLAN`完了 |
| `wifi mac` | — | 足場 | feature |  | Wi-Fi。`WIFI_C6_PLAN`／`WIFI_REFACTOR_PLAN`完了 |
| `wifi saved` | — | 足場 | feature |  | Wi-Fi。`WIFI_C6_PLAN`／`WIFI_REFACTOR_PLAN`完了 |
| `wifi scan` | — | 製品 | **残す** |  | APスキャン |
| `wifi status` | — | 製品 | **残す** |  | 接続状態 |
| `wifi up` | — | 足場 | 削除 |  | 同上 |
| `win` | — | 製品 | **残す** |  | 通常GUIデスクトップへの入口 |
| `write` | — | 製品 | **残す** | ⚠ | ファイル作成・置換 |

## 別名

`HELP_ENTRIES`の`aliases`に持たせてあり、`help <本体名>`が`also:`行で出します。
以前はこの6つが`execute`の`match`にしかなく、`help`から辿れませんでした。名前解決を
表へ一本化した経緯は[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)にあります。

## 名前空間について

`db` `dp` `di` `ui` `ut` `pf` `rt` `bt` `hs` `mix`のような打ちやすい2文字名は、すべて
足場に割り当たっています。足場を退役させるとこれらは空きます。製品側で短い名前が
欲しくなったときの原資です。
