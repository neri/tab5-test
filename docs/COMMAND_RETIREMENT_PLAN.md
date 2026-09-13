# コンソールコマンドの退役計画

> 索引: [`../DESIGN.md`](../DESIGN.md)

## 状態: 未着手（案のみ。削除は1件も実施していない）

足場コマンド62個（[`CONSOLE_COMMAND_REVIEW.md`](CONSOLE_COMMAND_REVIEW.md)）を、
**いま消す（11）／featureで落とす（50）／まだ残す（1）**へ振り分ける案です。
これに製品側の重複1件（`usbinfo`）が加わります。

計画の状態は2026-09-04時点で取り直しました。前版（2026-08-30）からの主な変化は
[改訂履歴](#改訂履歴)にあります。この文書の
判断はまだ人が承認していません。実装前に承認を取ります。

## 前提

足場は「実装が安定するまでの診断用」で、安定すれば大半は不要になります。
エンドユーザーに向けては最初から実用性がありません。したがって既定は
**削除**で、残すほうに理由が要ります。

残す理由になり得るのは次の2つだけとします。

1. **紐づく作業がまだ閉じていない**——`docs/*_PLAN.md`に未完了Stageがある
2. **現状文書が受入の根拠として名指ししている**——消すとその根拠を再取得する
   手段が無くなる

2について具体例を挙げると、[`USB.md`](USB.md)はBOT再同期という緩和策の根拠として
「High-Speed直結の`ut 100`は予防再同期6回、retry 0で100/100を完走」と書いています。
`ut`を消すと、この緩和策を次に触ったとき同じ検証ができません。緩和策が残る限り
`ut`も残ります。

## 案A: いま削除する（足場11件）

紐づく作業が閉じており、かつ現状文書が受入の根拠として使っていないものです。

| コマンド | 削除の理由 |
| --- | --- |
| `wifiinfo` `wifiup` | C6をSDIOカードとして立ち上げる段階の名残。`wifi`が内部で同じ経路を通り、失敗は`wifilog`に出る。[`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md)は全Stage完了 |
| `cpuinfo` `pma` `pmp` | メモリマップの立ち上げ確認。結果は[`BOOT.md`](BOOT.md)・[`PSRAM.md`](PSRAM.md)へ同期済みで、PMAテーブルは全エントリがロック済みのため読む以外にできることがない |
| `alloctest` | PSRAMヒープの確保検証。[`FLASH_XIP_MIGRATION_PLAN.md`](FLASH_XIP_MIGRATION_PLAN.md)完了後は`mix`と起動時のヒープ確立が同じことを見ている |
| `sdreadpsram` | SD→PSRAMのDMA検証。ブロック層とFAT読み出しが実運用で同じ経路を通る |
| `ppafill` | PPAとCPUの損益分岐点を出すために書いた。閾値は[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)にあり、[`PPA_FILL_PLAN.md`](PPA_FILL_PLAN.md)は完了 |
| `membench` | 同上。実測値は[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)・[`PSRAM.md`](PSRAM.md)にある |
| `coordtest` | CW回転とクリッピングの目視確認。[`GRAPHICS.md`](GRAPHICS.md)へ同期済みで、座標系はその後変わっていない |
| `entropy` | `test`／`fail`はTLSのCSPRNG導入時の検証。[`TLS_PLAN.md`](TLS_PLAN.md)はStage 8まで完了 |

`membench`と`ppafill`は判断が割れ得ます。**表示帯域やPPA閾値を将来もう一度
触るなら、測り直す手段が消えます**。残す判断も妥当です。

### 別件: `usbinfo`（製品側の重複）

`usbinfo`は足場ではなく製品に分類してあります（`lsusb`と同じく、何が挿さって
いるかを見るコマンドだからです）。ただし出力は`lsusb`（引数なし）とほぼ同じで、
**退役ではなく重複解消として消せます**。案Aと同時に処理できますが、判断の
理由が違うので分けてあります。消す場合は`HELP_ENTRIES`から項目を外すだけで、
製品の機能は減りません。

## 案B: featureで落とす（50件）

作業は閉じているが、現状文書が受入の根拠として名指ししているか、実機の回帰確認に
使う見込みがあるものです。`#[cfg(feature = "diag")]`で落とし、ソースには残します。

| 区分 | コマンド |
| --- | --- |
| 表示帯域とアンダーラン | `stress` `displaybench` `db` `dp` `di` `icm` `ui` `mix` |
| PSRAMプロファイルと再起動 | `pf` `rt` |
| USB読み出しと起動マージン | `usbmargin` `usbread` `usbmsc` `usbmbr` `usbvbus` |
| USB書き込みとBOT/HCD受入 | `usbcheck` `usbrawcheck` `usbcachefail` `usbmultiwrite` `usbwritetest` `usbzero` `ut` `usbfs` `usbhw` |
| USB割り込み駆動 | `usbperiodic` `usbhub` |
| ストレージ低レベル | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdwritetest` `sdzero` `blkread` |
| ファイルシステム | `fsopen` `fsread` `fsclose` `fsverify` `fill` `fswritetest` |
| ネットワーク | `netdump` `httpget` `tls` |
| ブラウザ取得経路 | `bt` `hs` |
| センサー・入力 | `axistest` `touchtest` `win` |
| Wi-Fi | `wifimac` `wifisaved` `wifilog` |

USB書き込みの9個は、[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)が
完了した受入harnessです。**削除ではなくfeature化**にしたのは、6構成の実機matrixと
故障注入という受入の根拠が、この9個でしか再取得できないためです。転送層に手を
入れるたびに通す試験なので、ソースには残します。

featureは`diag`1つで足ります。**恒久的な構造ではなく、削除できるようになるまでの
待避所**です。`default`に入れて`cargo build --release`は従来どおり全部入りにし、
絞った像は`--no-default-features`で作ります。

gate した側は流さないと必ずコンパイルが通らなくなるので、`mise.toml`にタスクを
足して定期的に通します。

```sh
cargo build --release --no-default-features
```

## 案C: まだ残す（1件）

紐づく作業に未完了Stageがあるものです。作業が閉じたら案A／案Bへ移します。

| コマンド | 生かしている作業 |
| --- | --- |
| `fonttest` | [`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md) 未着手。Stage 3が`fonttest`での実機比較を要求している |

`ui`は[`STARTUP_SCREEN_REFACTOR_PLAN.md`](STARTUP_SCREEN_REFACTOR_PLAN.md)が
完了したため、案Bのままで問題ありません（もともと視認は`ui`を通さずにもできる
という理由で案Bに置いていました）。

## 段階

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 案A〜Cの振り分けを人が承認する | 未着手 |
| 1 | 案Aの11件（＋`usbinfo`）を削除。`HELP_ENTRIES`の項目、`Cmd`のvariant、`match`のarm、`cmd_*`関数、`Outcome`のvariantと`src/app.rs`側の分岐、不要になった`src/app/*.rs` | 未着手 |
| 2 | 現状文書から削除したコマンドの記述を外す | 未着手 |
| 3 | `diag` featureを追加し、案Bの50件に`#[cfg]`を付ける。`mise.toml`に`--no-default-features`のタスクを足す | 未着手 |
| 4 | `--no-default-features`の実機起動を確認する | 未着手 |

Stage 1の消し忘れは全部コンパイル時に出ます（[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)の
「コマンド表が唯一の出典」）。

## README.md への影響（人の判断が要る）

> 2026-09-13: この節の第一候補どおり、`README.md`のコマンド表は人の承認を得て削除し、
> `help`と`CONSOLE_COMMAND_REVIEW.md`への案内に置き換えた。以下の行対応表は削除前の記録。

`README.md`は人がメンテする文書なので、この計画では編集しません。ただし
**案Aを実施すると「シェルコマンド」節の表が実態と合わなくなります**。
影響する行は次のとおりです。

| README の行 | 実施後の状態 |
| --- | --- |
| CPU（`cpuinfo`） | 行ごと消える |
| メモリ（`mem` `alloctest` `membench`） | `mem`だけが残る |
| 表示・DMA調停（`backlight` `stress` `icm` `ppafill`） | `backlight`だけが製品、残り3つは案A／案B |
| 画面の座標確認（`coordtest`） | 行ごと消える |
| SDカード（7個） | `sdreadpsram`が消え、残りは案B |
| USB-A（`usbinfo` `usbrescan` `usbhub` `usbhw` `usbvbus`） | `usbrescan`だけが製品 |
| Wi-Fi（`wifiinfo` `wifiup`を含む） | 2つ消える |

第一候補として提案するのは、**表を丸ごと削って`docs/`へ委ねる**ことです。
コマンド一覧が2箇所にあると必ず片方が古くなりますし、実機の`help`が常に正しい
一覧を持っています。`README.md`には「`help`でコマンド一覧を表示します」の1行と、
[`CONSOLE_COMMAND_REVIEW.md`](CONSOLE_COMMAND_REVIEW.md)へのリンクだけを残す形です。
採否は人が決めます。

なお、READMEの末尾にある破壊的コマンドの注意書き（`sdzero`／`usbzero`／
`sdwritetest`／`usbwritetest`）は、案Bで4つとも`--no-default-features`の像から
消えるため、条件付きの記述になります。

## 改訂履歴

### 2026-09-04b（この版）: 古い状態行に基づく誤分類の修正

`USB_WRITE_STABILITY_PLAN.md`の状態行「複数ブロックWRITE(10)は未解決」は
**更新漏れ**で、実際は解決済みでした。`src/fs/usb_msc.rs`の`MAX_WRITE_BLOCKS`は
回避策の`1`から`8`になっており、[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)が
「Full-Speed固定ハブ経路の旧故障は両媒体で解消した」「Stage 7を完了とする」
「2媒体×2 topologyで2／4／8 block各10回PASS」と記録しています。同計画の本文は
第29版で止まっており、作業はBOT/HCD計画側でv42まで続きました。

9月4日a版はこの古い状態行を根拠にUSB書き込み系を案Cへ移していたので、取り消します。

| 修正 | 影響 |
| --- | --- |
| USB書き込み系を案C→**案B** | `usbcheck` `usbrawcheck` `usbcachefail` `usbmultiwrite` `usbwritetest` `usbzero` `ut` `usbfs` `usbhw` の9個 |
| `fill`を案C→**案B** | FS書き込みの残件はSDの`fswritetest`とPC照合で、`fill`は関わらない |
| `fswritetest`の理由を差し替え | 「transport recovery継続調査中」ではなく「**SDが未実施**」。USB側は12検査PASS済み |

**案A（11件）は3版を通じて変わっていません。**

規則を1つ足します——**状態行はコードと突き合わせる**。計画文書の状態行は人が
手で更新するので実装に遅れます。今回は`MAX_WRITE_BLOCKS`の値1つで判別できました。

### 2026-09-04a

> **この版のUSB関連の振り分けは9月4日b版で取り消されました。**
> 根拠にした`USB_WRITE_STABILITY_PLAN.md`の状態行が古かったためです。
> 以下は当時の記録として残します。

コマンドは98→103個に増え、足場は57→62個になりました（`fswritetest` `usbcheck`
`usbrawcheck` `usbmultiwrite` `usbcachefail`。すべて`Group::Scaffold`で追加済み）。
計画の状態を取り直した結果、振り分けが次のように動いています。

| 変化 | 影響 |
| --- | --- |
| [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md) が**完了→複数ブロックWRITE(10)未解決**へ | `usbwritetest` `usbzero` `ut` `usbfs` `usbhw` が案B→**案C**。新しい5コマンドのうち4つも案C |
| [`FILESYSTEM_WRITE_REFACTOR_PLAN.md`](FILESYSTEM_WRITE_REFACTOR_PLAN.md) が新設（継続調査中） | `fswritetest`が案C、`fill`が案B→**案C** |
| [`WIFI_REFACTOR_PLAN.md`](WIFI_REFACTOR_PLAN.md) にStage 8（未着手）が追加 | `wifilog`は案Cのまま。理由が「調査に現役」から計画の未着手Stageへ変わり、根拠が強くなった |
| [`BROWSER_UI_PLAN.md`](BROWSER_UI_PLAN.md) が実機受入待ち→**完了** | `bt` `hs` が案C→**案B** |
| [`STARTUP_SCREEN_REFACTOR_PLAN.md`](STARTUP_SCREEN_REFACTOR_PLAN.md) が視認待ち→**完了** | `ui`は案Bのまま（根拠が確定） |
| [`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md) 完了の再確認 | `wifimac` `wifisaved` が案C→**案B**。「残す理由が弱い」としていたものを規則どおり動かした |
| [`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md) が中断→**凍結** | 凍結は再開予定が無いので、単独では足場を生かさないものとした。`usbfs`はWRITE調査の側で案C |
| [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)／[`USB_MSC_BOOT_MARGIN_PLAN.md`](USB_MSC_BOOT_MARGIN_PLAN.md)／[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md) に完了の状態行が付いた | 案Bの根拠が明示的になった |

**案Aの11件は前版から変わっていません。**

規則そのものも1つ足しました——紐づけ先は**道具を実装した計画ではなく、
道具が向けられた問いを持つ計画**です。新しい4コマンドを実装したのは完了済みの
[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)ですが、
それらが答えようとしている問いは未解決のまま残っています。

### 2026-08-30（初版）

足場57個を11／37／9へ振り分け。

### 2026-09-04c: 3計画をクローズ

実機確認が完了しているとの確認を受け、状態行を実態へ合わせました。

| 計画 | 変更 |
| --- | --- |
| [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md) | 「複数ブロックWRITE(10)は未解決」→ **完了**（機能受入）。根本原因は未特定のまま緩和策で封じ込め、と明記。同計画自身が「機能受入の残作業ではない」と書いていた区別をそのまま状態行へ上げたもの |
| [`FILESYSTEM_WRITE_REFACTOR_PLAN.md`](FILESYSTEM_WRITE_REFACTOR_PLAN.md) | 「USBの頻発するtransport recoveryは継続調査中」→ 解消済み。**残るのは再起動を挟んだ読み直し1件** |

[`FILESYSTEM_WRITE_REFACTOR_PLAN.md`](FILESYSTEM_WRITE_REFACTOR_PLAN.md)も、
残っていた「再起動を挟んだ読み直し」の実施確認を受けて**完了**にしました。
完了条件「実機で書いた媒体をPCと再起動後のTab5の両方から読める」がこれで満たされます。
`fswritetest`は案C→**案B**へ移り、振り分けは**11／47／4**になりました。

案Cに残るのは`fonttest`（フォント計画が未着手）、`usbperiodic`／`usbhub`
（hub statusが低頻度fallbackのまま）、`wifilog`（Wi-Fi計画のStage 8が未着手）の
4個だけです。

**未処理の引き継ぎ**: `FILESYSTEM_WRITE_REFACTOR_PLAN.md`が記録している
`README.md`の不一致3箇所は、同計画のクローズ後も判断待ちとして残ります
（下の「README.md への影響」と同じ扱い）。

### 2026-09-04d: Stage Gのスコープ外判定と状態行の書式統一

| 変更 | 内容 |
| --- | --- |
| [`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) | Stage G（多段ハブ）を**スコープ外**と判定し、計画を完了に。Stage Gの節も「将来・可能であれば…未着手」から「スコープ外」へ |
| 状態行の書式統一 | `FILESYSTEM_PLAN` `FILESYSTEM_WORKFLOW_PLAN` `ROOT_FILESYSTEM_PLAN` `FILESYSTEM_WRITE_REFACTOR_PLAN` の4つが`## 状態`だけで値が次行にあり、機械的な棚卸しから漏れていた。全28計画を`## 状態: <値>`へ揃えた |

書式統一の過程で、[`FILESYSTEM_PLAN.md`](FILESYSTEM_PLAN.md)のStage 5（exFAT）に
未検証の完了条件が残っていることが分かりました。値が次行にあったため、これまでの
棚卸しでは「状態行なし」として素通りしていました。書式を揃える目的そのものが、
この見落としです。→ 2026-09-04eで処理。

足場の振り分けは変わりません（11／47／4）。

### 2026-09-04e: exFATの未検証項目を完了条件から外す

[`FILESYSTEM_PLAN.md`](FILESYSTEM_PLAN.md)のStage 5に残っていた未検証の完了条件のうち、
exFATの**Unicode名の読み出し**と**大きな連続ファイルの読み出し**を完了条件から外し、
メモへ格下げしました（利用者判断）。理由は、判定がライブラリ側の実装に依存するため
問題が出ても対処できないこと、そして該当する媒体を用意するのが難しいことです。
これにより同計画は**完了**になりました。

壊れたboot regionの拒否は、もともとStage 0から別タスクへ保留されているホストテストの
一部なので、そちらへ残しています。

足場の振り分けは変わりません（11／47／4）。exFATの確認は`ls`／`cat`／`mounts`という
製品コマンドで行うため、足場を生かしていませんでした。

### 2026-09-04f: USB割り込みとWi-Fiの2計画をクローズ

現行実装で実機に大きな問題が出ていないため、残作業を実施しない判断を受けました。
問題が出た時点で新しい計画を起票します。

| 計画 | 実施しないことにした残作業 |
| --- | --- |
| [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md) | hub statusを割り込み駆動へ移す件。低頻度fallbackのまま運用する |
| [`WIFI_REFACTOR_PLAN.md`](WIFI_REFACTOR_PLAN.md) | Stage 8（長時間・異常系の実機受入）。現状文書は各Stage実装時に更新済み |

これで`usbperiodic` `usbhub` `wifilog`の3個が案C→**案B**へ移り、振り分けは
**11／50／1**になりました。

**案Cに残るのは`fonttest`だけです。**
[`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md)が全Stage未着手で、
Stage 3が`fonttest`での実機比較を要求しているためです。
