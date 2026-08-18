# 診断ログ

> 索引: [`../DESIGN.md`](../DESIGN.md)

正常時の主要な通過点は次のとおりです。

```text
RAM: L2 cache bytes=0x...
RAM: usable top=0x...
RAM: stack top=0x...
XIP: pre-PSRAM DROM probe start
XIP: pre-PSRAM IROM probe start
XIP: pre-PSRAM DROM+IROM ok
DMA2D: version=0x02304110
PPA: version=0x02304041
PPA: clocked, out of reset, registers verified
PSRAM: ready (framebuffer + heap)
XIP: post-PSRAM DROM probe start
XIP: post-PSRAM IROM probe start
XIP: post-PSRAM DROM+IROM ok
LCD: D-PHY 4/4 ready
LCD: DCS init complete
ICM: clk_en=0x...
ICM: master priority=0x...
ICM: master arqos=0x...
ICM: master awqos=0x...
LCD: DMA 3/3 full-frame interrupt installed
LCD: RGB565 framebuffer DMA active
```

`ICM: master priority`と`ICM: master arqos`は、DW-GDMAの2ポート分
（bit 12〜19）が`F`になっていれば書き込みが効いています。ここが`0`のままなら、
レジスタ自体が書けていません。`ICM: master awqos`は書き込み側の診断値です。
表示DMAはPSRAMを読み出すだけなので、DW-GDMAのbit 12〜19を変更しません。
`LCD: DPI FIFO underrun ...`が出た場合は、そのフレームのパネル表示が水色に
なっています（[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)を参照）。

SDカード関連は起動シーケンスに含まれず、シェルコマンド（`sdinfo`/`sdread`/
`sdreadn`/`sdwritetest`/`sdzero`）実行時にのみ`SDMMC: ...`という接頭辞で
UARTへ出ます。正常時は`SDMMC: card activated`の後にCID/CSDの生値が続きます。
対応範囲は[`STORAGE.md`](STORAGE.md)、失敗パターンの詳細は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照してください。

USB-AホストはLCDとCardKBの初期化後に起動し、最初の`UsbHost::rescan`を実行します。
そのため、起動時にも列挙結果や`USB: initial scan complete`がUARTへ出ます。その後も、
ルートポートの切断・再接続、空いているハブポートの増分スキャン、トランザクションエラーからの
復帰時に`USB: ...`ログが出ます。`usbinfo`/`usbhub`/`usbmsc`等は共有レジストリを使い、
`usbrescan`だけがユーザー操作でバスの再列挙を行います。`usbvbus`はI/O expanderの出力ビットを
直接変更する診断用コマンドです。対応範囲は[`USB.md`](USB.md)、段階分けと未確定事項は
[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)を参照してください。

`battery`実行時は、検出したINA226のI2Cアドレスを`Battery: INA226 found at I2C address=0x...`
として出力します。初期化できない場合は`Battery: INA226 identity read failed`または
`Battery: INA226 configuration write failed`、動作中の一時読出し失敗は
`Battery: INA226 read failed; retaining last reading`を出力します。

主な失敗ログ:

- `CPU: unexpected boot clock source, staying at 90 MHz`: ブートローダーがCPLL/4以外の経路でCPUを構成した（分周比を書き換えず90 MHzのまま継続）
- `RAM: stack top is inside the L2 cache area`: `memory.x`の`RAM`範囲が広すぎる
- `MEM: .data/.bss initialization failed`: bootloaderのRAMロードまたはゼロ初期化が不正
- pre側の`XIP: DROM probe failed`／`XIP: IROM probe actual=...`: bootloaderによる初期FLASHマッピングが不正
- post側の同ログ、またはpost probe途中の停止: PSRAM/MSPI初期化後にFLASHデータまたは命令取得へ復帰できない
- `PSRAM: mode-register transaction failed`: MSPI3コマンド経路
- `PSRAM: no valid DQS phase`: DQS位相調整
- `PSRAM: mapped memory test failed`: MMUまたはキャッシュ経路
- `LCD: PI4IOE1 reset control failed`: ソフトウェアI2CまたはI/O expander
- `LCD: D-PHY lock timeout`: D-PHY電源、クロック、PLL
- `LCD: DCS FIFO timeout`: パネルコマンド経路
- `LCD: DMA interrupt error`: DW-GDMA転送
