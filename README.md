# Tab5 実験レポジトリ

これはM5Stack Tab5の機能を実験するためのプログラムです。
明確なゴールはなく、思いついたことを色々実験します。

## できること

起動するとコンソール画面が出て、そこで動く簡易シェルからTab5の各デバイスを
試せます。

### 常時動いているもの

- USB Serial/JTAGへのUARTログ出力（[docs/DIAGNOSTICS.md](docs/DIAGNOSTICS.md)）
- 1280×720の画面（[docs/DISPLAY.md](docs/DISPLAY.md)）と、16ピクセルフォントの
  コンソール（[docs/CONSOLE_SHELL.md](docs/CONSOLE_SHELL.md)、[docs/FONT.md](docs/FONT.md)）
- CardKB、Tab5 Keyboard、USBキーボード・マウス、タッチの統合入力
  （[docs/INPUT.md](docs/INPUT.md)）
- USB-Aの接続検出と再スキャン（[docs/USB.md](docs/USB.md)）
- USBメモリの自動マウント。挿すと`/vol`以下に現れ、抜くと外れる
  （[docs/FILESYSTEM.md](docs/FILESYSTEM.md)）

コンソール画面はPSRAMの準備が終わってから表示します。PSRAMや画面の初期化に
失敗した場合は何も表示されないので、USBシリアルのログで切り分けます。

### シェルコマンド

`help`でコマンド一覧、`help <command>`で個別の使用法を表示します。

| 対象 | コマンド | 内容 |
| --- | --- | --- |
| 基本 | `help` `clear` `echo` `about` `uptime` `reboot` | コマンド一覧、画面消去、文字列表示、バナー、起動からの経過時間、再起動（[docs/CONSOLE_SHELL.md](docs/CONSOLE_SHELL.md)） |
| CPU | `cpuinfo` | RISC-Vの機械識別CSRとISA拡張表記を表示 |
| メモリ | `mem` `alloctest` `membench` | PSRAM/RAM使用量、ヒープ確保の検証、アクセス速度測定（[docs/PSRAM.md](docs/PSRAM.md)） |
| 表示・DMA調停 | `backlight` `stress` `icm` `ppafill` | バックライト、全画面塗りとFIFOアンダーランの計測、DMA優先度、PPAとCPUの比較（[docs/DISPLAY_BANDWIDTH.md](docs/DISPLAY_BANDWIDTH.md)） |
| タッチ | `paint` `touchtest` | お絵描き画面と多点タッチの確認（[docs/APPS.md](docs/APPS.md)） |
| 画面の座標確認 | `coordtest` | グリッドと四隅の座標を出す全画面チャート。回転とクリッピングを定規で確認する（[docs/GRAPHICS.md](docs/GRAPHICS.md)） |
| センサー・RTC | `axistest` `battery` `rtc` | BMI270の傾き、INA226によるバッテリー表示、RX8130CE RTCの表示・設定・検査（[docs/RTC.md](docs/RTC.md)） |
| ブラウザ | `browser` | 簡易ウェブブラウザを起動する。対応するHTML、操作、`file:`、TLSの扱いは[docs/BROWSER.md](docs/BROWSER.md) |
| USBマウス・画面 | `win` | Windows 95風デスクトップ。USB HID Bootマウスの動作確認用（[docs/APPS.md](docs/APPS.md)） |
| SDカード | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdreadpsram` `sdwritetest` `sdzero` | 生ブロックI/O。カード情報、MBR表示、読み出し、書き込み検証、ゼロ埋め（[docs/STORAGE.md](docs/STORAGE.md)） |
| USBデバイス情報 | `lsusb` | 接続デバイスをハブ経由のツリーで表示。引数を付けると記述子を表示（[docs/USB.md](docs/USB.md)） |
| USB-A | `usbinfo` `usbrescan` `usbhub` `usbhw` `usbvbus` | USBスタックの診断。デバイス一覧、再スキャン、ハブとコアの状態、VBUS制御（[docs/USB.md](docs/USB.md)） |
| USBストレージ | `usbmsc` `usbread` `usbmbr` `usbwritetest` `usbzero` | USBマスストレージの生ブロックI/O。SDカード側と同じ操作（[docs/STORAGE.md](docs/STORAGE.md)） |
| ブロックデバイス | `devices` `blkread` | ram／sd0／usbNの一覧とジオメトリ、1ブロックの読み出し（[docs/STORAGE.md](docs/STORAGE.md)） |
| ファイルシステム | `mount` `umount` `automount` `mounts` `fsverify` | FAT12/16/32とexFATの読み出し。単一ツリーへのマウント、USBの自動マウント、媒体の照合（[docs/FILESYSTEM.md](docs/FILESYSTEM.md)） |
| ファイル操作 | `cd` `pwd` `ls` `cat` `write` `append` `mkdir` `rm` `rmdir` `mv` | カレントディレクトリと相対パスで辿る。書けるのは`/tmp`だけ（[docs/FILESYSTEM.md](docs/FILESYSTEM.md)） |
| ファイルシステム診断 | `fsopen` `fsread` `fsclose` `fill` | コマンドを跨いでファイルを開いたまま保持し、媒体を抜いたときの挙動を見る。`fill`は書き込み経路の計測（[docs/FILESYSTEM.md](docs/FILESYSTEM.md)） |
| Wi-Fi | `wifiscan` `wificonnect` `wifistatus` `wifidisconnect` `wifiinfo` `wifiup` `wifimac` | ESP32-C6経由でAPのスキャンと接続、状態表示、切断、各層の診断（[docs/WIFI.md](docs/WIFI.md)） |
| ネットワーク | `ipconfig` `nslookup` `ping` `tftpget` `httpget` `hs` `bt` `netdump` | smoltcpによるIPv4。アドレス設定、名前解決、ping、TFTPとHTTPの取得、フレームダンプ、ブラウザの取得経路の診断（[docs/NETWORK.md](docs/NETWORK.md)） |
| 電源 | `shutdown` | 電源コントローラ経由で本体を切る（再開は物理電源キー） |

`sdzero`と`usbzero`は指定LBAをゼロで上書きする破壊的なコマンドです。`sdwritetest`と
`usbwritetest`も復元失敗時はデータを壊す可能性があるため、テスト用のカードやUSBメモリの
無害なLBAでのみ実行してください（[docs/STORAGE.md](docs/STORAGE.md)）。

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

正常に起動すると初期化の通過点がログに並びます。何が出れば正常か、どこで
止まったら何を疑うかは[docs/DIAGNOSTICS.md](docs/DIAGNOSTICS.md)にあります。

## 構成

クレート直下がハードウェアを触る層、`src/app/`がシェルコマンドを実行するための
層です。リンカ配置と検査ツールは`memory.x`と`tools/`にあります。

`browser/`と`font/`は別クレート（ワークスペースメンバ）です。ハードウェアに
触らないので**ホストでビルドしてテストできます**。

```sh
mise run test
```

モジュールごとの責務は[docs/FILE_LAYOUT.md](docs/FILE_LAYOUT.md)、ハードウェア
構成・起動方式・メモリ配置・性能測定・ECO2固有の制約は[DESIGN.md](DESIGN.md)に
あります。

## 未対応

- 日本語入力（IME、かな漢字変換）。表示はできます
- ファイルシステム経由でのSDカード・USBメモリへの書き込み。書けるのは`/tmp`だけ
  です（[docs/FILESYSTEM.md](docs/FILESYSTEM.md)）
- SDカードの自動マウント。明示マウントのみです（USBメモリは自動）
- 多段USBハブ（ハブ配下のハブ）
- IPv6、サーバ機能。TCP/IPはIPv4のクライアントのみです（[docs/NETWORK.md](docs/NETWORK.md)）
- **TLSの身元確認。**暗号化はしますが、接続先が名乗ったとおりの相手かは
  確認していません。受動的な盗聴は防ぎますが、能動的な攻撃者は防げません
  （[docs/NETWORK.md](docs/NETWORK.md)）
- `browser`のCSS・JavaScript・画像デコード・form送信・cookie
  （[docs/BROWSER.md](docs/BROWSER.md)）
- Wi-FiのSoftAPとBLE。5 GHz帯はESP32-C6が2.4 GHz専用のため使えません
