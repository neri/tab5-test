# USB-Aホスト機能 実装計画

## 方針

`DESIGN.md`の方針（ESP-IDF/RTOSをリンクせずレジスタ操作で実装、1機能=1モジュール=
実機確認可能な単位でコミット）を踏襲する。[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)と
同様、USBホストも「コントローラ初期化」「デバイス接続検出」「コントロール転送
（列挙）」「クラス固有プロトコル（HID Boot）」の層ごとに実機依存の罠が異なるため、
一気に実装せず層ごとに動作確認しながら進める。マイルストーンはHID BOOTキーボード
（`console.rs`へキー入力を渡し、CardKBと同様にコンソールへエコーできること）とし、
他デバイスクラス（マウス、MSC、複数デバイス用ハブ）は将来検討として範囲外とする。

参考実装はESP-IDFの`usb`コンポーネント（`hcd_dwc.c`、`usb_host.c`、`enum.c`、
`hub.c`）と`hal`コンポーネントの`usb_dwc_hal.c`/`usb_dwc_ll.h`（`esp32p4`向け）を
レジスタ・シーケンスの照合先として使う（リンクはしない）。これらはFreeRTOS上の
タスク・イベントキューを前提にしているため、本プロジェクトでは同じレジスタ操作を
`lcd::run_console`のフレームループに乗せるポーリング方式へ書き直す必要がある
（後述）。

## Stage 0: ハードウェア構成の確認

### ESP32-P4側（データシート・公開ドキュメントで確認済み）

ESP32-P4は独立した2系統のUSB 2.0 OTGコントローラを持ち、それぞれ単独でホストと
して動作できる。

- **USB OTG High-Speed**: 内蔵UTMI PHY、DM/DPは専用ピン（GPIO番号を持たない、
  GPIO Matrix/IOMUX非経由）。480 Mbps対応。
- **USB OTG Full-Speed**: 既定でGPIO26(D-)/GPIO27(D+)。12 Mbps止まり。
- **USB Serial/JTAG**（本プロジェクトが書き込み・UARTログに使用中、既存の
  `src/uart.rs`）: 既定でGPIO24(D-)/GPIO25(D+)。上記2つのOTGコントローラとは
  別ピン・別ハードウェアブロックであり、本機能追加による干渉はない。

ECO2（chip revision v1.3、`DESIGN.md`記載の対象個体）ではHigh-Speed OTGのDPラインに
過渡電流対策の1 MΩプルダウンが基板側で必要とされる（Espressifのスキーマチック
チェックリストに記載、v3.0以降で内部修正済み）。本プロジェクトはTab5基板の設計には
関与しないため、Tab5側で対応済みという前提で進める（実機で問題が出た場合のみ
疑う）。

### Tab5基板側 ✅ VBUSビットは実機確認済み

M5Stackの製品説明およびユーザーデモ（`m5stack/M5Tab5-UserDemo`）の記述から、
Tab5のUSB-AポートはHost専用（OTGのロール切り替えなし）で、High-Speed OTG
コントローラ（専用DM/DPピン）に配線されていると考えられる。USB-Cポートは
Full-Speed OTGおよび/または書き込み用USB Serial/JTAGに配線されており、本機能とは
別系統。

VBUSの5V供給はソフトウェアでのON/OFFが必要。既存の`src/lcd.rs`がPI4IOE1
（I2Cアドレス`0x43`、LCDリセット用）を叩いているのと対になる2個目の
PI4IOE5V6408拡張IC（アドレス`0x44`、便宜上PI4IOE2と呼ぶ）のbit 3が
USB-Aの5V有効化に割り当てられていることを、`usbvbus`コマンドでの実機トグル＋
テスター実測で確認した（`src/usb.rs`の`VBUS_ENABLE_BIT = 3`）。公開情報では
`OUT_SET`のbit3説と`P0`説の両方があったが、bit3が正しかった。

- ゴール: USB-Aコネクタの5V端子（VBUS）を`i2c.rs`経由のPI4IOE2書き込みで
  ON/OFFできることをテスターまたは市販USB電流チェッカーで確認する。
  → **完了（実機確認済み）**

## Stage 1: DWC OTGコア初期化とデバイス接続検出 ✅ 完了（実機確認済み）

`hcd_dwc.c`/`usb_dwc_hal.c`のホストモード初期化（コアリセット、PHY設定、
FIFOサイズ設定、ホストポート電源ON）を移植する。RTOSのイベントキューは
使わず、ポート状態レジスタをポーリングする形にする（`sdmmc.rs`が割り込み
ではなく`RINTSTS`ポーリングで完了判定しているのと同じ方針。まずポーリングで
動作確認し、必要になった時点でのみ割り込み化を検討する）。

- コアAHB/USBコンフィグレジスタの設定、ホストモード選択
- FIFOサイズ（RXFIFO/NPTXFIFO/PTXFIFO）の静的割り当て（`usb_dwc_hal.h`の
  `usb_dwc_hal_fifo_config_t`を参考に、HID Boot 1デバイス分の最小構成でよい）
- Stage 0で確認したVBUS ONを行った上でホストポートをpower on
- 接続検出（`HPRT`のポート接続ビット）→ 最低100ms程度のデバウンス
  →ポートリセット（USB2.0規格上10ms以上）→リセット解除後のポート速度
  （Low/Full/High-Speed）読み取り
- 切断検出（ポート接続ビットが落ちた場合）。CardKBの「未接続なら約1秒ごとに
  再検出」と同じ寛容さで、抜き差しに対してクラッシュしないことを最低条件とする
- ゴール: `shell.rs`に`usbinfo`のようなコマンドを追加し、USB-Aにキーボード等
  何かを挿した状態でポート速度（Low/Full-Speed想定、HIDキーボードなので
  High-Speedにはならない）と接続状態がUARTログ/コンソールに出ることを確認する
  → **完了。実機でHID Bootキーボードを接続し、`usbinfo`でポート接続・
  リセット・enableまで実機確認済み**

## Stage 2: コントロール転送（デバイス列挙） ✅ 完了（実機確認済み）

チャネル0を使い、デフォルトアドレス（0）・EP0でのコントロール転送を実装した
（`src/usb.rs`の`enumerate_device`。Scatter/Gather DMAのQTD 1個・1パケット
ずつを`hcd_dwc.c`/`usb_dwc_hal.c`/`usb_dwc_ll.h`のレジスタ操作に1:1で
対応させ、都度チャネルhalt待ちでポーリングする方式）。HIDキーボードは
Low/Full-Speedデバイスのため、単一チャネル・単一デバイスの範囲に限定し、
複数チャネル同時発行やハブ経由のsplit transactionは範囲外とした。

- `GET_DESCRIPTOR`(Device, 8byteのみ)でEP0の`bMaxPacketSize0`を取得
- `SET_ADDRESS`でデバイスアドレスを1に設定（以後EP0はこのアドレス宛）
- `GET_DESCRIPTOR`(Device, フル18byte)でVID/PID/クラスを取得
- `GET_DESCRIPTOR`(Configuration)でHIDインターフェース記述子・
  HID記述子・エンドポイント記述子（Interrupt IN、通常8byte前後の
  `wMaxPacketSize`）を取得し、インターフェース番号とエンドポイントアドレスを
  控える
- HIDクラスでない、または複数インターフェースを持つ複合デバイスの場合は、
  最初に見つかったHIDキーボード（`bInterfaceClass=3`、
  `bInterfaceSubClass=1` Boot、`bInterfaceProtocol=1` Keyboard）だけを対象とし、
  それ以外は無視する（マウス等の他インターフェースは将来検討）
- ゴール: `usbinfo`（またはenum専用コマンド）がVID/PID、製品名（可能なら
  `GET_DESCRIPTOR`(String)まで）、検出したHIDキーボードのインターフェース
  番号・エンドポイントアドレスを表示する
  → **完了。実機のHID Bootキーボードで`usbinfo`がVID/PID・クラス・
  configバイト数・HIDインターフェース番号・Interrupt INエンドポイント
  アドレスまで表示することを確認済み。文字列記述子（製品名）の取得は
  未実装のまま（optional扱い）**

**実機で確認できた前提（Stage 2着手時点では未確認としていたもの）**:
- HCINTは割り込みを一切unmaskしていない状態でもステータスビット自体が
  更新される（マスクは割り込み信号の伝播だけを止める）という前提で
  ポーリングしており、これはStage 1のHPRTポーリングと合わせて実機で
  正しく動作した
- Scatter/Gather DMAモードの非periodicチャネルはNAKをハードウェアが
  自動リトライするという前提（`usb_dwc_hal.c`の`CHAN_INTRS_EN_MSK`に
  NAKが含まれないことから推定）も実機の列挙成功で裏付けられた
- QTDリストの512byteアライメント必須という理解（`HCDMAi.dmaaddr`の
  ビットパッキング仕様）も、`#[repr(C, align(512))]`のローカル変数
  （`sdmmc.rs`の`#[repr(C, align(64))] struct Descriptor`と同じ手法）で
  問題なく機能した

## Stage 3: HID Boot Protocolキーボード（マイルストーン本体） ✅ キー入力は完了（実機確認済み）

- `SET_CONFIGURATION`でStage 2で見つけた構成を有効化
- HIDクラス固有リクエスト`SET_PROTOCOL`(Boot Protocol=0)を送り、レポート形式を
  Boot Keyboard固定フォーマット（8byte: モディファイヤ1byte + reserved1byte +
  キーコード6byte）に固定する
- `SET_IDLE`(0)でキーが押されっぱなしの間の自動再送を無効化し、状態変化時
  のみレポートさせる（省略してポーリングのみで済ませる案もあるが、まず
  ESP-IDFに倣って設定する）
- 対象エンドポイント（Interrupt IN）へ周期的にトランザクションを発行し、
  レポートを取得する。デバイスがまだデータを持たない間の`NAK`は正常応答と
  して扱い、`lcd::run_console`のフレームループから毎フレーム（または
  数フレームおき）ポーリングする形にする（専用タスク/割り込み駆動は
  行わない。CardKBが同じループ内でI2Cポーリングしているのと同じ構成）
- 8byteレポートの変化を検出したら、USB HID Usage ID（Boot Keyboardの
  キーコード表）から`console.rs`が受け付けるASCII相当へ変換するテーブルを
  実装する（`cardkb.rs`がCardKB独自のキーコードをASCIIへ変換しているのと
  同じ位置付けのモジュール）。まずは英数字・スペース・Enter・Backspace・
  Tab・矢印なしの範囲（既存コンソールが対応する文字種）に絞り、Shift
  （モディファイヤビット）で大文字/記号を切り替える
- 変換したキー入力は、CardKBと同じ経路（`Console::push`）で流し込む。
  CardKBとUSBキーボードを同時に挿していても両方の入力がコンソールへ
  反映される（`lcd.rs`のループで両方をポーリングするだけでよい設計とする）
- 抜き差し耐性: 列挙中の失敗、途中切断は`Option`を使ってCardKBと同様
  「未接続として扱い次のポーリングで再列挙を試みる」形にする。クラッシュ
  ではなくログのみで継続することを必須条件とする
- ゴール: 実機でUSB-AにHID Boot対応キーボード（一般的なUSBキーボードは
  ほぼ全てBoot Protocol対応）を接続し、キー入力が画面上のコンソールへ
  CardKBと同様にエコーされることを確認する。CardKB接続時との共存も確認する
  → **完了。`src/usb.rs`の`UsbKeyboard`として実装し、`lcd.rs`にCardKBと
  並列のポーリング・再接続ロジックを追加。実機のHID Bootキーボードで
  キー入力がコンソールへエコーされることを確認した（下記「踏んだ罠」の
  2件を修正した後）。CardKBとの同時接続時の共存は未確認**

**実装上、計画時点から具体化した判断**:
- Interrupt転送はperiodic schedulerを使わず、Stage 2のcontrol転送と同じ
  「チャネル0を1パケット分だけ都度activateしてhalt待ちpoll」方式を、
  対象エンドポイント番号で流用している（frame list等のperiodic
  scheduling基盤は未実装のまま）。NAK（＝まだ新しいレポートが無い）を
  打ち切るための短いタイムアウト（`INTERRUPT_POLL_TIMEOUT_ITERATIONS`）
  と、それでも`CHENA`が下りない場合の明示的なhalt要求
  （`force_halt_channel`、`HCCHAR.CHDIS`）を追加している。このタイムアウトは
  「まだキーが無い」を意味する正常系なので、ログは出さない
  （`run_packet`の`quiet_timeout`）。フレームループ（約57Hz）から毎フレーム
  呼ぶため、この経路がログを出すと毎秒何十行も出てしまう
- **踏んだ罠（実機、検証中）**: 初回実装では`HCCHAR.eptype`を素直に
  `INTR`（3）にしていたが、実機で「列挙は成功するがキー入力に一切
  反応しない」という壊れ方をした。DWC OTGのScatter/Gather DMAモードでは
  periodic（INTR/ISOC）channelがフレームリストのスケジューリング対象に
  登録されていないと、`CHENA`を直接立てただけではコアがそもそも
  トランザクションを試みない可能性がある、というのが現時点の仮説
  （frame list/HFLBAddr/`HCFG.PerSchedEna`はStage 1で明示的に未設定・
  無効のまま）。frame listを実装する代わりに、`HCCHAR.eptype`を`BULK`
  （2）にして同じ「都度activate・poll」方式を流用する回避策に変更した。
  FS/LSでは素のIN token自体はホスト側の内部チャネル分類に関わらず同一
  （SETUPだけが別トークン種別を持つ）なので、デバイス側は自分の
  エンドポイント記述子どおりInterrupt型として振る舞い、プロトコル上は
  問題ないはずという判断。あわせてタイムアウトも2,000
  →50,000イテレーションへ引き上げた（NAKの再試行はハードウェアが
  黙って行うため、1回分の実トランザクションが終わるのに前の値では
  短すぎた可能性を考慮）。この変更で実機のキー入力が動作することを確認した
  （✅ Stage 3のキー入力自体は完了）
- **踏んだ罠その2（実機、修正済み）**: 上記修正後、キー入力自体は動くように
  なったが、キーボード動作中に`usbinfo`コマンドを実行すると
  `HCINT=0x00001002`（CHHLTD＋XCS_XACT_ERR）でキーボードが反応しなくなる
  症状が出た。原因は`usbinfo`が呼ぶ`probe_port`がDWC OTGコアのフルソフト
  リセット＋USBバスリセットを行うため、`UsbKeyboard`がキャッシュしている
  デバイスアドレス（1）・設定状態が無効化され（デバイスはアドレス未割当の
  デフォルト状態に戻る）、その後の`UsbKeyboard::poll`がもう存在しない
  アドレス1へ話しかけ続けて`XCS_XACT_ERR`を出し続ける、という壊れ方。

  単一チャネル・単一デバイス前提の設計上、`usbinfo`/`usbvbus`実行中に
  `UsbKeyboard`セッションが生きていることを検知して衝突を防ぐ仕組みは
  実装しておらず、そのかわり**自己回復**で対処した。`run_packet`の戻り値を
  `Option<usize>`から`PacketOutcome`（`Ok`/`Timeout`/`Error`の3値）に変更し、
  `UsbKeyboard::poll`は「まだキーが無い」を意味する`Timeout`と、実際の
  トランザクションエラーを意味する`Error`を区別してカウントするように
  した（`Timeout`はアイドル時に毎フレーム起こりうる正常系なのでカウント
  しない。`SET_IDLE(0)`によりデバイスは変化があるまでNAKし続けるため）。
  `Error`が連続`POLL_FAILURE_GIVE_UP_THRESHOLD`（10）回続いたら
  `UsbKeyboard::needs_reinit`が`true`を返し、`lcd.rs`が破棄・再列挙する。

  実機確認したところ`usbinfo`実行時にこの自己回復自体は機能したが、
  （1）`Error`のたびに`run_packet`が毎回ログを出すため10行前後スパムする、
  （2）`needs_reinit`検知後も既存の「未接続時の再接続タイマー」（約5秒
  周期）に乗せていたため復帰に数秒かかる、の2点をユーザー指摘で
  改善した。（1）は`run_packet`に`quiet_errors`引数を追加し、
  `UsbKeyboard::poll`がストリーク内の最初の1回だけログを出して以降は
  黙らせるようにした。（2）は`needs_reinit`を検知した場合だけ
  `lcd.rs`が再接続タイマーを待たずその場で`UsbKeyboard::init`を
  即座に呼び直すようにした（`is_connected`が false＝物理的に未接続の
  場合は、急ぐ理由が無いので従来どおり約5秒周期のスロットルに乗せる）。
  この2点の改善自体は実機未確認
- HID Bootレポート（8byte）は前回との差分（新規に立ったキーコード）だけを
  ASCII変換してキュー（最大6件）に積み、`poll`は1回につき1byteずつ返す
  （`CardKb::poll`と同じ「1呼び出し1byte」形に合わせるため）
- USB側の再接続チェックは`CardKb`の「約1秒（60フレーム）ごと」より大幅に
  緩め、300フレーム（約5秒）ごとにした。`probe_port`からのフル再列挙は
  VBUS安定待ち・接続待ち・デバウンス・リセットで数百ms〜のブロッキング
  処理になるため、CardKBと同じ頻度で試みるとキーボード未接続中に
  コンソール全体が定期的にカクつく
- 物理的な抜去は`UsbKeyboard::is_connected`（`HPRT.PRTCONNSTS`を読むだけ）
  で検出し、`lcd.rs`側が`poll`のタイムアウトを待たずに毎フレーム判定する

## 想定される罠（実装前メモ、実機で要検証）

`DESIGN.md`・`SD_CARD_PLAN.md`同様、以下は「シミュレーションではなく実機でしか
踏めない」可能性が高い項目として、着手前に注意しておく。

- **VBUS ON順序**: 5Vを先に入れてからポートリセットする必要があるか、
  順序を間違えるとデバイス側が誤検出しないか（SDカードのCMD0前の電源
  安定待ちと同種の問題）
- **チャネル停止**: SDMMCのDW-GDMA同様、DWC OTGのホストチャネルも
  `CHENA`クリアだけでは止まらず、Disable要求→完了待ちの手順が必要な
  可能性がある（`usb_dwc_ll.h`のチャネルdisable手順を要確認）
- **キャッシュ同期**: 転送バッファがPSRAM/内蔵SRAMいずれの場合も、
  DMAを使うなら`psram.rs`/`sdmmc.rs`と同じ`Cache_WriteBack_Invalidate_Addr`
  呼び出しが必要になる可能性がある（DWC OTGがDMAモードかFIFOのCPU
  読み書きモードかは実装時に選択、まずCPU読み書き（Slave/FIFOモード）
  から試し、必要になった場合のみDMAモードへ移行する方針とする）
- **ポート速度とチャネル速度設定の不一致**: Low-SpeedデバイスをFull/
  High-Speedポート越しに扱う場合の分周・プリアンブル設定はESP-IDFでも
  複雑な部分なので、まずはFull-Speedキーボードでの動作を優先し、
  Low-Speedキーボードは追加検証項目とする

## モジュール構成（実際）

Stage 1〜3を実装した当初はSD_CARD_PLAN.mdのブロックI/O層と同じ理由（ホスト
初期化・列挙・HID固有処理が同じチャネル0・同じレジスタ操作を共有しており、
分ける明確な境界が無かった）で、すべて`src/usb.rs`にそのまま実装していた。
その後1ファイルが肥大化した（1400行近く）ため、ユーザーの指摘で
ホストコントローラー／USBプロトコル／クラスドライバの3層に分割した。
`lcd.rs`・`lcd/st7121.rs`と同じ「親ファイルが`mod`宣言、実体はサブ
ディレクトリ」という構成に合わせている。

- `src/usb.rs`: サブモジュール宣言と再エクスポートだけの薄い親ファイル
- `src/usb/hcd.rs`: ESP32-P4 High-Speed USB-DWCホストコントローラー
  ドライバー（Stage 1）。VBUS、コア/ポート初期化、チャネル・パケット実行
  プリミティブ（`run_packet`、`PacketOutcome`）。USBデバイス・記述子の意味は
  一切知らない
- `src/usb/protocol.rs`: 汎用USBプロトコル層（Stage 2）。コントロール転送の
  組み立てと標準記述子による列挙（`enumerate_device`）。デバイスクラスに
  ついては何も知らない。`EnumeratedDevice`はHIDキーボード固有の情報を
  持たず、生のconfiguration記述子バイト列（`config_bytes()`）だけを返す
- `src/usb/hid_keyboard.rs`: HID Bootキーボードのクラスドライバー（Stage 3）。
  `find_hid_keyboard`（configuration記述子からHIDインターフェースを探す）、
  クラス固有リクエスト、キーコード→ASCII変換、`UsbKeyboard`。Interrupt IN
  ポーリング（`hcd::run_packet`を直接呼ぶ）は標準コントロール転送ではないため
  `protocol.rs`を経由しない
- `src/shell.rs`: `usbinfo`（`hcd::probe_port`→`protocol::enumerate_device`→
  `usb::find_hid_keyboard`の順で呼び、Stage 1/2の確認用）・`usbvbus`
  （VBUS制御ビットの実機発見用）
- `src/lcd.rs`: フレームループに`UsbKeyboard`のポーリング・再接続ロジックを
  `CardKb`と並列に追加。コンソールへの合流点（`Console::push`）は共通

## 将来検討（範囲外）

- HIDマウス、複合デバイス（キーボード+ホイール等）の非Bootレポート解析
- USB Mass Storage（MSC）。将来的にSDカード同様「USBメモリの生ブロック
  読み書き」まで実機確認できれば、`sdmmc.rs`のブロックI/O層と同じ抽象で
  上位（ファイルシステム）を共有できる可能性がある
- USBハブ経由での複数デバイス接続
- High-Speedデバイスのchirp/ネゴシエーション（HID Bootキーボードの
  マイルストーンでは不要）
- Full-Speed OTGコントローラ（USB-C側）を使った同時ホスト動作
  （ESP32-P4は2系統同時ホスト動作が可能とされるが、本計画のマイルストーンでは
  USB-A/High-Speedコントローラのみを対象とする）

## 各段階の完了条件（実機確認）

`SD_CARD_PLAN.md`と同じく、各StageはUARTシェル経由でコマンドを叩いて目視確認
できることをもって完了とし、次のStageへ進む前に必ず実機でログを確認する。
