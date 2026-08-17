# Tab5 実験レポジトリ

これはM5Stack Tab5の機能を実験するためのプログラムです。
明確なゴールはなく、思いついたことを色々実験します。

## できること

起動するとコンソール画面が出て、そこで動く簡易シェルからTab5の各デバイスを
試せます。

### 常時動いているもの

- USB Serial/JTAGへのUARTログ出力
- 1280×720 Landscape（CW回転）のRGB565フレームバッファを、PSRAMから
  DW-GDMAでスキャンアウト
- 5×7 ASCIIコンソール。通常キー入力では変更された1セルだけを描画して
  部分キャッシュ同期する
- CardKB v1.1（PORT.A、GPIO53/54、I2C 0x5F）と、ハブ配下も含むUSB HID Boot
  キーボードの統合入力。どちらもEsc・カーソルキーを認識し、USBはさらに
  Home/End、Delete、F1〜F12も認識する
- USB-Aの起動時スキャンと、未接続のルートポート・空いているハブポートの定期再確認。
  CardKBも未接続なら約1秒ごとに再検出する

コンソール画面はPSRAMの準備が終わってから表示します。PSRAMや画面の初期化に
失敗した場合は何も表示されないので、USBシリアルのログで切り分けます。

### シェルコマンド

`help`でコマンド一覧、`help <command>`で個別の使用法を表示します。

| 対象 | コマンド | 内容 |
| --- | --- | --- |
| 基本 | `help` `clear` `echo` `about` `uptime` `reboot` | コマンド一覧、画面消去、文字列表示、バナー、起動からの経過時間、再起動 |
| CPU | `cpuinfo` | RISC-V機械識別CSR（`mvendorid`、`marchid`、`mimpid`、`mhartid`、`misa`）を16進数で表示。`misa`には`RV32IMAFDC`のようなISA拡張表記も併記 |
| メモリ | `mem` `alloctest` `membench` | PSRAM/RAM使用量、PSRAMヒープからN MiB確保しての読み書き検証、SRAM・キャッシュ経由PSRAM・直接aliasのアクセス速度測定 |
| 表示・DMA調停 | `backlight` `stress` `icm` `ppafill` | バックライト切り替え、全画面塗りつぶしの所要時間とDPI FIFO underrunの計測、DW-GDMA読み出し優先度とAXI QoSの設定、PPAとCPUによる矩形塗りつぶしの比較 |
| タッチ | `paint` `touchtest` | GT911またはST7121/ST7123タッチコントローラを使うお絵描き画面と、二本指同時入力の確認 |
| 画面の座標確認 | `coordtest` | 100ピクセルグリッド、論理中心軸、四隅の座標、1ピクセルずつ内側へ入った4本の枠を出す全画面チャート。CW回転とクリッピングを定規で突き合わせて確認する |
| センサー・RTC | `axistest` `battery` `rtc` | BMI270の傾きでボールを転がす、INA226でバッテリーパックの電圧・電流・電力をライブ表示、RX8130CE RTCの時刻表示・設定・レジスタダンプ・機能検査 |
| SDカード | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdreadpsram` `sdwritetest` `sdzero` | 4bit/High Speedモード（実クロック40 MHz）での生ブロックI/O。CID/CSD要約、MBR表示、1ブロック読み出し、DMAでnブロック読み出し、PSRAM宛DMA読み出しと検証、書き込み+検証+復元、ゼロ埋め |
| USB-A | `usbinfo` `usbrescan` `usbhub` `usbhw` `usbvbus` | ハブ配下を含む接続デバイス一覧、再スキャン、ハブのディスクリプタとポート状態、DWCコアのGHWCFG/HCSPLT、VBUSの手動制御 |
| USBストレージ | `usbmsc` `usbread` `usbmbr` | SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10)、1ブロック読み出し、MBR表示（`sdmbr`と同じ形式） |
| 電源 | `shutdown` | 電源コントローラ経由で本体を切る（再開は物理電源キー） |

`sdzero`は指定LBAをゼロで上書きする破壊的なコマンドです。`sdwritetest`も復元失敗時は
データを壊す可能性があるため、テスト用カードの無害なLBAでのみ実行してください。

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

## 構成

クレート直下がハードウェアを触る層、`src/app/`がシェルコマンドを実行するための
層です。

- `src/main.rs`: 各モジュールの呼び出し
- `src/uart.rs`: UART出力
- `src/psram.rs`: PSRAMとフレームバッファ領域
- `src/framebuffer.rs`: RGB565描画API
- `src/console.rs`: キー入力とシェル出力を表示するコンソール
- `src/cardkb.rs`: PORT.AのソフトウェアI2C CardKBドライバ
- `src/input.rs`: CardKBとUSBキーボードを統合する入力管理
- `src/touch.rs`: GT911／ST7121・ST7123タッチコントローラドライバ
- `src/bmi270.rs`: BMI270の6軸ドライバ
- `src/ina226.rs`: INA226バッテリー電力モニタードライバ
- `src/rtc.rs`: RX8130CE RTCドライバ（時刻読み書き、フラグ・制御レジスタ、機能検査）
- `src/sdmmc.rs`: SDブロックI/O
- `src/usb.rs`・`src/usb/`: USB-Aホスト、HIDキーボード、ハブ、MSC、デバイス管理
- `src/lcd.rs`: LCD、MIPI-DSI、GDMA
- `src/interrupts.rs`: フレーム完了割り込み
- `src/app.rs`: コンソールのフレームループと全画面モードへの出入り
  - `src/app/shell.rs`: 画面上の簡易シェル
  - `src/app/mbr.rs`: MBR表示（SDカードとUSBメモリで共用）
  - `src/app/membench.rs`: SRAM／PSRAMのアクセス速度測定
  - `src/app/paint.rs`: お絵描き画面
  - `src/app/touch_test.rs`: マルチタッチ診断画面
  - `src/app/coord_test.rs`: 座標キャリブレーションチャート
  - `src/app/axis_test.rs`: 傾きボール診断画面
  - `src/app/battery.rs`: バッテリーのライブ表示画面

ハードウェア構成、起動方式、メモリ配置、ECO2固有の制約は
[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- 日本語フォント
- FAT/exFATファイルシステムの解釈
- USB Mass Storageへの書き込み、HIDマウス、多段USBハブ
- ESP32-P4 revision v3以降での動作確認
