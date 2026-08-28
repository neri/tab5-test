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
- 16ピクセルビットマップフォントのコンソール（8×16セル、156桁×44行）。
  通常キー入力では変更された1セルだけを描画して部分キャッシュ同期する
- CardKB v1.1（PORT.A、GPIO53/54、I2C 0x5F）と、ハブ配下も含むUSB HID Boot
  キーボードの統合入力。どちらもEsc・カーソルキーを認識し、USBはさらに
  Home/End、Delete、F1〜F12とCtrl＋英字も認識する（CardKBにCtrlキーは無い）
- USB-Aの起動時スキャンと、未接続のルートポート・空いているハブポートの定期再確認。
  CardKBも未接続なら約1秒ごとに再検出する
- USBメモリの自動マウント。挿すと`/vol/usbNpM`に現れ、抜くと外れる。そのたびに
  コンソールへ1行出る（`automount off`で止められる）

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
| ブラウザ | `browser` | HTMLから文章とリンクを取り出して読む全画面ビューア。**Webブラウザではない**——CSS、JavaScript、画像デコードはいずれも無い。`http://`と`https://`を取得できるが、**HTTPSは暗号化するだけで接続先の身元を確認しない**（firmware組み込みのpinと一致した場合を除く）。toolbarの南京錠がその区別を出し、押すと文言が出る。文字符号化はUTF-8とShift_JIS。toolbarに戻る・進む・再読込（読み込み中は中止）のボタンがあり、キーではBackspaceまたは`[`で戻る、`]`で進む、`r`で再読込（履歴8ページ）。Tabでリンク選択、Enterで移動（未選択ならアドレス欄）、Ctrl+Lまたは`F2`でアドレス欄、Escapeで読み込み中止とアドレス欄を閉じる操作、`i`でヒープ・ソケット・履歴の数値、Ctrl+Q（または`q`）で終了。CardKBにはCtrlキーが無いので、CardKBで操作するときは`q`と`F2`を使う。schemeを省いて打ったアドレスには`http://`を補う。タッチとUSBマウスでもリンクを選べる。通信を必要としない組み込みページを持つのでWi-Fiが無くても開ける（[docs/BROWSER.md](docs/BROWSER.md)） |
| USBマウス・画面 | `win` | Windows 95風デスクトップを表示。USB HID Bootマウスでカーソル移動とタイトルバーのドラッグを確認し、タスクバーにRTC時刻を表示 |
| SDカード | `sdinfo` `sdmbr` `sdread` `sdreadn` `sdreadpsram` `sdwritetest` `sdzero` | 4bit/High Speedモード（実クロック40 MHz。ESP32-C6を使っている間は同じコントローラの入力クロックを共有するためDefault Speedの20 MHz）での生ブロックI/O。CID/CSD要約、MBR表示、1ブロック読み出し、DMAでnブロック読み出し、PSRAM宛DMA読み出しと検証、書き込み+検証+復元、ゼロ埋め |
| USBデバイス情報 | `lsusb` | 接続デバイスをハブ経由のツリーで表示。Composite Deviceは各interfaceを1行ずつ出す。`lsusb <アドレス>`でそのデバイスの主要な記述子（デバイス、コンフィグレーション、interfaceとendpoint、HID記述子）と、製品名・ベンダ名・シリアルの文字列記述子を表示 |
| USB-A | `usbinfo` `usbrescan` `usbhub` `usbhw` `usbvbus` | USBスタックの診断用。ハブ配下を含む接続デバイス一覧、再スキャン、ハブのディスクリプタとポート状態、DWCコアのGHWCFG/HCSPLT、VBUSの手動制御 |
| USBストレージ | `usbmsc` `usbread` `usbmbr` `usbwritetest` `usbzero` | SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10)、1ブロック読み出し、MBR表示（`sdmbr`と同じ形式）、WRITE(10)での書き込み+照合+復元、ゼロ埋め |
| ブロックデバイス | `devices` `blkread` | ram／sd0／usbNの一覧とジオメトリ、LBA 0がMBRか単体のFAT/exFATボリュームかの判定、1ブロックの読み出し（`pN`を付けるとそのMBRパーティション相対）。`usbN`の番号は接続時に振られ、そのデバイスが繋がっている間は動かないので、1本抜いても他のマウントは壊れない |
| ファイルシステム | `mount` `umount` `automount` `mounts` `fsverify` | FAT12/16/32とexFATの読み出し。Unix型の単一ツリーへマウントし（`/tmp`がPSRAM上のRAMディスク、`/vol/<name>`がSDとUSB）、書き込みは`/tmp`だけでSDとUSBは常に読み取り専用。USBメモリは挿抜に合わせて自動でマウント・アンマウントされ、`automount off`で止まる。SDは挿抜検出線が無いので明示マウントのみ。`fsverify`は媒体が入れ替わっていないかをマウント時の識別情報と突き合わせる |
| ファイル操作 | `cd` `pwd` `ls` `cat` `write` `append` `mkdir` `rm` `rmdir` `mv` | カレントディレクトリと相対パスで辿る。`ls`は名前順の桁詰めで、`-l`が詳細、`-a`が`.`と`..`。`rm`はファイル、`rmdir`は空のディレクトリ、`mv`は改名と同一ボリューム内の移動（ボリュームを跨ぐ移動はコピーになるので断る） |
| ファイルシステム診断 | `fsopen` `fsread` `fsclose` `fill` | `fsopen`はコマンドを跨いでファイルを開いたまま保持する。媒体を抜くとハンドルが`stale`になり、同じものを挿し直しても復活しないことを確認できる。`fill`は既知のパターンを書いて書き込み経路の所要時間を測る |
| Wi-Fi | `wifiscan` `wificonnect` `wifistatus` `wifidisconnect` `wifiinfo` `wifiup` `wifimac` | ESP32-C6のESP-Hostedファームウェア経由でAPのスキャンと接続。接続先のSSID/BSSID/チャンネル/RSSI表示、切断。`wifiinfo`/`wifiup`/`wifimac`はSDIO活性化・リンク・RPCの各層の診断 |
| ネットワーク | `ipconfig` `nslookup` `ping` `tftpget` `httpget` `hs` `bt` `netdump` | smoltcpによるIPv4。DHCPまたは手動でのアドレス設定、名前解決（Aレコード）、ICMP echoと往復時間、TFTP読み出し（サイズとCRC-32）、HTTP/1.0 GET（`https://`も可。chunked転送の復号とリダイレクトの追跡はブラウザ側）。`tftpget`と`httpget`は受け取ったファイルをカレントディレクトリへ保存する（書けるのは`/tmp`だけなので`cd /tmp`してから使う）。書き込みは`.part`という名前で行い完了時に改名するので、本来の名前で現れたファイルは完全なもの。`httpget`はパスがファイルを名指していないとき（`/`や`/`で終わるパス）と、応答が2xx以外のときは保存せずヘッダだけ表示する。宛先はホスト名でもIPアドレスでも指定できる。`netdump`はC6とやり取りする802.3フレームのヘッダを表示する。`hs`はブラウザが使う中断可能なHTTPトランザクションを直接回して結果を数値で出し、`bt <url>`は試験用サーバ（`tools/browser_fixture_server.py`）の全端点を巡回して期待どおりの結末になるか検査する。`hs`と`bt`もschemeを省いたアドレスを受け付ける |
| 電源 | `shutdown` | 電源コントローラ経由で本体を切る（再開は物理電源キー） |

`sdzero`と`usbzero`は指定LBAをゼロで上書きする破壊的なコマンドです。`sdwritetest`と
`usbwritetest`も復元失敗時はデータを壊す可能性があるため、テスト用のカードやUSBメモリの
無害なLBAでのみ実行してください。USB MSCへの書き込みは間欠故障の原因が未特定のままで、
各WRITEの直前に予防的なBOT再同期を行うことで成立しています。

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

`browser/`と`font/`は別クレート（ワークスペースメンバ）です。ブラウザのURL解析・
HTML解析・文書モデル・折り返し・文字符号化と、16ピクセルフォントのglyphデータが
ここにあり、どちらもハードウェアに触らないので**ホストでビルドしてテストできます**。
ファームウェア側は`src/browser.rs`と`src/font.rs`が再エクスポートするだけです。

```sh
mise run test
```

ハードウェア構成、起動方式、FLASH XIP、メモリ配置、性能測定、ECO2固有の制約などの
技術的な詳細は[DESIGN.md](DESIGN.md)を参照してください。

## 未対応

- 日本語入力（IME、かな漢字変換）。表示は16ピクセルフォントでできます
- ファイルシステム経由でのSDカード・USBメモリへの書き込み。読み出しは可能で、
  ブロック単位の`sdwritetest`などとは別の話です。exFATへの書き込みも、採用
  ライブラリのexFAT対応が不安定なプレビューのため行いません
- SDカードの自動マウント。スロットに挿抜検出線が無く、挿さっているかは
  コマンドのタイムアウトでしか分からないため、明示マウントのままです
  （USBメモリは自動でマウントされます）
- 多段USBハブ（ハブ配下のハブ）
- IPv6、サーバ機能（TCP/IPはIPv4のクライアントのみです）。名前解決は
  Aレコードだけで、キャッシュ・逆引き・mDNSはありません
- **TLSの身元確認**。TLS 1.3で暗号化はしますが、証明書のchainを辿らず、
  root証明書を持たず、ホスト名も有効期限も照合しません。分かるのは「相手が
  提示した証明書の秘密鍵を持っている」ことだけで、それがアドレス欄のホストの
  ものかは確認していません。firmware組み込みのSPKI pinと一致した場合だけが
  例外ですが、通常ビルドのpin表は空です。したがって受動的な盗聴は防ぎますが、
  能動的な攻撃者は防げません（[docs/NETWORK.md](docs/NETWORK.md)）
- `browser`のCSS・JavaScript・画像デコード・form送信・cookie。HTMLから文章と
  リンクを取り出すところまでです。文字符号化はUTF-8とShift_JISだけで、
  EUC-JPやISO-2022-JPは読めません
- Wi-FiのSoftAPとBLE。5 GHz帯はESP32-C6が2.4 GHz専用のため使えません
- ESP32-P4 revision v3以降での動作確認
