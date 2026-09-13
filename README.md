# Tab5 実験レポジトリ

これはM5Stack Tab5の機能を実験するためのプログラムです。
明確なゴールはなく、思いついたことを色々実験します。

## できること

起動画面で各デバイスを初期化したあと、Wi-Fiにつながっていればブラウザ、
そうでなければデスクトップが開きます。起動中にEscapeを押すとコンソールに
入ります（[docs/SYSTEM_BAR.md](docs/SYSTEM_BAR.md)）。

- 画面上部のバーから、ランチャー、Wi-Fi設定、バッテリー表示を開く
- 簡易ブラウザでHTTP／HTTPSのページを読む。表、フォーム、PNG／JPEG画像に対応
  （[docs/BROWSER.md](docs/BROWSER.md)）
- CardKB、Tab5 Keyboard、USBキーボード・マウス、タッチで操作する
  （[docs/INPUT.md](docs/INPUT.md)）
- microSDとUSBメモリのFAT／exFATを読み、FATへ書き込む
  （[docs/FILESYSTEM.md](docs/FILESYSTEM.md)）
- ESP32-C6経由のWi-FiとIPv4通信
  （[docs/WIFI.md](docs/WIFI.md)、[docs/NETWORK.md](docs/NETWORK.md)）
- コンソールのシェルから各デバイスを診断する。`help`でコマンド一覧、
  `help <command>`で使用法を表示します
  （[docs/CONSOLE_COMMAND_REVIEW.md](docs/CONSOLE_COMMAND_REVIEW.md)）

シェルには媒体を上書きする破壊的なコマンドもあります。使う前に
[docs/STORAGE.md](docs/STORAGE.md)を確認してください。

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
モードに入るため避けます。

UARTログは別ターミナルで確認できます（既定は`/dev/ttyACM0`、引数で変更可）。

```sh
python3 tools/monitor.py
```

画面に何も出ないときの切り分けは[docs/DIAGNOSTICS.md](docs/DIAGNOSTICS.md)にあります。

## 構成

クレート直下がハードウェアを触る層、`src/app/`がアプリとシェルの層です。
ハードウェアに触らない処理はワークスペースメンバに分けてあり、ホストで
テストできます。

```sh
mise run test
```

モジュールごとの責務は[docs/FILE_LAYOUT.md](docs/FILE_LAYOUT.md)、ハードウェア
構成と設計上の判断は[DESIGN.md](DESIGN.md)にあります。

## 未対応

- 日本語入力（IME、かな漢字変換）。表示はできます
- exFATへの書き込み
- 多段USBハブ（ハブ配下のハブ）
- IPv6とサーバ機能
- pinを登録していないHTTPS接続先の身元確認（[docs/NETWORK.md](docs/NETWORK.md)）
- `browser`のCSS・JavaScript・cookie
- Wi-FiのSoftAP、BLE、5 GHz帯
