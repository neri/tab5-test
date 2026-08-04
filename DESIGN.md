# 設計資料

## 対象と方針

このプロジェクトはM5Stack Tab5のESP32-P4 ECO2（chip revision v1.3）を対象に
しています。ESP-IDFやRTOSをリンクせず、`riscv-rt`とレジスタ操作だけで起動、
PSRAM、MIPI-DSI、GDMAを初期化します。

実機で確認した構成は次のとおりです。

- ESP32-P4 ECO2、eFuse block revision v0.3
- 16 MiB SPI Flash
- Hex-DDR PSRAM
- ネイティブ走査720×1280のMIPI-DSI LCD
- USB Serial/JTAG

## 起動とイメージ配置

現行のESP32-P4向けHALにはECO5以降を前提とする初期化が含まれるため、汎用の
`riscv-rt`を使用しています。起動直後に`startup.rs`がブートローダーから継承した
RTC watchdogを停止します。

ESP-IDF v5.5の2nd-stage bootloaderが読み込めるよう、`memory.x`では次の配置を
定義しています。

- `0x40000020`: アプリケーション記述子と読み取り専用データ
- `0x40001000`: 実行コード
- `0x4ff40000`: データ、BSS、スタック

アプリケーション記述子は`src/main.rs`の`EspAppDesc`です。空のRAMロード
セグメントにならないよう、`.data`に`BOOT_LAYOUT_MARKER`も配置しています。

## 起動シーケンス

```text
riscv-rt
  → RTC watchdog停止
  → USB Serial/JTAG初期化
  → PSRAM電源・クロック・DQS調整・MMU割り当て
  → 2面のRGB565画像を描画してキャッシュを同期・検証
  → LCDリセット・D-PHY・パネル初期化
  → DSI BridgeとDW-GDMAを準備してvideo modeを開始
  → GDMA完了割り込みで次フレームを再投入
```

正常経路ではDSI HostのVideo Pattern Generatorを使用しません。ECO2では動作中の
VPGからBridge入力へ切り替えると、設定値、FIFO量、underrun状態が同一でも稀に黒画面に
なることを実機で確認したためです。最初のDMAデータをFIFOへ充填してからHostのvideo
modeとBridge出力を初めて有効化します。VPGのカラーバーはPSRAMまたはDMA経路が失敗
した場合の独立した診断表示としてだけ使用します。

## PSRAM

`src/psram.rs`が次の処理を担当します。

1. LDO2を1.8 Vに設定し、MSPI PHYの電源とクロックを有効化
2. 480 MHz SPLLを6分周し、PSRAMを80 MHzで駆動
3. MSPI3経由でモードレジスタを読み書き
4. コマンド経路の読み書き試験
5. DQS位相とdata/DQS delayの実機調整
6. 先頭4 MiBを`0x48000000`へMMU割り当て
7. キャッシュ経由の読み書き試験

1面は720×1280×2 byteで1,843,200 byteです。2面で3,686,400 byteを使用し、
4 MiBの割り当て内に収めています。

DQS調整では、この実機で繰り返し選択された`phase=0, data=0, dqs=0`を最初に
100回読み出して検証します。合格時は31点の全探索を省略し、不合格時だけ従来の
フル探索へ戻ります。高速経路でも各候補に対するESP-IDFと同じ検査回数を使用します。

CPUが描画した内容をGDMAから参照できるよう、転送前にROMの
`Cache_WriteBack_Invalidate_Addr`をL1 DCache、L2 Cacheの順に呼び出します。
その後、両面の既知画素を再読出しし、外部PSRAMへ同期されたことを確認します。

## LCDとパネル初期化

Tab5のLCDリセットはESP32-P4のGPIOへ直結されていません。GPIO31/32上の
ソフトウェアI2CからPI4IOE1を操作し、P4のLCD resetをパルスします。バックライトは
GPIO22です。

D-PHYにはLDO_VO3から2.5 Vを供給し、2レーン、約960 Mbps/laneで動作させます。

確認した本体と箱にはST7123と記載されていますが、タッチコントローラーの
ファームウェア値は`1`でした。M5Stack BSPはこの値に対してST7121互換シーケンスを
選ぶため、本プロジェクトも実機で表示できた`src/lcd/st7121.rs`のシーケンスを
使用しています。

## 映像データ経路

```text
CPU描画
  → PSRAM上のRGB565フレームバッファ
  → L1/L2キャッシュ書き戻し
  → DW-GDMA channel 0
  → DSI Bridge FIFO
  → DSI Host
  → 2-lane D-PHY
  → LCDパネル
```

フレームバッファ入力はRGB565、ネイティブ走査は720×1280です。DSI Hostの
DPIクロックは実クロック80 MHzを使用し、実機で動作確認した水平・垂直
タイミングを設定します。

DW-GDMAは64 bit幅でPSRAMから読み、DSI Bridge FIFOへ固定アドレス書き込みを
行います。1回のブロック転送が1画面全体に対応します。

## フレーム割り込み

ESP32-P4 ECO2のDSI BridgeにはVSync割り込みがありません。ESP-IDF v5.5.3も
ECO2では1画面分のGDMA完了イベントを擬似VSyncとして扱います。本プロジェクトも
同じ方式を採用しています。

GDMA完了ISRは次の最小処理だけを行います。

1. 割り込み状態をクリア
2. 要求されているフレームバッファのアドレスを設定
3. 次の1画面転送を開始
4. 表示中の面とフレーム番号を更新

表示面の変更は転送完了時だけ反映されるため、走査途中でフレームバッファが
切り替わることを防ぎます。

ECO2は初期版CLICを使用します。汎用`riscv-rt`のトラップ入口はCLIC拡張された
`mcause`を保存しないため、`src/interrupts.rs`に専用入口を置いています。この入口は
全整数レジスタ、`mcause`、`mstatus`、`mepc`を保存・復元します。DW-GDMA割り込み
source 24をCPU external line 1、CLIC interrupt 17へ接続しています。

## 描画API

`src/framebuffer.rs`の`DoubleBuffer`は次のRGB565描画処理を提供します。

- `fill`、`draw_pixel`
- `draw_line`
- `fill_rect`、`stroke_rect`
- `fill_circle`、`draw_circle`
- `blit_rgb565`
- `draw_text`

`draw_text`は`src/framebuffer/font.rs`の5×7 ASCIIフォントを使用します。倍率と
前景色、任意の背景色を指定できます。小文字は大文字として表示します。

描画APIの論理解像度は1280×720 Landscapeです。CW回転のため、論理座標を
ネイティブフレームバッファへ次のように変換します。

```text
native_x = logical_y
native_y = 1279 - logical_x
```

DSIの解像度やパネル初期化コマンドは変更せず、全描画プリミティブと画像転送に
同じ座標変換を適用します。

FB1は表示位置を確認する座標校正画面です。100 px間隔のグリッド、四隅と中央の
論理座標、外端から赤・緑・青・白の1 px枠を描きます。外周色の欠落からクロップを、
グリッド位置から表示オフセットを判別できます。

## ファイル構成

- `src/main.rs`: 起動順だけを定義
- `src/startup.rs`: watchdog停止
- `src/uart.rs`: USB Serial/JTAG出力
- `src/psram.rs`: PSRAM、DQS調整、MMU、キャッシュ同期
- `src/framebuffer.rs`: ダブルバッファと描画API
- `src/framebuffer/font.rs`: 5×7フォント
- `src/lcd.rs`: I/O expander、D-PHY、パネル、DSI Bridge、DW-GDMA
- `src/lcd/st7121.rs`: パネル初期化コマンド
- `src/interrupts.rs`: CLICトラップ入口とGDMA ISR
- `memory.x`: ESP32-P4用メモリとイメージ配置
- `.cargo/config.toml`: ターゲット、リンカー、`espflash` runner

`esp-idf-reference/`には、レジスタ設定との比較に使用したESP-IDF v5.5.3版の
参照実装があります。

## 診断ログ

正常時の主要な通過点は次のとおりです。

```text
PSRAM: ready for two RGB565 framebuffers
LCD: D-PHY 4/4 ready
LCD: DCS init complete
LCD: DMA 3/3 full-frame interrupt installed
LCD: RGB565 framebuffer DMA active
```

主な失敗ログ:

- `PSRAM: mode-register transaction failed`: MSPI3コマンド経路
- `PSRAM: no valid DQS phase`: DQS位相調整
- `PSRAM: mapped memory test failed`: MMUまたはキャッシュ経路
- `LCD: PI4IOE1 reset control failed`: ソフトウェアI2CまたはI/O expander
- `LCD: D-PHY lock timeout`: D-PHY電源、クロック、PLL
- `LCD: DCS FIFO timeout`: パネルコマンド経路
- `LCD: DMA interrupt error`: DW-GDMA転送

## 制約

- ECO2で確認したレジスタ値とROM APIアドレスを使用しています。
- PSRAMは先頭4 MiBだけを固定アドレスへ割り当て、汎用allocatorには登録しません。
- DSIタイミングとパネルシーケンスは確認したTab5個体向けです。
- タッチ入力、日本語フォント、省電力制御は未実装です。
