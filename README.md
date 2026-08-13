# Tab5 実験レポジトリ

これはM5Stack Tab5の機能を実験するためのプログラムです。
明確なゴールはなく、思いついたことを色々実験します。

## できること

- USB Serial/JTAGへのUARTログ出力
- 1280×720 Landscape（CW回転）のRGB565ダブルバッファ
- CardKB v1.1（PORT.A、GPIO53/54、I2C 0x5F）とUSB HID Bootキーボードから入力できる
  5×7 ASCIIコンソール
- PSRAM・バックライト・SDカード・USB-Aを確認する簡易シェル
- SDカードの4bit/High Speedモード（実クロック40 MHz）での生ブロック読み書きと
  MBR表示
- USBハブ配下を含むHID BootキーボードとUSB Mass Storageの読み込み
- GT911またはST7121/ST7123タッチコントローラを使う`paint`コマンドと、二本指同時入力を確認する`touchtest`コマンド
- 通常キー入力時の1セル部分描画・部分キャッシュ同期
- PSRAM初期化失敗時のカラーバー表示

PSRAMの準備後にコンソール画面を表示します。CardKBが未接続の場合は約1秒ごとに
再検出します。USB-Aは起動時にスキャンし、未接続のルートポートや空いているハブポートも
定期的に再確認します。カラーバーはPSRAM初期化に失敗した場合だけ表示します。

## 準備

Rustターゲットと`espflash`をインストールします。UARTモニターを使う場合は
Python 3と`pyserial`も必要です。

```sh
rustup target add riscv32imafc-unknown-none-elf
cargo install espflash
python3 -m pip install pyserial
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

既定では`/dev/ttyACM0`を開きます。別のデバイス名になった場合は引数で指定できます。

```sh
python3 tools/monitor.py /dev/ttyACM1
```

正常時は、初期化の通過点として次のようなログが1回ずつ表示されます。

```text
LCD: RGB565 framebuffer DMA active
CardKB: ready
USB: initial scan complete
```

CardKBが接続されていなければ`CardKB: absent`となります。USBの接続状態に
よっては、初期スキャン中の`USB: ...`ログも先に表示されます。

シェルで`help`を実行するとコマンド一覧、`help <command>`で個別の使用法を表示します。
`sdzero`は指定LBAをゼロで上書きする破壊的なコマンドです。`sdwritetest`も復元失敗時は
データを壊す可能性があるため、テスト用カードの無害なLBAでのみ実行してください。

## 構成

- `src/main.rs`: 各モジュールの呼び出し
- `src/uart.rs`: UART出力
- `src/psram.rs`: PSRAMとフレームバッファ領域
- `src/framebuffer.rs`: RGB565描画API
- `src/console.rs`: キー入力とシェル出力を表示するコンソール
- `src/shell.rs`: 画面上の簡易シェル
- `src/cardkb.rs`: PORT.AのソフトウェアI2C CardKBドライバ
- `src/touch.rs`・`src/paint.rs`: タッチ入力とお絵描き画面
- `src/sdmmc.rs`・`src/mbr.rs`: SDブロックI/OとMBR表示
- `src/usb.rs`・`src/usb/`: USB-Aホスト、HIDキーボード、ハブ、MSC、デバイス管理
- `src/lcd.rs`: LCD、MIPI-DSI、GDMA
- `src/interrupts.rs`: フレーム完了割り込み

ハードウェア構成、起動方式、メモリ配置、ECO2固有の制約は
[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- 日本語フォント
- FAT/exFATファイルシステムの解釈
- USB Mass Storageへの書き込み、HIDマウス、多段USBハブ
- ESP32-P4 revision v3以降での動作確認
