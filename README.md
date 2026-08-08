# Tab5 実験レポジトリ

これはM5Stack Tab5の機能を実験するためのプログラムです。
明確なゴールはなく、思いついたことを色々実験します。

## できること

- USB Serial/JTAGへのUARTログ出力
- 1280×720 Landscape（CW回転）のRGB565ダブルバッファ
- CardKB v1.1（PORT.A、GPIO53/54、I2C 0x5F）の入力エコー
- 改行、Backspace、Tab、画面末尾でのスクロールに対応した5×7 ASCIIコンソール
- 通常キー入力時の1セル部分描画・部分キャッシュ同期
- PSRAM初期化失敗時のカラーバー表示

PSRAMの準備後にコンソール画面を表示します。CardKBが未接続の場合は約1秒ごとに
再検出するため、起動後の接続にも対応します。カラーバーはPSRAM初期化に
失敗した場合だけ表示します。

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
CardKB: ready
```

## 構成

- `src/main.rs`: 各モジュールの呼び出し
- `src/uart.rs`: UART出力
- `src/psram.rs`: PSRAMとフレームバッファ領域
- `src/framebuffer.rs`: RGB565描画API
- `src/console.rs`: CardKB入力エコー用コンソール
- `src/cardkb.rs`: PORT.AのソフトウェアI2C CardKBドライバ
- `src/lcd.rs`: LCD、MIPI-DSI、GDMA
- `src/interrupts.rs`: フレーム完了割り込み

ハードウェア構成、起動方式、メモリ配置、ECO2固有の制約は
[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- タッチ入力
- 日本語フォント
- ESP32-P4 revision v3以降での動作確認
