# ファイル構成とコーディング方針

> 索引: [`../DESIGN.md`](../DESIGN.md)

## ファイル構成

- `src/main.rs`: 起動順の定義、グローバルアロケータ（`linked_list_allocator`の
  `LockedHeap`）の宣言とPSRAMヒープでの初期化
- `src/startup.rs`: watchdog停止、CPUクロック引き上げ、L2キャッシュ分割とRAM上限の確認
- `src/uart.rs`: USB Serial/JTAG出力。SOF割り込みのraw bitでホストの接続を判定し、
  未接続の間は出力を捨てて待たない
- `src/delay.rs`: `rdcycle`を基準にしたビジーウェイト（`delay_ms`・`delay_us`）。
  下位32 bitしか読まないため360 MHzで約11.9秒で一周する。周辺回路の短い待ちには
  十分だが秒単位の計測やプロトコルタイマの基準には使えないので、そちらは`tick.rs`
- `src/tick.rs`: SYSTIMER comparator 0の周期割り込みによる1 kHzのティックと、
  そこから作る単調な64 bitミリ秒時刻（`now_ms`）。ネットワーク専用ではなく汎用の
  時刻源で、`uptime`とsmoltcpの再送・リースタイマが利用者。ISRは「カウンタを進めて
  `INT_CLR`を書く」だけで、表示の走査に割り込まない優先度に置く
  （[`NETWORK.md`](NETWORK.md)）
- `src/psram.rs`: PSRAM、DQS調整、MMU、キャッシュ同期。32 MiBのマッピングを
  フレームバッファ（`Psram::framebuffer`）、RAMディスク（`Psram::ram_disk`、
  固定8 MiB）、ヒープ（`Psram::heap`、残り全部）の3領域へ分けて提供する
- `src/framebuffer.rs`: シングルフレームバッファと描画API
- `src/framebuffer/font.rs`: 5×7フォント
- `src/console.rs`: キーボード入力エコーとコマンドライン切り出し用コンソール。
  桁数（`COLUMNS`）だけ公開していて、`ls`の桁詰めがそれを使う
- `src/app.rs`: コンソールのフレームループ。入力、コマンド実行、全画面モードへの
  出入りだけを持つ。以下は`app`配下の、シェルコマンドを実行するためだけに存在する
  モジュール群で、クレート直下のハードウェア寄りモジュールからは参照されない
    - `src/app/shell.rs`: `console.rs`から渡されたコマンドラインを解析・実行する簡易シェル
    - `src/app/lsusb.rs`: `lsusb`コマンドの表示。ハブを介したツリー（全interfaceを含む）と、
      指定デバイスの主要な記述子。`UsbHost`のデバイスレコードを読むだけで、
      文字列記述子の取得以外はバスへ何も出さない
    - `src/app/mbr.rs`: SDカードとUSB Mass Storageで共用するMBRパーティション表示。
      読み終えたセクタをそのまま整形する`sdmbr`／`usbmbr`専用の古い表示で、
      判定は行わない
    - `src/app/files.rs`: `mount`／`umount`／`mounts`／`fsverify`／`cd`／`ls`／`cat`／
      `write`／`append`／`mkdir`の表示。`ls`の並べ替えと桁詰めもここで、
      ソートのためにエントリを一度全部集める。
      `blockdev.rs`が「媒体が何か」を出すのに対し、こちらは「そこに何があるか」を出す
    - `src/app/blockdev.rs`: `devices`／`blkread`コマンドの表示。`fs::mbr`の
      判定結果（MBR、superfloppy、ambiguous、判定不能）と各entryを出す`mbr.rs`の
      対になるモジュール
    - `src/app/membench.rs`: 内蔵SRAMとPSRAMのCPUアクセスコスト測定。`mcycle`を時間基準に、逐次スループットと1キャッシュラインあたりのレイテンシを実測する（`membench`コマンド）
    - `src/app/paint.rs`: `paint`コマンドで起動するタッチお絵描き画面
    - `src/app/touch_test.rs`: `touchtest`コマンドで起動するマルチタッチ診断画面
    - `src/app/coord_test.rs`: `coordtest`コマンドで起動する座標キャリブレーションチャート画面
    - `src/app/axis_test.rs`: `axistest`コマンドで起動するBMI270の6軸表示、水平器、傾きボール診断画面
    - `src/app/battery.rs`: `battery`／`batinfo`コマンドで起動するバッテリー電圧・電流・電力のライブ表示画面
    - `src/app/win.rs`: `win`コマンドで起動するWindows 95風デスクトップ。USB HID Bootマウスの動作テスト用。マウスカーソル、タスクバーの時計、タイトルバーのドラッグによるウィンドウ移動（内容を表示したまま移動）だけが動く
- `src/gpio.rs`: GPIO/IO_MUXのピン単位操作（オープンドレイン設定、プッシュプル出力設定、low/release/level）とGPIO Matrixの入出力ルーティング（`configure_c6_sdio_pins`はSDMMCスロット1をGPIO8..13へ配線する）
- `src/i2c.rs`: `gpio.rs`の上に実装した汎用ソフトウェアI2C（bit-bang）。物理バスごとに一つの`SoftI2c`を持ち、GPIO設定と初回バス復旧は起動時に一度だけ実行する。通常はアドレス付きの読出し・書込み・書込み後読出しをトランザクションとして提供し、可変長プロトコルだけをクロージャ型の逐次APIで扱う。SPI等の別インターフェースを追加する場合も同じ構成（`gpio.rs`の上に載せる独立モジュール）に従う
- `src/cardkb.rs`: PORT.AのCardKBドライバ（`i2c.rs`のI2Cバスを使用）
- `src/tab5_keyboard.rs`: Ext.Port1（GPIO0/1）のTab5 KeyboardをHIDモードで読むI2Cドライバ。HID usage IDを`input.rs`の共通変換へ渡す
- `src/input.rs`: CardKB、Tab5 Keyboard、USBキーボード、USBマウス、タッチを統合する`InputManager`、再接続管理、キーイベント、全画面モードが共通で使うキー待ち（`wait_for_key`）を持つ。USBマウスは`poll_mouse`で生の`usb::MouseUpdate`を渡し、相対移動量を位置にするカーソルは描画側（`app::win`）が持つ。タッチはドライバを隠した論理座標の全接触点と、追加接触を無視して最初の接触を追跡する1本指ポインタ位相を提供する
- `src/touch.rs`: GT911／ST7121・ST7123タッチコントローラードライバ（`i2c.rs`のI2Cバスを使用、[`INPUT.md`](INPUT.md)）。接触ID・ST7121/ST7123の固定レポートスロットなどのハードウェア差はここに閉じ、初期化・再接続とアプリ向けの抽象化は`InputManager`が行う
- `src/power.rs`: E2 P4（`PWROFF_PULSE`）を用いたTab5全体の電源断要求
- `src/bmi270.rs`: Tab5内蔵BMI270のソフトウェアI2C初期化、ファームウェア転送、設定、6軸生データ読出し
- `src/ina226.rs`: INA226の識別、連続測定設定、5 mΩシャント向け較正、電圧・電流の読出しと換算
- `src/rtc.rs`: RX8130CE（`0x32`）のカレンダー読み書き、BCDと週ビットフィールドの検証、フラグ・制御レジスタの読出し（`rtc`コマンド）
- `src/lcd.rs`: I/O expander（`i2c.rs`のI2Cバスを使用）、D-PHY、パネル、DSI Bridge、DW-GDMA
- `src/lcd/st7121.rs`: パネル初期化コマンド
- `src/interrupts.rs`: 共通CLICトラップ入口、表示用DW-GDMA ISR、USB-A High-Speed DWC
  とSYSTIMERティックの割り込みディスパッチ。CPU外部線は「遅れたときの痛さ」順で、
  線1が表示、線2がUSB、線3が`tick.rs`
- `src/icm.rs`: システムAXIインターコネクトの調停優先度。表示DMAのPSRAM読み出しを最優先にしてDSI BridgeのFIFOアンダーラン（水色フラッシュ）を防ぐ。2D-DMAマスターは逆に最低優先度へ明示的に固定する
- `src/dma2d.rs`: 2D-DMA。矩形ブロックを単位に転送するエンジンで、ディスクリプタが
  「画像の中のブロック」を表すためCW回転した配置をそのまま扱える。クロック投入、
  チャネル設定、完了待ち、メモリ間ブロックコピー（M2M、スクロールに使用）
- `src/ppa.rs`: PPAのBlendエンジンによる矩形の単色塗り。PPA自身はDMAを持たないので
  `dma2d.rs`のRXチャネルと組で動く。全画面クリアと大きい矩形の塗りがここを通る
- `src/pma.rs`: ESP32-P4固有のPMA CSRを読み出し、TOR／NA4／NAPOTの設定語を
  アドレス範囲と属性へ復元する読み取り専用デコーダ。`pma`シェルコマンドが使用する
- `src/pmp.rs`: 標準RISC-VのPMP CSRを読み出し、範囲とR/W/X・ロックへ復元する
  読み取り専用デコーダ。`pma.rs`と対になるが、設定バイトが`pmpcfgN`に4エントリずつ
  詰め込まれている点が違う。`pmp`シェルコマンドが使用する
- `src/sdmmc.rs`: SDHOSTコントローラー初期化、SDカード活性化、DMA（IDMAC）
  経由のブロック読み書き。`gpio.rs`は使わずIO_MUXを直接操作する点は`psram.rs`と
  同じ構成。現状は[`STORAGE.md`](STORAGE.md)、実機で踏んだ罠は
  [`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照。コントローラーは1つでカード
  （スロット）が2つあり、カード0がmicroSD、カード1がESP32-C6。カード番号を取る
  低レベルAPI（`init_host`／`send_command_on`／`set_clock`／
  `set_host_bus_width_4bit`）を`sdio.rs`へ公開する
- `src/sdio.rs`: SDMMCカード1に載るESP32-C6のSDIOカードとしての活性化
  （電源E2.P0、GPIO15リセット、CMD52／CMD5／CMD3／CMD7、CCCR設定、CIS読み出し）と、
  CMD52の1バイトアクセス・CMD53のブロック／バイトモード転送。ピンはGPIO Matrix
  経由なので`gpio.rs`の`configure_c6_sdio_pins`を使う。計画と実機での判断は
  [`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md)を参照
- `src/wifi.rs`・`src/wifi/`: ESP32-C6経由のWi-Fi。`usb.rs`と同じく親ファイルは
  サブモジュール宣言と再エクスポートだけを持つ
    - `src/wifi/hosted.rs`: ESP-Hostedのトランスポート層。12 byteペイロード
      ヘッダの組み立てと検査、スレーブレジスタ（受信長・送信バッファトークン・
      割り込み）、CMD53による1フレームの送受信、スレーブ初期化イベントの受信と
      ホスト設定の返送。1回の読み出しに複数フレームが載るため、バッファを
      使い切るまでバスに触らずフレームを取り出す
    - `src/wifi/proto.rs`: RPCメッセージに必要な範囲だけのprotobuf。varint、
      length-delimited、入れ子だけを扱う`Writer`と`Reader`で、未知のフィールドは
      wire typeを見て読み飛ばす
    - `src/wifi/rpc.rs`: SERIALインターフェース上のRPC。TLVエンベロープ
      （`RPCRsp`／`RPCEvt`）と`Rpc`メッセージ（msg_type／msg_id／uid＋
      msg_id番のフィールドに入るペイロード）の組み立てと解析、
      フレーム長を超えるメッセージの分割と再結合、応答待ちの間に届いた
      イベントの保持
    - `src/wifi/station.rs`: RPCの上に載るWi-Fi操作。`esp_wifi_init`／
      モード設定／開始、スキャンの実行とAPレコードの解析、STA設定と接続・
      切断、接続結果イベントの待ち受け。スレーブが返す`esp_err_t`は
      握りつぶさずそのまま返す
- `src/fs.rs`・`src/fs/`: ファイルシステム層。`usb.rs`と同じく親ファイルは
  サブモジュール宣言と再エクスポートだけ。現状はブロックデバイス層とMBR判定まで
  （[FILESYSTEM_PLAN.md](FILESYSTEM_PLAN.md)のStage 1、現状は[STORAGE.md](STORAGE.md)）
    - `src/fs/block.rs`: 全媒体共通の同期`BlockDevice` trait、`BlockGeometry`、
      共通エラー`BlockError`、範囲検査。読み取り専用と読み書きでtraitを分けない
    - `src/fs/ramdisk.rs`: PSRAM固定領域上のRAMディスク。唯一の書き込み可能な媒体で、
      DMAが触らないのでキャッシュ操作は要らない
    - `src/fs/sd.rs`・`src/fs/usb_msc.rs`: `sdmmc.rs`／`usb/msc.rs`の上に載る
      adapter。媒体固有の処理は下層に残し、結果の変換・転送分割・書き込み抑止だけを持つ。
      USB側はセッションを`UsbHost`から借りる一時的なviewである点がSD側と違う
    - `src/fs/bootsector.rs`: FAT/exFATブートセクタの妥当性検査。MBR判定と
      将来のFSドライバの両方が使う
    - `src/fs/mbr.rs`: LBA 0がMBRなのかsuperfloppyなのかの判定と、primary entryの
      検査・列挙。両方成立した場合は拒否する
    - `src/fs/partition.rs`: パーティション範囲のデータ（`PartitionRange`）と、
      I/Oの間だけデバイスを借りる`PartitionBlockDevice`
    - `src/fs/registry.rs`: `DeviceId`と、名前から実デバイスを解決する`Devices`
    - `src/fs/clock.rs`: FATのタイムスタンプ源。RTCを操作ごとに1回だけ読んで
      atomicへ置き、ライブラリの`&'static`プロバイダがそれを読む。未設定時は0
      （FATの「タイムスタンプ無し」）
    - `src/fs/format.rs`: FAT16のformat。起動時にRAMディスクへ書く
    - `src/fs/seed.rs`: 読み出し検証用ファイル（短名・LFN・複数クラスタ）を
      RAMディスクへ直接書く。ライブラリの書き込み経路を使わないので`write`
      featureをビルドから外したままにできる
    - `src/fs/stream.rs`: `BlockDevice`の上のbyte単位`Read`/`Seek`と、
      マウントあたり4 KiBの連続区間セクタキャッシュ。FATライブラリと
      ブロック層の唯一の境界
    - `src/fs/fingerprint.rs`: 媒体の同一性確認。SDのCID、SCSIのINQUIRYとVPD、
      MBRのdisk signatureとパーティション表、ボリュームのブートセクタを畳み込む。
      「どの媒体か」ではなく「さっきと同じ媒体か」に答える
    - `src/fs/path.rs`: 絶対パスの正規化、長さと文字の検査、FAT流の名前比較、
      シェルのカレントディレクトリと相対パスの連結（`join`）
    - `src/fs/vfs.rs`: マウント表、パス解決、ファイルハンドル、ディレクトリ列挙、
      `metadata`と`create_dir`。マウントはファイルシステムを保持せず、操作のたびに
      開き直す（[FILESYSTEM.md](FILESYSTEM.md)）
- `src/net.rs`・`src/net/`: smoltcpによるIPv4。`usb.rs`・`wifi.rs`と同じく親ファイルは
  サブモジュール宣言と再エクスポートだけ。プロトコル層を自前実装しない唯一の層で、
  理由と対応範囲は[`NETWORK.md`](NETWORK.md)
    - `src/net/device.rs`: smoltcpの`phy::Device`実装。`wifi::Rpc`の受信キューから
      1フレーム取る`RxToken`（フレームを所有するので送信トークンと同時に返せる）と、
      `Rpc::send_station_frame`へ流す`TxToken`。medium・MTUの申告もここ
    - `src/net/stack.rs`: `Interface`・`SocketSet`・DHCPソケット・DNSソケットの保持、
      アドレス・既定経路・リゾルバの適用、リンクを読んでインタフェースに捌かせるポンプ
      （`poll`／`pump_until`）。C6のリンクは所有せず、呼び出しごとに`&mut Rpc`を借りる
    - `src/net/dns.rs`: 名前解決（Aレコード）。ソケットは`stack.rs`が常設で持つので、
      ここにあるのは「問い合わせを始めて、settleするまでポンプして、答えを1回だけ
      取り出す」駆動だけ
    - `src/net/ping.rs`: ICMP echoの送信と往復時間の測定。こちら宛のechoへの応答は
      smoltcpの`auto-icmp-echo-reply`が行うのでここにはない
    - `src/net/tftp.rs`: TFTP読み出しクライアント（RFC 1350、512 byteロックステップ、
      オプション拡張なし）とCRC-32
    - `src/net/http.rs`: TCP確認用の最小HTTP/1.0 GET
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
      （`run_packet`）。USB割り込みのmask／ack、channel 0、periodic channel 1〜4とroot-portのAtomic診断
      スナップショットも持つ。通常転送は世代付き固定slotへsubmitし、競合防止付き`WFI`後に
      reapする。periodic slotを確保できない追加HID／Split HIDのidle NAKだけ短いbounded pollで
      回収する。channel 1を使うperiodic Interrupt INの旧Go/No-Go診断、およびchannel 1〜4で共有する
      static frame list、channel別QTD／report buffer bankとallocatorもここに置く。
      FS/LS-only host modeは既定offのruntime診断設定として保持し、`usbfs`が切替え後に再列挙する。
      Split buffer DMAは排他modeで実行し、periodic descriptor DMAがactiveなら切替えを拒否する。
      レジスタ・チャネル・パケットのことだけを知っており、
      USBデバイスや記述子の意味は一切知らない
    - `src/usb/protocol.rs`: 汎用USBプロトコル層（Stage 2相当）。コントロール
      転送（SETUP/DATA/STATUS）の組み立てと標準記述子（USB2.0 chapter 9）に
      よる列挙。デバイスクラスについては何も知らない。記述子チェーンの走査
      （`descriptors`）とinterface／endpoint記述子の解釈、要求されたときだけ
      読む文字列記述子（UTF-16LE→ASCII）もここ
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
      直結デバイス、または1段のハブの全ポートを列挙し、キーボードとMSCのハンドルを保持。
      「バスに何があるか」と「何を駆動できるか」は別の配列で、`records`が列挙できた
      全デバイス（ハブ自身と未対応クラスを含む、記述子の生バイトつき）、`slots`が
      クラスドライバ。ポーリングと判断は`slots`、`lsusb`の表示は`records`を読む
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
