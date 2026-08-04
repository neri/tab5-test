# M5Stack Tab5 `no_std` Rust template

M5Stack Tab5のESP32-P4 ECO2（revision v1.3）向け`no_std`テンプレートです。
UARTへHello Worldを出力し、PSRAM上のRGB565ダブルバッファをMIPI-DSI LCDへ
表示します。

## できること

- USB Serial/JTAGへのUARTログ出力
- 1280×720 Landscape（CW回転）のRGB565ダブルバッファ
- フレーム境界での表示面切り替え
- ピクセル、直線、矩形、円、RGB565画像、5×7 ASCII文字の描画
- LCD初期化失敗時のカラーバー表示

PSRAMの準備後に4分割画像と座標校正用グリッドを直接表示し、その後は約2秒ごとに
画像が切り替わります。カラーバーはフレームバッファ経路の失敗時だけ表示します。

## 準備

Rustターゲットと`espflash`をインストールします。

```sh
rustup target add riscv32imafc-unknown-none-elf
cargo install espflash
```

## 実行

Tab5をUSB接続して書き込みます。

```sh
cargo run --release
```

書き込み後は本体を短くリセットしてください。約2秒の長押しはダウンロード
モードに入るため避けます。リセット時はUSBが一度切断・再接続されます。

UARTログは別ターミナルで確認できます。

```sh
python3 tools/monitor.py
```

正常時は、最終的に次のようなログが繰り返し表示されます。

```text
LCD: RGB565 framebuffer DMA active
LCD: framebuffer=0x00000001
```

## 構成

- `src/main.rs`: 各モジュールの呼び出し
- `src/uart.rs`: UART出力
- `src/psram.rs`: PSRAMとフレームバッファ領域
- `src/framebuffer.rs`: RGB565描画API
- `src/lcd.rs`: LCD、MIPI-DSI、GDMA
- `src/interrupts.rs`: フレーム完了割り込み

ハードウェア構成、起動方式、メモリ配置、ECO2固有の制約は
[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- タッチ入力
- 日本語フォント
- ESP32-P4 revision v3以降での動作確認
