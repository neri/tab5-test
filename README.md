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
| USBマウス・画面 | `win` | Windows 95風デスクトップを表示。USB HID Bootマウスでカーソル移動とタイトルバーのドラッグを確認し、タスクバーにRTC時刻を表示 |
| SDカード | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdreadpsram` `sdwritetest` `sdzero` | 4bit/High Speedモード（実クロック40 MHz。ESP32-C6を使っている間は同じコントローラの入力クロックを共有するためDefault Speedの20 MHz）での生ブロックI/O。CID/CSD要約、MBR表示、1ブロック読み出し、DMAでnブロック読み出し、PSRAM宛DMA読み出しと検証、書き込み+検証+復元、ゼロ埋め |
| USB-A | `usbinfo` `usbrescan` `usbhub` `usbhw` `usbvbus` | ハブ配下を含む接続デバイス一覧、再スキャン、ハブのディスクリプタとポート状態、DWCコアのGHWCFG/HCSPLT、VBUSの手動制御 |
| USBストレージ | `usbmsc` `usbread` `usbmbr` | SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10)、1ブロック読み出し、MBR表示（`sdmbr`と同じ形式） |
| Wi-Fi | `wifiscan` `wificonnect` `wifistatus` `wifidisconnect` `wifiinfo` `wifiup` `wifimac` | ESP32-C6のESP-Hostedファームウェア経由でAPのスキャンと接続。接続先のSSID/BSSID/チャンネル/RSSI表示、切断。`wifiinfo`/`wifiup`/`wifimac`はSDIO活性化・リンク・RPCの各層の診断 |
| ネットワーク | `ipconfig` `nslookup` `ping` `tftpget` `httpget` `netdump` | smoltcpによるIPv4。DHCPまたは手動でのアドレス設定、名前解決（Aレコード）、ICMP echoと往復時間、TFTP読み出し（サイズとCRC-32）、最小のHTTP/1.0 GET。宛先はホスト名でもIPアドレスでも指定できる。`netdump`はC6とやり取りする802.3フレームのヘッダを表示する |
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
XIP: pre-PSRAM DROM+IROM ok
PSRAM: ready (framebuffer + heap)
XIP: post-PSRAM DROM+IROM ok
LCD: RGB565 framebuffer DMA active
CardKB: ready
USB: initial scan complete
```

CardKBが接続されていなければ`CardKB: absent`となります。USBの接続状態に
よっては、初期スキャン中の`USB: ...`ログも先に表示されます。

## 構成

クレート直下がハードウェアを触る層、`src/app/`がシェルコマンドを実行するための
層です。リンカ配置と検査ツールは`memory.x`と`tools/`にあります。

ハードウェア構成、起動方式、FLASH XIP、メモリ配置、性能測定、ECO2固有の制約などの
技術的な詳細は[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- 日本語フォント
- FAT/exFATファイルシステムの解釈
- USB Mass Storageへの書き込み、多段USBハブ
- IPv6、TLS、サーバ機能（TCP/IPはIPv4のクライアントのみで、受信したデータの
  保存先はメモリだけです）。名前解決はAレコードだけで、キャッシュ・逆引き・
  mDNSはありません
- Wi-FiのSoftAPとBLE。5 GHz帯はESP32-C6が2.4 GHz専用のため使えません
- ESP32-P4 revision v3以降での動作確認
