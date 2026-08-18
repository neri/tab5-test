# 表示パイプライン

> 索引: [`../DESIGN.md`](../DESIGN.md)

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
2. フレームバッファのアドレスを再設定
3. 次の1画面転送を開始
4. フレーム番号を更新

走査はシングルバッファなので、ISRは毎フレーム同じアドレスを設定し直すだけです。
切り替えるべき面がないため、前景ループは1フレームのどの時点でも描画できます。

ECO2は初期版CLICを使用します。汎用`riscv-rt`のトラップ入口はCLIC拡張された
`mcause`を保存しないため、`src/interrupts.rs`に専用入口を置いています。この入口は
全整数レジスタ、`mcause`、`mstatus`、`mepc`を保存・復元します。DW-GDMA割り込み
source 24をCPU external line 1、CLIC interrupt 17へ接続しています。`_start_trap`、
`ExceptionHandler`、`esp32p4_interrupt`は常にIRAMへ配置し、release ELFの
relocation検査でDROM/IROM参照がないことを確認します。
