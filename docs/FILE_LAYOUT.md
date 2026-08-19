# ファイル構成とコーディング方針

> 索引: [`../DESIGN.md`](../DESIGN.md)

## ファイル構成

- `src/main.rs`: 起動順の定義、グローバルアロケータ（`linked_list_allocator`の
  `LockedHeap`）の宣言とPSRAMヒープでの初期化
- `src/startup.rs`: watchdog停止、CPUクロック引き上げ、L2キャッシュ分割とRAM上限の確認
- `src/uart.rs`: USB Serial/JTAG出力
- `src/delay.rs`: `rdcycle`を基準にしたビジーウェイト（`delay_ms`・`delay_us`）
- `src/psram.rs`: PSRAM、DQS調整、MMU、キャッシュ同期。フレームバッファと
  ヒープ用領域（`Psram::heap`）の両方を提供
- `src/framebuffer.rs`: シングルフレームバッファと描画API
- `src/framebuffer/font.rs`: 5×7フォント
- `src/console.rs`: キーボード入力エコーとコマンドライン切り出し用コンソール
- `src/app.rs`: コンソールのフレームループ。入力、コマンド実行、全画面モードへの
  出入りだけを持つ。以下は`app`配下の、シェルコマンドを実行するためだけに存在する
  モジュール群で、クレート直下のハードウェア寄りモジュールからは参照されない
    - `src/app/shell.rs`: `console.rs`から渡されたコマンドラインを解析・実行する簡易シェル
    - `src/app/mbr.rs`: SDカードとUSB Mass Storageで共用するMBRパーティション表示
    - `src/app/membench.rs`: 内蔵SRAMとPSRAMのCPUアクセスコスト測定。`mcycle`を時間基準に、逐次スループットと1キャッシュラインあたりのレイテンシを実測する（`membench`コマンド）
    - `src/app/paint.rs`: `paint`コマンドで起動するタッチお絵描き画面
    - `src/app/touch_test.rs`: `touchtest`コマンドで起動するマルチタッチ診断画面
    - `src/app/coord_test.rs`: `coordtest`コマンドで起動する座標キャリブレーションチャート画面
    - `src/app/axis_test.rs`: `axistest`コマンドで起動するBMI270の6軸表示、水平器、傾きボール診断画面
    - `src/app/battery.rs`: `battery`／`batinfo`コマンドで起動するバッテリー電圧・電流・電力のライブ表示画面
    - `src/app/win.rs`: `win`コマンドで起動するWindows 95風デスクトップ。USB HID Bootマウスの動作テスト用。マウスカーソル、タスクバーの時計、タイトルバーのドラッグによるウィンドウ移動（Win95と同じ枠線ドラッグ）だけが動く
- `src/gpio.rs`: GPIO/IO_MUXのピン単位操作（オープンドレイン設定、low/release/level）
- `src/i2c.rs`: `gpio.rs`の上に実装した汎用ソフトウェアI2C（bit-bang）。物理バスごとに一つの`SoftI2c`を持ち、GPIO設定と初回バス復旧は起動時に一度だけ実行する。通常はアドレス付きの読出し・書込み・書込み後読出しをトランザクションとして提供し、可変長プロトコルだけをクロージャ型の逐次APIで扱う。SPI等の別インターフェースを追加する場合も同じ構成（`gpio.rs`の上に載せる独立モジュール）に従う
- `src/cardkb.rs`: PORT.AのCardKBドライバ（`i2c.rs`のI2Cバスを使用）
- `src/input.rs`: CardKBとUSBキーボードを統合する`InputManager`、再接続管理、キーイベント、全画面モードが共通で使うキー待ち（`wait_for_key`）、およびUSBマウスの移動量の受け渡し（`poll_mouse`）。キーは`Key`へ正規化するがポインタは正規化せず`usb::MouseUpdate`をそのまま渡す。相対移動量が位置になるのは「何の上を動くか」を決めた側なので、カーソル位置は描画側（`app::win`）が持つ
- `src/touch.rs`: GT911／ST7121・ST7123タッチコントローラードライバ（`i2c.rs`のI2Cバスを使用、[`INPUT.md`](INPUT.md)）
- `src/power.rs`: E2 P4（`PWROFF_PULSE`）を用いたTab5全体の電源断要求
- `src/bmi270.rs`: Tab5内蔵BMI270のソフトウェアI2C初期化、ファームウェア転送、設定、6軸生データ読出し
- `src/ina226.rs`: INA226の識別、連続測定設定、5 mΩシャント向け較正、電圧・電流の読出しと換算
- `src/rtc.rs`: RX8130CE（`0x32`）のカレンダー読み書き、BCDと週ビットフィールドの検証、フラグ・制御レジスタの読出し（`rtc`コマンド）
- `src/lcd.rs`: I/O expander（`i2c.rs`のI2Cバスを使用）、D-PHY、パネル、DSI Bridge、DW-GDMA
- `src/lcd/st7121.rs`: パネル初期化コマンド
- `src/interrupts.rs`: CLICトラップ入口とGDMA ISR
- `src/icm.rs`: システムAXIインターコネクトの調停優先度。表示DMAのPSRAM読み出しを最優先にしてDSI BridgeのFIFOアンダーラン（水色フラッシュ）を防ぐ。2D-DMAマスターは逆に最低優先度へ明示的に固定する
- `src/dma2d.rs`: 2D-DMA。矩形ブロックを単位に転送するエンジンで、ディスクリプタが
  「画像の中のブロック」を表すためCW回転した配置をそのまま扱える。クロック投入、
  チャネル設定、完了待ち、メモリ間ブロックコピー（M2M、スクロールに使用）
- `src/ppa.rs`: PPAのBlendエンジンによる矩形の単色塗り。PPA自身はDMAを持たないので
  `dma2d.rs`のRXチャネルと組で動く。全画面クリアと大きい矩形の塗りがここを通る
- `src/pma.rs`: ESP32-P4固有のPMA CSRを読み出し、TOR／NA4／NAPOTの設定語を
  アドレス範囲と属性へ復元する読み取り専用デコーダ。`pma`シェルコマンドが使用する
- `src/sdmmc.rs`: SDHOSTコントローラー初期化、SDカード活性化、DMA（IDMAC）
  経由のブロック読み書き。`gpio.rs`は使わずIO_MUXを直接操作する点は`psram.rs`と
  同じ構成。現状は[`STORAGE.md`](STORAGE.md)、実機で踏んだ罠は
  [`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照
- `src/usb.rs`・`src/usb/`: USB-Aホスト。`lcd.rs`/`lcd/st7121.rs`と同じ
  「親ファイルがサブモジュールを`mod`宣言し、実体は`src/usb/`以下」という構成。
  親の`usb.rs`はサブモジュール宣言と、他ファイルが使う型・関数の再エクスポート
  だけを持つ薄いファイル。ホストコントローラー、USBプロトコル、クラスドライバ、
  それらを所有するデバイスレジストリに分離している
    - `src/usb/hcd.rs`: ESP32-P4 High-Speed USB-DWCホストコントローラー
      ドライバー（Stage 1相当）。VBUS電源（`i2c.rs`のI2Cバス経由で
      PI4IOE5V6408、2個目、0x44を叩く）、および同expanderのビット単位
      read-modify-write、コア初期化・ホストポート電源投入・
      接続検出・リセット・速度判定、チャネル/パケット実行のプリミティブ
      （`run_packet`）。レジスタ・チャネル・パケットのことだけを知っており、
      USBデバイスや記述子の意味は一切知らない
    - `src/usb/protocol.rs`: 汎用USBプロトコル層（Stage 2相当）。コントロール
      転送（SETUP/DATA/STATUS）の組み立てと標準記述子（USB2.0 chapter 9）に
      よる列挙。デバイスクラスについては何も知らない
    - `src/usb/hid.rs`: HID 1.11 Boot Protocolのうち、どのブートデバイスでも
      共通の部分。`SET_CONFIGURATION`／`SET_PROTOCOL(Boot)`／`SET_IDLE(0)`の
      手順、コンフィグレーション記述子からブートインターフェースを探す走査、
      Interrupt INのレポート読み出しセッション（データトグル、NAKと本物の
      転送エラーの区別、再列挙の判断）
    - `src/usb/hid_keyboard.rs`: HID Bootキーボードのクラスドライバー
      （Stage 3相当）。`hid.rs`の上で、キーボード固有のレポート差分と
      キーコード変換だけを持つ`UsbKeyboard`
      （`InputManager`から`CardKb`と並列にポーリングされる）
    - `src/usb/hid_mouse.rs`: HID Bootマウスのクラスドライバー。`hid.rs`の上に
      載る`hid_keyboard.rs`の兄弟。ブートマウスのレポートは前回からの*相対*移動量と
      ボタンの状態なので、キーボードのように差分を取るのではなく加算する。1
      フレーム（約57 Hz）の間にマウス（多くは125 Hz）は複数回報告するため、
      `poll`は待機中のレポートを全て読み切って合算する
    - `src/usb/hub.rs`: USBハブのクラスドライバー。ディスクリプタ取得、ポート電源、
      接続検出、リセット、速度判定を担当
    - `src/usb/bot.rs`: USB Mass StorageのBulk-Only Transport。Bulk IN/OUT、
      endpointごとのデータトグル、CBW/CSW、STALL回復を実装し、`msc.rs`が利用する
    - `src/usb/msc.rs`: SCSI Transparent USB Mass StorageのSCSI読み込みコマンドを
      実装するクラスドライバー。BOTの転送処理は`bot.rs`へ委譲する
    - `src/usb/floppy.rs`: 中断したUFI/CBI USB Floppyのクラスドライバー試作。記述子検出、
      CBIの制御転送、固定1.44 MB FAT12メディア認識を実装するが、現在は`usb.rs`から
      読み込まれず、レジストリも選択しない
    - `src/usb/registry.rs`: USBバスの単一オーナーである`UsbHost`とデバイスレジストリ。
      直結デバイス、または1段のハブの全ポートを列挙し、キーボードとMSCのハンドルを保持
  現状は[`USB.md`](USB.md)、段階分けと実装上の判断は
  [`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)を参照
- `memory.x`: ESP32-P4用メモリとイメージ配置
- `.cargo/config.toml`: ターゲット、リンカー、`partitions.csv`の`factory`アプリを
  選ぶ`espflash` runner
- `partitions.csv`: Tab5の16 MiB SPI Flash向けESP-IDF互換パーティション表
- `tools/check_elf_layout.py`: release ELFのXIP/IRAM/DRAM配置、critical relocation、
  RAM範囲、stack下限を検査
- `tools/check_esp_image.py`: `espflash save-image`後のXIPセグメント数、appdesc、
  物理・仮想64 KiBページ内オフセットを検査
- `tools/monitor.py`: USB Serial/JTAGの再列挙をまたいでログを追い続けるモニタ
- [`FLASH_XIP_MIGRATION_PLAN.md`](FLASH_XIP_MIGRATION_PLAN.md): XIP移行のStage、判断、実測結果

`esp-idf-reference/`には、レジスタ設定との比較に使用したESP-IDF v5.5.3版の
参照実装があります。

## コーディング方針

`src/`以下のコードコメント（`//`・`///`・`//!`）はすべて英語で書きます。
`DESIGN.md`と`docs/`以下、`README.md`など、人間向けドキュメントは日本語のままです。

各ファイル末尾の`read`/`write`/`modify`（任意の`usize`アドレスを読み書きする
MMIOプリミティブ）は`unsafe fn`として定義します。呼び出し元の`address`が
有効なレジスタである保証はシグネチャからは得られないため、これはRustの
安全性の観点で本来unsafeであるべき操作です。一方、これらを呼び出す各関数
（`enable_dsi_clock`など）は、既知のハードウェア定数アドレスしか渡さない
ことでその安全性を担保するので、`unsafe fn`にはせず、関数内で`unsafe { ... }`
ブロックにまとめて使います（呼び出し1つずつを`unsafe`で囲むのではなく、
関数単位でまとめるのが方針です）。

`README.md`は人間がメンテします。AIは指示された場合を除き編集しないでください。
このルールと設計資料への入口は[`AGENTS.md`](../AGENTS.md)に、同じREADME管理ルールは
[`CLAUDE.md`](../CLAUDE.md)にも書いてあります（DESIGN.mdは自動では読み込まれないため）。
`.claude/settings.json`の`permissions.ask`でも、README.mdへの
`Edit`/`Write`に確認を挟むようにしてあります。
