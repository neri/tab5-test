# USB-Aホスト機能 実装計画

## 方針

`../DESIGN.md`の方針（ESP-IDF/RTOSをリンクせずレジスタ操作で実装、1機能=1モジュール=
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

ECO2（chip revision v1.3、`../DESIGN.md`記載の対象個体）ではHigh-Speed OTGのDPラインに
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
複数チャネル同時発行はこのStageの範囲外とした。ハブ経由のSplit Transactionも
このStageでは範囲外とし、Stage 6で実装した。

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

## Stage 4: USBハブ対応（FS強制 → Stage 6でSplit Transaction対応に置き換え）

Stage 3までの前提（`protocol::DEVICE_ADDRESS`固定値・チャネル0のみ・単一デバイス）を
崩し、複数デバイスを列挙・ポーリングできるようにする。手元の検証機材は
High-Speed対応ハブだが、HSハブをHSで動かした状態で配下のFS/LSデバイスを
扱うにはSplit Transaction（SSPLIT/CSPLIT）が必須となる。

当初これは**ハードウェア制限で対応不可能**と結論した。根拠はEspressifの
資料が一致してそう述べていたことである（`usb_dwc_cfg.h`の
`OTG20_SINGLE_POINT 1`、maintainer notesの"Split transfers not supported"、
`components/usb/hub.c`が速度不一致ポートを"transaction translator (TT) is
not supported"で切り離す実装）。

**この結論は実機測定で誤りだと判明した**（Stage 6参照）。ESP32-P4 v1.3の
シリコンは`GHWCFG2.SingPnt = 0`（multi-point）を報告し、`HCSPLT`は完全に
機能するレジスタである。Stage 4のFS強制は「ハードウェア制限」ではなく
「Split Transaction未実装の間の回避策」だった。Stage 6で実装したため、
`FORCE_FS_LS_ONLY_HOST`は既定で`false`になっている。

以下はStage 4当時の記録として残す。FS強制の仕組み自体はフォールバックとして
有効なままである。

代わりに、**ホスト自身をFS/LS専用動作に強制し、手元のHSハブをFSハブとして
振る舞わせる**方針を採る。USB2.0のHS検出はリセット中のチャープ
（ホストがK、対応デバイスがKJKJKJ...で応答）で成立するため、ホストが
チャープを送らなければHS対応デバイスはFSのまま通常のリセット/enable
シーケンスを続ける（規格上必須の後方互換動作。全てのHS認証デバイスは
これに応答できなければならない）。DWC OTGコアにはこれをそのまま実現する
`HCFG.FSLSSupp`（FS/LS-Only Support）ビットがあり、ESP-IDFの
`usb_dwc_ll.h`にも`usb_dwc_ll_hcfg_set_fsls_supp_only()`という専用の
setterが存在することを確認済み（`hw->hcfg_reg.fslssupp = 1`、
`usb_dwc_hcfg_reg_t`のbit 2）。

**注意**: ESP-IDFの実ホストドライバ（`hcd_dwc.c`）自身はこのビットを
一度も呼んでいない。ESP-IDFは常にHS対応ホストとして動作し、FS/LS
デバイスは（チャープに応答しないことで）自然にフォールバックする経路
しか使っていない。つまり「HS対応PHY・HS対応コアで、意図的にチャープ
そのものを止める」という今回の使い方はESP-IDFの実績が無い組み合わせで、
`../DESIGN.md`の言う「実機でしか踏めない罠」に該当する可能性が高い。

### FS強制を切り替え可能に保つ方針

（Stage 4当時は「両方を同時に満たすモードは存在しない」と考えていたが、
Stage 6のSplit Transaction対応でHS動作とFS/LSデバイスの併用が可能になった。
以下の切り替え可能性を保つ方針そのものは、そのままフォールバックとして
役に立っている。）

- `hcd.rs`に`FORCE_FS_LS_ONLY_HOST: bool`のような名前付き定数を1つ置き、
  `HCFG.FSLSSupp`の設定箇所はこの定数の分岐だけにする（コンパイル時に
  畳み込まれるので実行時オーバーヘッドは無い）
- 定数を`false`に戻すだけで元のHS対応動作（Stage 1〜3で実機確認済みの
  経路）にそのまま復帰できることを保証する。既存のHS関連コード
  （`GUSBCFG_FORCEHSTMODE`、UTMI PHYのHS選択、`Speed::High`列挙子、
  `HPRT_PRTSPD_MASK`の判定など）は一切削除・変更しない
- Stage 5以降で新設するハブ・複数デバイス関連のコードも、`FSLSSupp`の
  ON/OFFに依存する分岐は作らない（Stage 6でHSハブ配下のFS/LSデバイスを
  実際に扱えるようになったため、この方針は結果的に正しかった。速度差の
  扱いは`hcd::Route`に閉じている）

### Stage 4-1: FS/LS専用動作への切り替え ✅ 完了（実機確認済み、現在は既定オフ）

（`FORCE_FS_LS_ONLY_HOST`はStage 6で既定`false`になった。この節の内容は
機構の記録であり、フォールバックとして今も有効に動く。）

- `hcd.rs`の`set_core_defaults()`（またはその近辺）に`HCFG_FSLSSUPP`
  （`1 << 2`）を条件付きで設定する処理を追加
- ゴール: 手元のHS対応ハブをUSB-Aに直結し、`usbinfo`で`HPRT.PrtSpd`が
  Full-Speed（従来ならHighと判定されていたはず）と報告されることを
  実機で確認する。これが確認できて初めてStage 5以降のハブ検証が可能になる
  → **完了。実機でHSハブ（Genesys Logic 05E3:0610）を直結し、`usbinfo`が
  `speed: Full-Speed`を報告することを確認した**

**実装上の判断・実機で分かったこと**:
- 設定箇所は`set_core_defaults()`本体ではなく、force host mode待ちの後・
  ポート電源投入とリセットの前に置いた新関数`configure_host_speed_support()`
  にした。HCFGはホストモードのレジスタであり、かつチャープを出すか否かが
  決まるのはポートリセット時なので、その間でなければならない
- `HCFG.FSLSPclkSel`は既定値（30/60MHz）のまま変更しない。48MHz設定は
  FS専用PHY向けで、今回はUTMI+ HS PHYを動かしたままチャープだけ止める形の
  ため。`finish_port_enable`のHCFG書き込みはread-modify-writeなので
  `FSLSSupp`を壊さない
- ESP-IDFに前例が無いビットなので不発を警戒していたが、`HPRT.PrtSpd`だけ
  でなくハブが返すデバイス記述子も`bDeviceProtocol=0`（＝FSハブ）となり、
  HS動作時の値（01/02、Single/Multi TT）ではないことから、チャープが実際に
  行われていないことを2重に確認できた

### Stage 4-2: ハブ自身の列挙とハブクラスディスクリプタ取得 ✅ 完了（実機確認済み）

新規モジュール`src/usb/hub.rs`（`hid_keyboard.rs`と同じ「`protocol.rs`の
上に乗るクラスドライバ」という位置付け）。

- `protocol::enumerate_device`でハブを列挙し、`bDeviceClass == 9`
  （Hub）であることを確認する
- クラス固有`GET_DESCRIPTOR`（wValue = `0x29 << 8`、Hub Descriptor）で
  `bNbrPorts`・`wHubCharacteristics`・`bPwrOn2PwrGood`・
  `bHubContrCurrent`を取得する。`DeviceRemovable`/`PortPwrCtrlMask`は
  ポート数に応じた可変長ビットマップなので、まず固定長ヘッダ部分だけ
  読んで`bNbrPorts`を確定させてから、必要バイト数だけ読み直す
  （`protocol.rs`の設定記述子ヘッダ→フル読み出しの2段階パターンを踏襲）
- 標準`GET_STATUS`（ハブ自身宛て）でローカル電源・過電流状態を取得する
- `shell.rs`に`usbhub`のようなコマンドを追加し、ポート数・電源特性が
  ログ/コンソールに出ることを確認する
  → **完了。実機のHSハブで`usbhub`が「4ポート、power-good 100ms、
  hub current 100mA、per-portの電源スイッチング・過電流保護、compound、
  ポートインジケータあり、DeviceRemovable＝ポート1〜3のみ着脱可能、
  hub status＝ローカル電源good・過電流なし」を表示することを確認した**

**実装上、計画時点から具体化した判断**:
- 計画では触れていなかったが、`Hub::open`で`SET_CONFIGURATION`まで行う。
  Addressステートのデバイスが応答を保証されるのは標準リクエストだけで、
  ハブディスクリプタ取得を含むクラスリクエストはconfigured後が前提の
  ため（`UsbKeyboard::init`と同じ位置付け。Stage 4-3のポート制御でも
  どのみち必要）
- `protocol.rs`への変更は`control_transfer_in`を`pub`にしただけ。
  クラス固有のINリクエスト（ハブディスクリプタ・GET_STATUS）を
  同じSETUP/DATA/STATUSの組み立てに乗せるため、setupパケットは
  呼び出し側（`hub.rs`の`build_class_in_setup`）が作る形にした
- `DeviceRemovable`は`bNbrPorts / 8 + 1`バイト。ここを固定長と誤ると
  直後の`PortPwrCtrlMask`をビットマップに取り込んでしまう（実装中に
  一度踏んだ）
- **実機のハブが`compound = yes`かつポート4が`non-removable`だった**。
  4ポートのうち1つに何かが常時ぶら下がっている構成のため、Stage 4-3で
  「接続を検出した最初のポート」を選ぶ方針だと、ユーザーが挿した機器
  ではなくこの内蔵デバイスを掴む可能性がある。そのため着脱可能な
  ポートを優先して選ぶ（見つからない場合のみ non-removable ポートに
  フォールバックし、その旨をログに出す）

**あわせて入れたインフラ変更（Stage 4固有ではない）**:
- `console.rs`の`Console::write_output_line`が各行をUARTログにも
  ミラーするようにした。シェル出力の唯一の出口（呼び出し元は`shell.rs`
  だけ）なので1箇所の変更で全コマンドの結果がシリアルに出る。実機の
  画面から目視で書き写す必要が無くなり、以降のStageの動作確認ログは
  そのまま貼れる。`shell::execute`は実行したコマンド行を`> usbhub`の
  形で先にUARTへ出し、ログがトランスクリプトとして読めるようにしている

### Stage 4-3: 単一ポートの電源投入・接続検出・リセット・速度判定 ✅ 完了（実機確認済み）

複数ポート同時対応は範囲外とし、まず1ポートだけを対象にする。

- `SET_FEATURE(PORT_POWER)`でポート電源を投入し、`bPwrOn2PwrGood`
  （2msit単位）分待つ
- `GET_PORT_STATUS`をポーリングして接続検出（`hcd.rs`の
  `wait_for_connect`と同じ発想でポート接続ビットを見る）→デバウンス
- `SET_FEATURE(PORT_RESET)`→ポートステータス変化ビット
  （`C_PORT_RESET`）をポーリングして完了検出→
  `CLEAR_FEATURE(C_PORT_RESET)`
- `GET_PORT_STATUS`の`PORT_LOW_SPEED`/`PORT_HIGH_SPEED`ビットで速度
  判定する。Stage 4-1でHSを抑止済みのため`PORT_HIGH_SPEED`は立たない
  想定だが、ビット自体は読んでおき、想定外に立った場合はログを出して
  そのポートは扱わない（安全側に倒す）
- ゴール: ハブの1ポートに何らかのUSB機器を挿した状態で、電源投入から
  リセット完了・速度判定までのログが実機で出ることを確認する
  → **完了。VIA Labs 2109:2813のハブ（4ポート、per-port電源スイッチング、
  全ポートremovable）で、全ポートへの`PORT_POWER`投入 → ポート1の接続検出
  （`st=0x0301`）→ リセット → enable＋速度判定（`st=0x0303`）まで
  `usbhub`が完走することを確認した**

**踏んだ罠（実機、修正済み）**:
- **最初に使ったハブ（Genesys Logic 05E3:0610、compound・ポート4が
  non-removable）では、ポート1への`SET_FEATURE(PORT_POWER)`は通るが、
  以降ハブが一切応答しなくなる**という壊れ方をした。別のハブ
  （VIA Labs 2109:2813）に替えたところ問題なく動いたため、このハブ固有の
  問題として深追いはしていない。Stage 4のスコープは「1ポート・1デバイス」
  なので、対応ハブの一般性はいったん問わない
- 上記の症状を報告する際、**ログが延々と出続ける**という二次的な問題が
  あった。原因は`find_connected_port`が`port_status`の失敗を「まだ接続
  されていない」として扱って再スキャンしていたこと。1回のコントロール
  転送タイムアウトが約0.2秒かかる一方、待ち時間カウンタはスキャン1周
  あたり5msしか進まないため、4ポート×100周＝約400回のタイムアウトログを
  出す計算になっていた。ハブ自身への転送が失敗した時点で即座に打ち切る
  ように修正（`power_on_all_ports`・シェルのポート一覧も同様）
- 切り分けのため、`hcd::run_packet`のタイムアウト時・トランザクション
  エラー時に`HPRT`の生値をログするようにした（接続・enableが維持されて
  いるか、過電流ビット（bit 4）が立っていないかで、電気的な問題と
  プロトコルの問題を区別できる）。`quiet_timeout`/`quiet_errors`が
  立っている経路（キーボードの毎フレームポーリング）では出ない

**実装上、計画時点から具体化した判断**:
- ポートは全ポートに電源を入れてから選ぶ。per-port電源スイッチングの
  ハブでは電源を入れるまで接続状態が読めないため、「どのポートを使うか」
  を決めるより前に電源投入が必要になる
- ポート電源投入の間に20msの間隔を空けている（バスパワーのハブに
  全ポート分の突入電流を同時に要求しないため）
- 接続検出後は250msのデバウンス＋`C_PORT_CONNECTION`クリアを行い、
  デバウンス中に接続が消えた場合は中止する（`hcd::probe_port`の
  ルートポート側と同じ扱い）
- リセット完了は`C_PORT_RESET`のセットまたは`PORT_RESET`の落ちで検出し、
  `CLEAR_FEATURE(C_PORT_RESET)`＋回復30msの後に**ポートステータスを
  読み直して**enableと速度を確定する（リセット直後の値は確定前）

### Stage 4-4: マルチデバイスアドレス管理の一般化とハブ経由デバイスの列挙 ✅ 完了（実機確認済み）

- `protocol::DEVICE_ADDRESS`の固定値運用をやめる。動的なアドレス
  プールまでは作らず、まず「ハブ＝アドレス1、ポート1のデバイス＝
  アドレス2」の2枠固定で十分とする（複数ポート・複数デバイスへの
  一般化は将来検討）
- `protocol::enumerate_device`をデバイスアドレス引数化する
  （`hcd::run_packet`・`control_transfer_*`は既にアドレスを引数で
  受け取る設計になっているため、`protocol.rs`内の`DEVICE_ADDRESS`
  直接参照を外すだけで済む見込み）
- **想定される罠**: ハブ経由デバイスがLow-Speedの場合、
  `HCCHAR.LSpeed`（Low-Speed Device、PREトークンの要否に関わる
  ビット）の扱いが新たに問題になる可能性がある。現状の`hcd.rs`は
  このビットを一度も設定していない。これまで直結LSデバイスで問題が
  出なかったのは「ホスト自身の動作速度とデバイスの速度が一致していた
  （またはコアが自動処理していた）」だけの可能性があり、FS動作の
  ホスト経由でLSデバイスに話しかける今回のハブ構成で初めて表面化する
  かもしれない。実機で原因不明のSTALL/XACTERRが出た場合はまずここを
  疑うこと
- ゴール: ハブの1ポートに挿したデバイスが`SET_ADDRESS`（アドレス2）
  〜configuration記述子取得まで実機で完走する
  → **完了（実機確認済み）。ハブのポート1に挿したUSBメモリ
  （Sony 054C:0243、Full-Speed）が`usbhub`でアドレス2の割り当て・
  デバイス記述子・configuration記述子（32バイト）まで完走した。
  Low-Speedキーボードも、下記の罠を解決した後に同様に完走している**

**踏んだ罠: Low-Speedデバイス（PREトークン）— PHY側のビットが必要だった**

計画書が「想定される罠」に挙げていた`HCCHAR.LSpeed`（PREトークン）が
実際に問題になった。`HCCHAR.LSpdDev`を立てるだけでは足りず、**UTMI PHY
側のプリアンブル制御ビットを別途有効にする必要がある**というのが結論。

- 症状: ハブのポートに挿したLow-Speedキーボードに対し、**最初の
  コントロール転送のSETUPステージ**が`HCINT=0x00001002`
  （CHHLTD＋XCS_XACT_ERR）で失敗する。`HPRT=0x0002140F`で
  ルートポートは接続・enable・FS・過電流なしと健全、ハブから見た
  ポートも`conn pwr ena Low-Speed`のまま
- 切り分け: 動く経路との差分はPREトークンだけに絞り込めた

  | 経路 | バス速度 | デバイス速度 | PRE | 結果 |
  |---|---|---|---|---|
  | ハブ自身へのコントロール転送 | FS | FS | 不要 | 動く |
  | キーボード直結 | LS | LS | 不要 | 動く |
  | ハブ配下のUSBメモリ | FS | FS | 不要 | 動く |
  | ハブ配下のキーボード | FS | LS | **必要** | SETUPで失敗 |

  Scatter/Gather DMA・`HCTSIZ`・QTD・アドレス割り当て・MPS=8は
  動く経路と全て共通。`HCCHAR`のビット配置（`lspddev`=bit17、
  `ec`=bits21:20、`devaddr`=bits28:22）も`usb_dwc_struct.h`で
  確認済みで、ESP-IDFの`usb_dwc_ll_hcchar_init`が書く内容とも一致する
- ESP-IDFの状況（この結論の根拠）:
  - `ls_via_fs_hub`（＝FSポート配下のLSデバイス）というフラグは存在し、
    `HCCHAR.LSpdDev`を立てる唯一の条件になっている。ただしこれが
    実際に走るのはFS専用コアのESP32-S2/S3であって、**P4では一度も
    通らない**
  - `hcd_dwc.c`の`_buffer_check_done`は`ls_via_fs_hub`のとき
    コントロール転送のステージ間に`esp_rom_delay_us(1000)`を挟む
    （"The HW can't handle two transactions with preamble in one
    frame"、IDF-12986）。本実装でも同じ間隔を入れたが症状は変わらず、
    SETPUステージ自体が通らないため間隔の問題ではない
  - **ESP-IDFにはSplit Transactionのコードが存在しない**
    （`usb/`・`hal/`に`HCSPLT`/splitの記述が皆無）。`usb/hub.c:348`は
    HSハブ配下に速度の異なるデバイスが繋がった場合、
    "transaction translator (TT) is not supported"として明示的に
    拒否する
  - （当時の記述「P4のチャネルレジスタには`HCSPLT`自体が無い」は誤り。
    `reserved_0x04`になっているのはESP32-S2/S3のヘッダとP4の
    デバイスモード用構造体で、P4の`usb_dwc_host_chan_regs_t`は
    `hcsplt_reg`を持っている。実機にも実在する → Stage 6）
- **原因と解決**: UTMI PHYの`fc_06.pre_hphy_lsie`（bit 2、ESP-IDFの
  `usb_utmi_struct.h`に "**Dis_preamble enable**" と記載、リセット値0）
  を1にすると動作した。実機で `usbexp 1` → `usbhub` の一発で、SETUPの
  失敗が消えてLSキーボードのキー入力まで通ることを確認している。
  ESP-IDFはこのビットを触らない（`usb_utmi_ll_configure_ls`が設定するのは
  `ls_par_en`と`ls_kpalv_en`だけ）が、それはP4でPREを一度も送らないため。
  現在は`configure_utmi_phy`で他のLS用ビットと並べて常時設定している
- レジスタ側（`HCCHAR.LSpdDev`、`lspddev`=bit17）の設定も必要で、これは
  「LSデバイス」ではなく「**FSバス上のLSデバイス**」でのみ立てる。
  直結LSデバイスはバス全体がLSになりPRE自体が存在しないため、立てては
  いけない（ESP-IDFのフラグ名も`ls_via_fs_hub`、条件は
  `port_speed == FULL && dev_speed == LOW`）。実装では
  `hcd::Endpoint::low_speed_via_hub`という名前でこの区別を型に持たせた
- **「FS-onlyハブを入手する」は解決にならない**（Stage 4着手時点の
  想定の誤り）。LSデバイスがハブ配下にある限り、ハブがFS専用かHSかに
  関係なくホストはPREを送る必要がある。PREを回避できるのはHSハブの
  TTを使うSplit Transactionだけで、それがこのコアに無い。つまり
  PREを動かすこと自体が唯一の道だった
- 切り分けの過程で、`usbexp <mask>`という実験用コマンドを一時的に追加した
  （PHYのプリアンブルビット／`HCCHAR.EC`／フレーム境界揃えの3つを実行時に
  組み合わせられるようにし、再フラッシュせずに試せるようにしたもの）。
  1つ目で解決したため、残り2つは不要と確認できた時点でコマンドごと削除した

### Stage 4-5: 既存HID Bootキーボードクラスドライバをハブ経由へ適用 ✅ 完了（実機確認済み）

- `hid_keyboard.rs`の`DEVICE_ADDRESS`直接参照を、ハブ経由で確定した
  アドレスを受け取る引数に差し替える
- `lcd.rs`のフレームループに、ハブ経由`UsbKeyboard`のポーリングを
  追加する。直結USBキーボードとハブ経由デバイスの同時使用は範囲外とし、
  まず「ハブ経由のみ」での動作を確認する
- ゴール: 実機でハブの1ポートに挿したHID Bootキーボードのキー入力が
  `console.rs`へエコーされることを確認する（Stage 3のマイルストーンの
  ハブ経由版）
  → **完了。Low-Speedキーボードをハブのポートに挿した状態でキー入力が
  コンソールへエコーされることを実機で確認した**

**実装上、計画時点から具体化した判断**:
- `lcd.rs`にハブ経由専用のポーリングを足すのではなく、`usb.rs`に
  `connect_keyboard`を新設して直結／ハブ経由の判断をそこに閉じ込めた。
  `UsbKeyboard`が持つのは`hcd::Endpoint`（アドレス・EP番号・速度を含む）
  なので、どちらの経路で列挙されたかは`poll`以降のコードに影響しない。
  結果として`lcd.rs`の変更は`UsbKeyboard::init()`→`usb::connect_keyboard()`
  の差し替えだけで済み、フレームループのポーリング・再接続ロジックは
  Stage 3のまま
- `UsbKeyboard::init`（自分で`probe_port`する）を`attach(&device)`
  （列挙済みデバイスに取り付く）に分解した。ポート立ち上げと列挙は
  クラスドライバの仕事ではないという整理
- ハブ経由の場合、`is_connected`（`HPRT`を読むだけ）が見ているのは
  「ハブがUSB-Aに挿さっているか」であって、キーボードがハブのポートに
  挿さっているかではない。ハブのポートから抜いた場合はポーリングが
  エラーを返し始め、`needs_reinit`（連続10回）経由で再列挙される。
  毎フレーム`GET_PORT_STATUS`を撃つのはコストが見合わないための割り切り
  （ハブのホットプラグ検出は計画どおり範囲外）

## 想定される罠（実装前メモ、実機で要検証）

`../DESIGN.md`・`SD_CARD_PLAN.md`同様、以下は「シミュレーションではなく実機でしか
踏めない」可能性が高い項目として、着手前に注意しておく。

- **VBUS ON順序**: 5Vを先に入れてからポートリセットする必要があるか、
  順序を間違えるとデバイス側が誤検出しないか（SDカードのCMD0前の電源
  安定待ちと同種の問題）
- **チャネル停止**: SDMMCのDW-GDMA同様、DWC OTGのホストチャネルも
  `CHENA`クリアだけでは止まらず、Disable要求→完了待ちの手順が必要な
  可能性がある（`usb_dwc_ll.h`のチャネルdisable手順を要確認）
  → **当たっていたが、実際に刺さったのは逆方向だった。** Disable要求→完了待ち
  （`force_halt_channel`）は必要である一方、**既に停止しているチャネルに
  それをやってはいけない**（停止済みチャネルはhalt割り込みを出さないため
  待ちが空振りし、コアの状態が壊れる）。Stage 6の罠4を参照
- **キャッシュ同期**: 転送バッファがPSRAM/内蔵SRAMいずれの場合も、
  DMAを使うなら`psram.rs`/`sdmmc.rs`と同じ`Cache_WriteBack_Invalidate_Addr`
  呼び出しが必要になる可能性がある（DWC OTGがDMAモードかFIFOのCPU
  読み書きモードかは実装時に選択、まずCPU読み書き（Slave/FIFOモード）
  から試し、必要になった場合のみDMAモードへ移行する方針とする）
- **ポート速度とチャネル速度設定の不一致**: Low-SpeedデバイスをFull/
  High-Speedポート越しに扱う場合の分周・プリアンブル設定はESP-IDFでも
  複雑な部分なので、まずはFull-Speedキーボードでの動作を優先し、
  Low-Speedキーボードは追加検証項目とする
  → **これが実際に一番手強い罠だった。`HCCHAR.LSpdDev`に加えてUTMI PHYの
  `pre_hphy_lsie`が必要で、後者はESP-IDFに前例が無い。Stage 4-4の記録を
  参照**

## モジュール構成（現在）

Stage 1〜3を実装した当初はSD_CARD_PLAN.mdのブロックI/O層と同じ理由（ホスト
初期化・列挙・HID固有処理が同じチャネル0・同じレジスタ操作を共有しており、
分ける明確な境界が無かった）で、すべて`src/usb.rs`にそのまま実装していた。
その後1ファイルが肥大化した（1400行近く）ため、ユーザーの指摘で
ホストコントローラー／USBプロトコル／クラスドライバに分割した。その後、
[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)でMSCクラスドライバ、
[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md)でバスの単一所有者とデバイスレジストリを追加した。
`lcd.rs`・`lcd/st7121.rs`と同じ「親ファイルが`mod`宣言、実体はサブディレクトリ」という
構成に合わせている。

- `src/usb.rs`: サブモジュール宣言と、`lcd.rs`/`shell.rs`が使う型・関数の
  最小限の再エクスポート。以前の`connect_keyboard`/
  `connect_keyboard_through_hub`は削除済みで、`registry::UsbHost::rescan`に統合した
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
- `src/usb/hub.rs`: USBハブのクラスドライバー（Stage 4）。ハブディスクリプタ取得、
  ポート電源投入・デバウンス・リセット・速度判定を担当
- `src/usb/msc.rs`: USB Mass StorageのBulk-Only Transport、トグル管理、STALL回復、
  SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10)/READ(10)を担当
- `src/usb/registry.rs`: `UsbHost`がバスを単一所有し、直結デバイスまたは
  1段のハブの全ポートをスロット管理する。デバイスの列挙、クラスドライバへの振り分け、
  複数キーボードのラウンドロビンポーリング、MSCハンドルの共有を担当
- `src/lcd.rs`: フレームループが`UsbHost`を所有し、CardKBとUSBキーボードの入力を
  `Console::push`で合流させる。切断、トランザクションエラー、空いているハブポートの再スキャンも担当
- `src/shell.rs`: `usbinfo`/`usbhub`/`usbmsc`/`usbread`/`usbmbr`は共有レジストリを
  参照し、`usbrescan`だけが明示的にバスを再列挙する。`usbvbus`はI/O expanderの
  VBUSビットを直接操作する診断用コマンド

## Stage 5（将来）: Interrupt転送の割り込み駆動化

Stage 1〜4は一貫してポーリング方式で通してきたが（`../DESIGN.md`の「まず
ポーリングで動作確認し、必要になった時点でのみ割り込み化を検討する」に
従ったもの）、Interrupt転送についてはコアが本来持っている自動スケジューリング
機能を使っていない。着手するとしたらStage 4のコミット後、独立したStageとして
扱う。

### 動機（実害が2つある）

- **フレームループがブロックされる**: `UsbKeyboard::poll`は`HCINT`を最大
  `INTERRUPT_POLL_TIMEOUT_ITERATIONS`（50,000）回スピンする。キーが無い
  アイドル時にも毎フレーム発生する
- **キー入力を取りこぼしうる**: `SET_IDLE(0)`によりデバイスは状態変化時
  にしかレポートしないため、約17.5ms（57Hz）のポーリング間隔の間に
  「押して離す」が完了すると中間状態が失われる。LSキーボードの
  `bInterval`は通常10ms前後で、こちらの方が遅い

### コア側の機能（ESP-IDFで確認済み）

Scatter/Gather DMAモードにはperiodic frame listがあり、xHCIと同じく
「事前に登録しておけば、コアが自動でスケジューリングし、実際にデータが
来たときだけ割り込む」動作ができる。

- `HFLBAddr`にフレームリストの先頭アドレス、`HCFG.FrListEn`でエントリ数
  （8/16/32/64）、`HCFG.PerSchedEna`で巡回を有効化
- 各エントリは「そのUSBフレームでサービスするチャネルのビットマップ」。
  ESP-IDFは`bInterval`を2の冪に丸めて
  `frame_list[offset + i*interval] |= 1 << chan_idx`と埋めている
  （`usb_dwc_hal.c`の`usb_dwc_hal_chan_set_ep_char`）
- 割り込みマスクは3段（`GINTMSK`→`HAINTMSK`→`HCINTMSK`）。ESP-IDFが
  有効にする`CHAN_INTRS_EN_MSK`は`XFERCOMPL | CHHLTD | BNAINTR`で、
  **NAKを含まない**＝実際に転送が完了したときだけCPUが起きる
- Stage 3で「`eptype`を`INTR`にすると列挙は成功するのにキー入力に一切
  反応しない」という壊れ方をしたのは、まさにこのフレームリストに登録して
  いなかったため。`BULK`扱いにした回避策はここで解消できる

### 必要な作業（現ドライバの不変条件を崩す）

`hcd.rs`は「チャネル0だけを使い、1パケットごとに全レジスタを書き切り、
呼び出し間に状態を持たない」という前提で書かれている。ここを崩す必要がある。

- DMA可視・キャッシュ整合の取れたフレームリスト領域（QTDと同様の
  アライメント要件を確認すること）
- チャネルアロケータ。フレームリストはチャネル番号のビットマップなので、
  割り込みエンドポイントに専用チャネルを固定する必要があり、コントロール
  転送とチャネル0を共有できなくなる
- ISRを`interrupts.rs`（既にLCD/DSIで使用中）経由で配線し、ISRと
  `lcd::run_console`のループの間にHIDレポートのキューを置く

### 着手前に確認すべき点

- ESP-IDFのコードに "LS endpoints do not support periodic transfers"
  というコメントがある（`usb_dwc_hal.c`の`sched_info`設定箇所）。現在の
  検証機材のキーボードはLow-Speedなので、periodicスケジューリングの
  対象にできるかを最初に確かめる必要がある
- LS＋PRE＋periodicという組み合わせはESP-IDFに前例が無い（Stage 4-4で
  踏んだPHYビットと同種の未知が残っている可能性がある）

## Stage 6: Split Transaction対応（HSハブ配下のFS/LSデバイス）✅ 実機確認済み

Stage 4で「ハードウェア制限により対応不可能」と結論した項目である。この結論は
誤りだった。

### 調査: Espressifの資料は全て誤り、シリコンが正しい

`usbhw`シェルコマンド（`hcd::probe_split_support`）でESP32-P4 v1.3の実機を
測定した結果:

```text
GHWCFG2=0x215FFFD0   SingPnt(bit5)=0
HCSPLT ch0: wrote 0xFFFFFFFF -> 0x8001FFFF; wrote 0x12345678 -> 0x00005678
```

- `GHWCFG2.SingPnt`はコア自身が`OTG_SINGLE_POINT`合成パラメータを報告する
  読み出し専用ビットで、**0（multi-point = hubとsplit対応）**を返す。同じ
  レジスタの他10フィールド（architecture 2、host channel 16、dynamic FIFO、
  multi-processor interruptなど）は全て資料通りにデコードできるので、ビット
  位置のずれではない。食い違うのは`SingPnt`と`FSPhyType`の2つだけである。
- `HCSPLT`は実在する読み書き可能なレジスタである。全1書き込みで
  データブックの実装フィールドマスク（`SpltEna`|`CompSplt`|`XactPos`|
  `HubAddr`|`PrtAddr` = `0x8001FFFF`）がそのまま返り、予約ビット[30:17]は
  0に落ちる。任意パターンも同じマスクを通して保持される
  （`0x12345678 & 0x8001FFFF = 0x00005678`）。

つまり`soc/esp32p4/register/hw_ver{1,3}/soc/usb_dwc_cfg.h`の
`OTG20_SINGLE_POINT 1`は、実際に出荷されているシリコンを正しく記述していない。
`usb_dwc_cfg.h`をこのチップの仕様書として信用してはならない。

### 実機で踏んだ罠 1・2（split実装時）

罠は全部で4つあった。どれも実機でしか分からず、Espressifの資料にもESP-IDFの
コードにも手がかりが無かった（ESP-IDFは`hcsplt_reg`に一度も書き込まないため）。
3と4は後述する（Stage 7で定期rescanを廃止して初めて露出した）。

1. **Scatter/Gather DMAではSplit Transactionが動作しない。**
   `HCFG.DescDMA=1`のまま`HCSPLT.SpltEna`を立ててチャネルを有効化すると、
   コアはトランザクションを一切試行しない（`ChEna=1`のまま`HCINT=0`で
   タイムアウト）。DWC_OTGの既知の制約で、Linuxのdwc2もsplitが必要な場面では
   descriptor DMAを無効化する。
   → splitパケットだけbuffer DMAで走らせる`hcd::run_split_packet`を追加。
   `HCFG.DescDMA`はコア全体の設定なので、パケット単位でクリアして復帰させる
   （`run_packet`は同期実行でチャネル0しか使わないため安全）。buffer DMAでは
   QTDではなく`HCTSIZ`にXferSize/PktCnt/PIDを直接書き、SETUPはQTDのビット
   ではなくPID=2'b11で示す。転送量も`HCTSIZ.XferSize`の減算で読む。
2. **`HCCHAR.MC/EC`が0だとTTが永久に完了しない。**
   SSPLITはACKされるのに、CSPLITが延々`NYET`を返し続ける。データブックは
   `SpltEna=1`のときこのフィールドを1以上にすることを要求しており、dwc2も
   全チャネルで1に初期化している。1にした瞬間に完走した。

### 実装

- `hcd::Route`: デバイスへの到達方法（PRE要否 + split対象ハブ/ポート）を
  デバイスと一緒に持ち回る型。従来の`low_speed_via_hub: bool`を置き換える。
  LSかつsplitは同時に成立するため独立した2フィールドにしてある
- `hcd::await_packet`: SSPLIT → ACK/NYETならCSPLIT → NAKならSSPLITから
  やり直し、という状態機械。ラウンド数の上限は用途別に呼び出し側が渡す
  （`CONTROL_SPLIT_ROUNDS`=512 / `INTERRUPT_POLL_SPLIT_ROUNDS`=1 /
  `BULK_SPLIT_ROUNDS`=20000）。splitではNAKリトライがハードウェアから
  ソフトウェア側に移るため、フレーム予算との兼ね合いを呼び出し側が決める。
  この上限は**ソフト予算**であり、到達した瞬間ではなく「到達以降で最初に
  訪れた安全な境界」で離脱する（罠3を参照）。したがって
  `INTERRUPT_POLL_SPLIT_ROUNDS = 1`は「1回だけ聞いてNAKなら諦める」を意味する
- `registry::route_behind_hub`: ハブの動作速度とポートが報告したデバイス速度を
  比較して`Route`を決める
- `hub::Hub::reset_port`: HSポートの拒否を撤去（速度差はsplitで扱えるため）
- `FORCE_FS_LS_ONLY_HOST`は`false`が既定。定数自体は特定のハブのTTが
  怪しいときのフォールバックとして残す

### 実測した1回のsplitの流れ

```text
SPLIT: round hcint=0x00000022   ← SSPLIT: ACK
SPLIT: round hcint=0x00000042   ← CSPLIT: NYET（TTがLSデバイスと通信中）
SPLIT: done  hcint=0x00000023   ← CSPLIT: XferCompl
```

最低3ラウンドかかる。キーボードのポーリング予算を4ラウンドにしていた当初は
TTが終わる前に諦めており、キーが1つも通らなかった。最終的にはこの予算を
ソフト予算（安全境界まで離脱しない）に変えたため、`1`＝1回分の問い合わせで
足りるようになっている。

### 実機で踏んだ罠 3・4（Stage 7で定期rescanを廃止して露出）

Stage 7で定期`rescan()`をやめるまで、**この2つはどちらも隠れていた**。
5秒ごとのバスリセットが副作用で壊れた状態を復旧させていたためである。

3. **Splitを中途で放棄してはいけない。** アイドルのキーボードのポーリングが
   ラウンド予算切れで即座に離脱すると、ハブのTTが誰も回収しないトランザクションを
   抱えたままになる（USB2.0 11.17はNYET以外の応答が返るまでcomplete splitを
   続けることを要求している）。`max_split_rounds`は**ソフト予算**とし、安全な
   境界（NAK = TTがバッファを解放した時点）以降でしか離脱しないようにした。
4. **停止済みチャネルに`HCCHAR.ChDis`を書いてはいけない。** これが実際の
   原因だった。安全境界で離脱した時点でチャネルは既にhalt済みなので
   `force_halt_channel()`は無害なno-opのつもりだったが、**停止済みチャネルは
   halt割り込みを出さない**ため待ちが空振りし、コアの状態が壊れる。しかも
   毎フレーム（約57回/秒）走っていた。症状は「次に行うハブへの無関係な
   コントロール転送が`XCS_XACT_ERR`でIN dataステージ失敗し、ハブが死んだように
   見える」。現在は`HCCHAR.CHENA`を確認し、本当に転送中のときだけhalt +
   FIFOフラッシュする。

### 試したが誤りだった仮説

- **`HCCHAR.LSpdDev`はHSバス上では立てるべきでない。** `low_speed_via_hub`は
  この実装では「FSバス上のLSデバイス」＝PREトークンを意味しており、PREはFSバスに
  しか存在しないので、split時は落とすのが筋だと考えた。実機では**最初のSETUPが
  即STALL**した。splitでも`LSpdDev`は必要である。
- **`HCFG.DescDMA`の復帰順序**。転送中にDMAモードを戻しているのが原因かと考えて
  halt後に移したが、症状は変わらなかった（安全境界で離脱した時点でチャネルは
  既にhalt済みなので、そもそも転送中ではなかった）。順序自体は現在も正しい方に
  してある。

### 確認済みの範囲

- HS動作のまま、HSハブ配下のLSキーボードの列挙一式（`GET_DESCRIPTOR`、
  `SET_ADDRESS`、コンフィグ記述子、`SET_CONFIGURATION`、HIDの
  `SET_PROTOCOL`/`SET_IDLE`）がsplit経由で成功する。記述子の内容が正しく
  読めているため、split INが実データを運んでいることの証明になっている
- 同デバイスのInterrupt INも通り、8バイトのHIDレポートを受信できる
- **実際のキー入力がコンソールに届く。** `SET_IDLE(0)`のため変化時のみ
  レポートが来る経路（=マイルストーン本来の動作）まで通っている
- 同じハブの別ポートのHS Mass Storageは、splitを使わず直結のまま
  High-Speedで動作する（両者の併用ができている）
- 90秒の連続動作でエラー・再列挙が一度も発生しない

つまりStage 3のHIDキーボードのマイルストーンが、**HSハブ配下のLSデバイスでも**
成立している。

### 未確認

- **split経由のBulk転送（許容済みの未検証項目）。** HSハブ配下にFS/LSの
  Mass Storageを繋いだ場合の経路。手元のUSBメモリはHSなので直結で扱われ、
  この経路を通らない。FSのMSCは入手困難なため未検証のままとする。
  他のsplit経路と異なる点があるのでリスクとして記録しておく:
  - splitでペイロードを**OUT方向**に運ぶ唯一の経路である
    （`msc::bulk_transfer_out`）
  - `hcd::await_packet`は素の`ACK`を「complete splitを実行せよ」と解釈する。
    split OUTでは`ACK`が完了を意味するため、コアが`XferCompl`を上げることに
    依存している。上げない場合はcomplete splitを繰り返して予算切れになる
  - ここが動かなくてもリグレッションではなく「未実装が露出した」と扱う
- ハブ配下のハブ（多段）は従来どおり範囲外

## Stage 7: 定期rescanの廃止（増分ハブポートスキャン）✅ 実機確認済み

フレームループはハブに空きポートがあるかぎり5秒ごとに`UsbHost::rescan()`を
呼んでいた。`rescan()`はバスをリセットして全デバイスのアドレスを無効化するため、
**動作中のデバイスを数秒ごとに破棄して再列挙していた**。表示のカクつきと、
キー入力を落とすのに十分な長さの空白が発生する。

ハブは自分のポート状態を答えられるので、バスに触らずに新規デバイスを検出できる。
`UsbHost::scan_empty_hub_ports()`を追加し、空きスロットのポートについてのみ
`GET_STATUS`を1回投げる方式にした（空きポートなら
`Hub::debounce_connected_port`は遅延ゼロで即`false`を返す）。実際にデバイスが
増えたポートだけリセット・列挙し、既存デバイスには一切触らない。

| 状態 | 変更前 | 変更後 |
|---|---|---|
| ハブあり・空きポートあり | 5秒ごとに全バスリセット | 1秒ごとに空きポート数回の`GET_STATUS` |
| ハブあり・全ポート使用中 | 5秒ごとに全バスリセット | 何もしない |
| ハブなし・未列挙 | 5秒ごとに全リセット | 同じ（ルートポートしか聞ける相手がいない） |

`rescan()`自体は残す。ルートポートの再接続と`needs_reinit`（セッションが
実際に壊れた場合）では依然として必要である。

さらに減らす場合はハブのStatus Change Interruptエンドポイントを使う手があるが、
全ポート使用中は完全に無音になったため優先度は低い。

## 将来検討（範囲外）

かつてここに挙げていた「USB Mass Storage」「ハブの複数ポート同時使用」
「HSハブ配下のFS/LSデバイス」は、それぞれ`USB_MSC_PLAN.md`、
`USB_REFACTOR_PLAN.md` Stage C、上記Stage 6で実装済みのため外した。

- HIDマウス、複合デバイス（キーボード+ホイール等）の非Bootレポート解析
- ハブのカスケード接続（ハブの下にハブ）
- ハブ自身のInterrupt INステータス変更エンドポイントを使った割り込み駆動の
  ポート変化検出。Stage 7でポーリングは空きポートへの`GET_STATUS`だけになり、
  全ポート使用中は完全に無音になったため、優先度は下がった
- 真の並列転送（複数チャネル・frame list・割り込み駆動）はStage 5のまま未着手。
  Stage 6/7がStage 5より先に完了しているのは、そちらが実害のある不具合を
  抱えていたためで、Stage 5が不要になったわけではない
- Full-Speed OTGコントローラ（USB-C側）を使った同時ホスト動作
  （ESP32-P4は2系統同時ホスト動作が可能とされるが、本計画のマイルストーンでは
  USB-A/High-Speedコントローラのみを対象とする）

## 各段階の完了条件（実機確認）

`SD_CARD_PLAN.md`と同じく、各StageはUARTシェル経由でコマンドを叩いて目視確認
できることをもって完了とし、次のStageへ進む前に必ず実機でログを確認する。
