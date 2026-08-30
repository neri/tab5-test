# コンソールコマンドの退役計画

> 索引: [`../DESIGN.md`](../DESIGN.md)

## 状態: 未着手（案のみ。削除は1件も実施していない）

足場コマンド57個（[`CONSOLE_COMMAND_REVIEW.md`](CONSOLE_COMMAND_REVIEW.md)）を、
**いま消す（11）／featureで落とす（37）／まだ残す（9）**へ振り分ける案です。
これに製品側の重複1件（`usbinfo`）が加わります。この文書の
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

## 案B: featureで落とす（37件）

作業は閉じているが、現状文書が受入の根拠として名指ししているか、実機の回帰確認に
使う見込みがあるものです。`#[cfg(feature = "diag")]`で落とし、ソースには残します。

| 区分 | コマンド |
| --- | --- |
| 表示帯域とアンダーラン | `stress` `displaybench` `db` `dp` `di` `icm` `ui` `mix` |
| PSRAMプロファイルと再起動 | `pf` `rt` |
| USBマスストレージ | `ut` `usbmargin` `usbread` `usbwritetest` `usbzero` `usbmsc` `usbmbr` `usbhw` `usbvbus` |
| ストレージ低レベル | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdwritetest` `sdzero` `blkread` |
| ファイルハンドル寿命 | `fsopen` `fsread` `fsclose` `fsverify` `fill` |
| ネットワーク | `netdump` `httpget` `tls` |
| センサー・入力 | `axistest` `touchtest` `win` |

featureは`diag`1つで足ります。**恒久的な構造ではなく、削除できるようになるまでの
待避所**です。`default`に入れて`cargo build --release`は従来どおり全部入りにし、
絞った像は`--no-default-features`で作ります。

gate した側は流さないと必ずコンパイルが通らなくなるので、`mise.toml`にタスクを
足して定期的に通します。

```sh
cargo build --release --no-default-features
```

## 案C: まだ残す（9件）

紐づく作業に未完了Stageがあるものです。作業が閉じたら案A／案Bへ移します。

| コマンド | 生かしている作業 |
| --- | --- |
| `fonttest` | [`SCALABLE_PROPORTIONAL_FONT_PLAN.md`](SCALABLE_PROPORTIONAL_FONT_PLAN.md) 未着手。Stage 3が`fonttest`での実機比較を要求している |
| `usbperiodic` `usbhub` | [`USB_INTERRUPT_REFACTOR_PLAN.md`](USB_INTERRUPT_REFACTOR_PLAN.md) hub statusは低頻度fallbackを維持中 |
| `usbfs` | [`USB_FLOPPY_PLAN.md`](USB_FLOPPY_PLAN.md) 凍結（Stage 2 CBI ADSC未解決、再開予定なし） |
| `wifimac` `wifisaved` `wifilog` | `wifilog`は[`WIFI_REFACTOR_PLAN.md`](WIFI_REFACTOR_PLAN.md) Stage 8（長時間・異常系の実機受入）が未着手で、再接続リトライの調査に現役。他2つは`wifi`画面から辿れず、残す理由は弱い |

**振り分けの見直しが要る2件（人の判断待ち）**: `bt`／`hs`を案Cに置いた理由は
[`BROWSER_UI_PLAN.md`](BROWSER_UI_PLAN.md) Stage 10の実機受入待ちでしたが、同計画は
Stage 0〜10とも実機確認済みで閉じました。案Cの条件（紐づく作業に未完了Stageがある）を
満たさなくなったので、案Aか案Bへ移せます。`browsertest`のfixture巡回という受入根拠が
[`BROWSER.md`](BROWSER.md)にあるため、案B（`diag` featureで落とす）が素直です。

`usbfs`も凍結であって未完了Stageが動く見込みは無いため、案Cに置き続ける根拠は
「いつか再開するかもしれない」だけです。削除して差し支えないかは人が決めてください。

`ui`は[`STARTUP_SCREEN_REFACTOR_PLAN.md`](STARTUP_SCREEN_REFACTOR_PLAN.md)が
完了したため、案Bのままで問題ありません（もともと視認は`ui`を通さずにもできる
という理由で案Bに置いていました）。

## 段階

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | 案A〜Cの振り分けを人が承認する | 未着手 |
| 1 | 案Aの11件（＋`usbinfo`）を削除。`HELP_ENTRIES`の項目、`Cmd`のvariant、`match`のarm、`cmd_*`関数、`Outcome`のvariantと`src/app.rs`側の分岐、不要になった`src/app/*.rs` | 未着手 |
| 2 | 現状文書から削除したコマンドの記述を外す | 未着手 |
| 3 | `diag` featureを追加し、案Bの35件に`#[cfg]`を付ける。`mise.toml`に`--no-default-features`のタスクを足す | 未着手 |
| 4 | `--no-default-features`の実機起動を確認する | 未着手 |

Stage 1の消し忘れは全部コンパイル時に出ます（[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)の
「コマンド表が唯一の出典」）。

## README.md への影響（人の判断が要る）

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
