# 設計資料

## 対象と方針

このプロジェクトはM5Stack Tab5のESP32-P4 ECO2（chip revision v1.3）を対象に
しています。ESP-IDFやRTOSをリンクせず、`riscv-rt`とレジスタ操作だけで起動、
PSRAM、MIPI-DSI、GDMAを初期化します。

実機で確認した構成は次のとおりです。

- ESP32-P4 ECO2、eFuse block revision v0.3
- 16 MiB SPI Flash
- Hex-DDR PSRAM（32 MiB）
- ネイティブ走査720×1280のMIPI-DSI LCD
- USB Serial/JTAG

## ドキュメント構成

本文は`docs/`直下に分割してあります。各文書は実装の現状を説明するもので、
作業計画は`docs/plans/`に分けてあります（下記）。

| 文書 | 内容 |
| --- | --- |
| [BOOT.md](docs/BOOT.md) | イメージ配置（XIP／IRAM／DRAM）、RAMの範囲、起動シーケンス |
| [PSRAM.md](docs/PSRAM.md) | PSRAM初期化、DQS調整、MMU割り当て、キャッシュ同期、グローバルアロケータ |
| [DISPLAY.md](docs/DISPLAY.md) | LCDとパネル初期化、映像データ経路、フレーム割り込み |
| [DISPLAY_BANDWIDTH.md](docs/DISPLAY_BANDWIDTH.md) | 表示帯域とFIFOアンダーラン、PSRAMの実測値、PPA／2D-DMAへの移行、試して駄目だった方法 |
| [GRAPHICS.md](docs/GRAPHICS.md) | `Framebuffer`の描画API、CW回転による論理↔ネイティブ座標変換 |
| [FONT.md](docs/FONT.md) | DROM直接ASCIIと16px日本語A4、文字幅、生成物、font source取得 |
| [CONSOLE_SHELL.md](docs/CONSOLE_SHELL.md) | コンソールのセル管理と部分書き戻し、シェル、再起動、全体電源断 |
| [CONSOLE_COMMAND_REVIEW.md](docs/CONSOLE_COMMAND_REVIEW.md) | シェルコマンド全数の棚卸しと、一般実用／専門家向け／開発検証専用の分類 |
| [INPUT.md](docs/INPUT.md) | ソフトI2C、CardKB／USBキーボード、`Key`正規化、`InputManager`、ポインタ、タッチコントローラー |
| [GUI_THEME.md](docs/GUI_THEME.md) | 通常GUIの共通配色、選択行、状態色、Backの表示 |
| [SYSTEM_BAR.md](docs/SYSTEM_BAR.md) | 上部48 pixelの共有バー、アプリ区分、coordinator、入力・timer・ミニアプリの所有 |
| [APPS.md](docs/APPS.md) | ペイント／タッチ診断、座標チャート、BMI270軸テスト、バッテリー、デスクトップ |
| [USB.md](docs/USB.md) | USB-Aホストの対応範囲、バス所有とスキャン、転送方式、Split Transaction |
| [STORAGE.md](docs/STORAGE.md) | SDカードとUSBマスストレージのブロックI/O、共通ブロックデバイス層、MBR判定、シェルコマンド |
| [FILESYSTEM.md](docs/FILESYSTEM.md) | VFS、マウント規則、USBの自動マウント、FAT読み出し、パスの規則、カレントディレクトリ、`ls`の表示、空き容量（`df`）、読み書きの保証、RAMディスク |
| [WIFI.md](docs/WIFI.md) | ESP32-C6経由のWi-Fi。SDIO接続、ESP-Hostedのフレーム層とRPC、シェルコマンド、microSDとの共存 |
| [NETWORK.md](docs/NETWORK.md) | smoltcpによるIPv4。`phy::Device`実装、受信キューと背圧、SYSTIMERの1 kHzティック、DHCP／DNS／ping／TFTP／HTTP、TLS 1.3とSPKI pin |
| [BROWSER.md](docs/BROWSER.md) | `browser`のハイパーテキストビューア。対応するHTML、操作、上限、エラー、未認証TLSとセキュリティ表示、診断コマンド |
| [RTC.md](docs/RTC.md) | RX8130CEのカレンダー読み書き、UTCという決めごとと既定JST表示、`rtc test`の検査内容 |
| [FILE_LAYOUT.md](docs/FILE_LAYOUT.md) | モジュールごとの責務一覧、コーディング方針（コメントの言語、`unsafe`の粒度） |
| [DIAGNOSTICS.md](docs/DIAGNOSTICS.md) | 正常時のUARTログ通過点と主な失敗ログ |
| [KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md) | 実機で見つかった制約（DW-GDMA／SDHOST、USB、ファイルシステム） |

作業計画（段階分け、実機での判断条件と実際に踏んだ罠を残すもの）は`docs/plans/`以下に、
状態ごとに`active/`（着手済み・受入待ち）、`proposed/`（未着手）、`archive/`（完了・凍結・
打ち切り）へ分けて置いてあります。一覧と各計画の状態は
[docs/plans/INDEX.md](docs/plans/INDEX.md)を参照してください。

## 制約

- 起動完了時にWi-FiがOnlineかつIPv4取得済みならBrowser、それ以外はデスクトップへ進み、上部48 pixelのsystem barからNetwork settingsと
  Battery detailsを開きます。Consoleと専有GUIはbarを表示しません。統合経路は実機未確認です（[SYSTEM_BAR.md](docs/SYSTEM_BAR.md)）。

- ECO2で確認したレジスタ値とROM APIアドレスを使用しています。
- PSRAMは32 MiB全体を固定アドレスへMMU割り当てし、フレームバッファ（1,843,200
  byte）、`/`へ載せるRAMディスク（固定8 MiB）、残る23,322,624 byte（約22.24 MiB）の
  ヒープの3つへ分けます。ヒープは`linked_list_allocator`によるグローバル
  アロケータで、通常アプリ開始前にDROMのA4 font LZ4 raw blockを展開し、payload 1,021,279 byteを永続確保します。
  LZ4は圧縮率よりdecoderの単純さ、展開速度、追加の大きな作業bufferが不要な点を優先して採用しました
  （[PSRAM.md](docs/PSRAM.md)、[FONT.md](docs/FONT.md)）。
- DSIタイミングとパネルシーケンスは確認したTab5個体向けです。
- 省電力制御は未実装です。通常GUIのEnglish Latinは16／24／32 pixelのA4 font、
  日本語は16 pixelのA4 bitmap subset（32 pixel時は整数2倍）で表示し（[FONT.md](docs/FONT.md)）、収録外の文字は
  中空の枠で表示します。日本語入力は
  ありません。
- バッテリー表示はINA226による瞬時測定と電圧ベースの目安だけです。充電状態、USB-Cの
  接続状態、正確なSoC／残り時間、電池の健全性は取得しません。
- ストレージはブロック単位の読み書きとMBR表示に加えて、FAT12/16/32とexFATを
  読み出すVFSがあります。書き込めるのはFATで、PSRAM上のFAT16 RAMルート`/`
  （8 MiB、リセットで消える）に加えてSDカードとUSB Mass Storage上のFATが
  **既定で読み書き**です（`mount -r`で読み取り専用にできます）。exFATは形式として
  読み取り専用です。各ボリュームの空き容量は`df`で確認できます。電断に対する原子性や自動修復は保証しません——保証するのは
  エラーを返さず完了した通常操作が同じ内容で読み出せることまでです
  （[FILESYSTEM.md](docs/FILESYSTEM.md)）。SDのUHS-Iモードは未実装です。
  ブロック単位のUSB MSC WRITE(10)は実装・実機受入済みです。かつては間欠故障の
  緩和として各WRITE前・READ 16回ごとのhost controller FIFO cleanupを必要としましたが、
  HCD側の契約（DMA buffer所有とcache同期、descriptor完了の検査、実転送長の単一化、
  cleanup失敗の伝播）を整えた結果、3構成の実機A/Bで不要と確認して撤去しました。
  複数ブロックWRITEはStage 7で2媒体×2接続構成の2／4／8 blockを各10回実機確認し、
  1回のWRITE(10)上限を8ブロック（4 KiB）へ増やしました
  （[USB_WRITE_STABILITY_PLAN.md](docs/plans/archive/USB_WRITE_STABILITY_PLAN.md)、
  [USB_BOT_HCD_REFACTOR_PLAN.md](docs/plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md)、
  [STORAGE.md](docs/STORAGE.md)）。
- Wi-FiはESP32-C6のESP-Hostedファームウェアを経由します。C6は2.4 GHz専用で
  5 GHzのAPは見えません。SoftAP、BLE、OpenThreadは未対応です
  （[WIFI.md](docs/WIFI.md)）。
- TCP/IPはsmoltcpによるIPv4です。DHCPでのアドレス取得、名前解決、ping、
  TFTP読み出し、HTTP GET／urlencoded POST、TLS 1.3クライアントまでで、**IPv6とサーバ機能は
  ありません**。`https://`はブラウザ・`hs`・`httpget`（明示スキーム時）・`tls`
  から取得できますが、接続先のidentityを保証しない**未認証TLS**です。受動的な
  盗聴は防ぎますが能動的な攻撃者は防ぎません。表示は必ず`TLS UNVERIFIED`とし、
  `SECURE`とは表示しません。SPKI pinの仕組みはありますが登録先は空です
  （[NETWORK.md](docs/NETWORK.md)、[TLS_PLAN.md](docs/plans/archive/TLS_PLAN.md)）。
  名前解決はAレコードだけで、キャッシュ・逆引き・mDNSはありません。
  受信したファイルはRAMルート上のカレントディレクトリへ保存できます
  （[NETWORK.md](docs/NETWORK.md)、[FILESYSTEM.md](docs/FILESYSTEM.md)）。
  HTTPは同期の`httpget`と、1回のpollごとに戻る`net::http::Transaction`の
  2つの顔がありますが、実装は1つです。
- `browser`はHTMLから文章とリンクを取り出して読む全画面ビューアです。
  **Webブラウザではありません**。CSSとJavaScriptはありません。PNG/JPEG画像、table、
  form（text・hidden・submit・button・checkbox・radio・textarea・select、GET／urlencoded POST）、
  `/tmp`へ保存するHTTP cache（鮮度・`ETag`再検証）を限定的に扱います。
  `https://`は取得できますが
  未認証TLSなので、toolbarは
  `TLS UNVERIFIED`を平文と同じ赤で出します。`https`→`http`のredirectは
  `https-downgrade`で拒否します。
  日本語は16 pixelのNoto Sans CJK A4 strike（32 pixel時は整数2倍）、English Latinは比例幅で表示します
  （[BROWSER.md](docs/BROWSER.md)）。
- USB-AホストはHID Bootキーボード、HID Bootマウス、1段のハブ、Mass Storageの
  読み書きまで実機確認済みです。High-Speedハブ配下Low-Speed HIDのSplit経路も
  10 ms周期で実機確認済みで、同じハブ上のHigh-Speed MSCとの併用も`ut 100`を
  retry 0で完走しています。
  デバイス情報の表示は`lsusb`（ツリー表示と記述子表示）で、文字列記述子は
  この詳細表示のときだけ取得します。多段ハブは未実装です
  （[USB.md](docs/USB.md)）。
- USB Mass Storageは挿抜に合わせて自動でマウント・アンマウントします
  （`automount off`で止まります）。CFとして扱うSDスロットには挿抜検出線が無いので、
  起動画面でだけ有限回リトライして自動マウントし、以後の自動マウント・アンマウントは
  行いません（[FILESYSTEM.md](docs/FILESYSTEM.md)）。
