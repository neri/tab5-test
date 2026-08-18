# USB Mass Storage対応 実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: Stage 1〜6 ✅ 完了（実機確認済み、本計画のゴール達成）

USB-A**直結**のUSBメモリで、列挙・Bulk-Only Transport・SCSI
（INQUIRY/TEST UNIT READY/READ CAPACITY(10)/READ(10)）・SDカードと共通の
MBRパース（`mbr::show`、`usbmbr`/`sdmbr`）まで実機確認済み。

**USBハブ経由の接続は本計画では意図的に後回しにしたが、その後
[`USB_REFACTOR_PLAN.md`](USB_REFACTOR_PLAN.md) Stage Fで対応済みである。**
以下の「未実装」の記述は本計画完了時点のもので、現在は当てはまらない。
`connect_mass_storage`のような関数を書く代わりに、`usb::registry::UsbHost`が
ハブの全ポートを走査してMSCもレジストリに載せ、`usbmsc`/`usbread`/`usbmbr`は
`mass_storage_mut()`でレジストリを引く（直結・ハブ経由を区別しない）形に
なった。実機ではハブのポートに挿したUSBメモリが認識されるところまで
確認している。

以下、本計画完了時点の記録:
`usb::connect_keyboard`が持つ直結/ハブ経由の自動判定に相当する
`connect_mass_storage`は書いておらず、`usbmsc`/`usbread`/`usbmbr`は
すべてUSB-A直結デバイスのみを対象にする。技術的な障害があるわけではなく
（`USB_HOST_PLAN.md` Stage 4-5でHID Bootキーボードに対して同じパターンを
実装済み）、単に本計画のスコープに含めなかっただけ。詳細は「範囲外
（将来検討）」を参照。

## 方針

本計画ではESP-IDF/RTOSをリンクせずレジスタ操作で実装し、1機能を1モジュール・
実機確認可能な単位でコミットする。[`USB_HOST_PLAN.md`](USB_HOST_PLAN.md)の
「将来検討」に挙げていた項目の着手であり、`hcd.rs`（チャネル/パケットプリミティブ）・
`protocol.rs`（コントロール転送・標準列挙）は変更せず、その上にクラスドライバ
`src/usb/msc.rs`を追加する形で進める（`hid_keyboard.rs`・`hub.rs`と同じ位置付け）。

**ゴールは`SD_CARD_PLAN.md`のブロックI/O層（`sdmmc::read_block`/`read_blocks`）と
同じ抽象でUSBメモリの生ブロックが読め、`sdmbr`と同じMBRパース処理を両方の
デバイスで共有できることまで**とする。ファイルシステム（FAT/exFAT）の解釈は
`SD_CARD_PLAN.md`のStage 4bと同様に完全に範囲外とし、別タスクとして後回しにする。
書き込み（WRITE(10)）も範囲外とし、まず読み込みだけを対象にする。

参考仕様はUSB Mass Storage Class仕様（Bulk-Only Transport、通称BOT）と、対象を
USBメモリに絞ることで実質必要になるSCSIコマンドサブセット（INQUIRY、
TEST UNIT READY、REQUEST SENSE、READ CAPACITY(10)、READ(10)）。今回はレジスタ
レベルの新規実装が無く（`hcd.rs`のBulk転送経路は既にHIDキーボードの箇所で実機
経由済み）、クラスプロトコルの組み立てが主眼のため、ESP-IDFのコンポーネントを
照合先にする必要は無い。

## Stage 0: 前提の棚卸し（実装前メモ）

`USB_HOST_PLAN.md`のStage 1〜4で以下は既に実機確認済みであり、MSC対応が
新規に踏み込む部分ではない。

- VBUS ON、コア/ポート初期化、接続検出・リセット・速度判定（`hcd::probe_port`）
- コントロール転送によるデバイス列挙（`protocol::enumerate_device`）。MSCデバイスも
  `EnumeratedDevice`として同じ経路で列挙できる（デバイスクラスがHIDでなくMass
  Storageなだけ）
- ハブ経由での列挙・アドレス管理（`usb::connect_keyboard`の直結/ハブ分岐と
  同じ構造をMSC版として複製できる想定）

一方、以下はMSC対応で初めて踏み込む領域であり、`USB_HOST_PLAN.md`の想定通り
「実機でしか踏めない罠」が残っている可能性が高い。

- **Bulk OUT方向のデータフェーズ**: これまでBulk分類（`HCCHAR_EPTYPE_BULK`）で
  実際に使われてきたのはHIDキーボードのInterrupt IN代替（`hcd.rs`の
  `HCCHAR_EPTYPE_BULK`のコメント参照）だけで、方向は常にIN。コントロール転送の
  OUT ステータスステージ（0byte）も実行しているが、これはBOTのCBW送信のような
  実データを伴うBulk OUTとは別物。実機での初検証になる
- **データトグルの持続**: `protocol.rs`のコントロール転送は毎回SETUPで
  トグルがリセットされる前提で書かれている（`data_stage_in`が常にDATA1から
  開始）。Bulkエンドポイントのトグルは転送をまたいで持続するため、
  `hcd::run_packet`を直接呼ぶ側（`msc.rs`）がトグル状態を自分で保持する
  必要がある。`hid_keyboard.rs`のInterrupt INポーリングは毎回DATA0/DATA1を
  固定引数で渡しており、これまでこの問題を踏んでいない
- **STALL回復**: BOT仕様はSTALL発生時に`CLEAR_FEATURE(ENDPOINT_HALT)`
  （標準リクエスト、recipient=endpoint）でエンドポイントのトグルとSTALL状態を
  リセットすることを要求する。未実装のままだと1回のSTALLでデバイスが
  以降ずっと応答しなくなる可能性がある

## Stage 1: MSCデバイスの列挙とBulkエンドポイント検出 ✅ 完了（実機確認済み）

新規モジュール`src/usb/msc.rs`。

- `protocol::enumerate_device`で列挙したMSCデバイスの`config_bytes()`を
  `hid_keyboard::find_hid_keyboard`と同じ要領で走査し、
  `bInterfaceClass=0x08`（Mass Storage）・`bInterfaceSubClass=0x06`（SCSI
  Transparent Command Set）・`bInterfaceProtocol=0x50`（Bulk-Only
  Transport）のインターフェースを探す
- 見つけたインターフェースに続くBulk IN/Bulk OUTの2エンドポイント記述子
  （`bEndpointAddress`のbit7で方向判定）からアドレスと`wMaxPacketSize`を
  控える
- `SET_CONFIGURATION`を発行する（`hub::Hub::open`と同じ理由。Addressステートの
  デバイスがBulk転送に応答する保証はConfigured後のみ）
- `shell.rs`に`usbmsc`コマンドを追加（`usbinfo`/`usbhub`と同じ構成）。
  `hcd::probe_port`→`protocol::enumerate_device`→上記の走査を行い、VID/PID・
  Bulk IN/OUTのエンドポイントアドレスと`wMaxPacketSize`を表示する
- ゴール: 実機でUSBメモリ（`USB_HOST_PLAN.md`のStage 4-4で使用したSony
  054C:0243等）をUSB-Aに直結し、`usbmsc`が正しいBulk IN/OUT
  エンドポイントアドレスを表示することを確認する。ハブ経由接続は
  Stage 0で述べた通り本計画の範囲外（「範囲外（将来検討）」参照）
  → **完了。実機のUSBメモリで`usbmsc`が
  `MSC (BOT) interface 0: bulk IN 0x81 (MPS 64), bulk OUT 0x02 (MPS 64)`
  を表示することを確認した（インターフェース0、Bulk IN/OUTとも
  MPS 64byteのFull-Speed機器）**

## Stage 2: Bulk転送プリミティブとトグル管理 ✅ 完了（実機確認済み、Stage 3と合わせて確認）

- `msc.rs`に`bulk_transfer_out`/`bulk_transfer_in`を実装する。中身は
  `hcd::run_packet`をBulk分類・対象エンドポイント番号で呼ぶだけだが、
  Stage 0で触れたトグル持続の問題があるため、`UsbMassStorage`構造体に
  `in_toggle`/`out_toggle`（`bool`、次に送るPIDがDATA1かどうか）を持たせ、
  転送のたびに読み書きする
- MPSより大きいデータは複数パケットに分割する（`protocol.rs`の
  `data_stage_in`と同じパターンをBulk版として実装する）
- STALL（`PacketOutcome::Error`かつ`HCINT_STALL`）を検出した場合の
  `CLEAR_FEATURE(ENDPOINT_HALT)`実装。標準リクエストなので
  `protocol::control_transfer_out_no_data`と
  `protocol::build_standard_out_setup`をそのまま使える見込み
  （`wValue=0`ENDPOINT_HALT、`wIndex=`対象エンドポイントアドレス）
- 単体では検証しづらいため、ゴールはStage 3のBOT実データ転送（INQUIRY）と
  合わせて確認する（このStage単体の実機確認コマンドは用意しない）
  → **完了。`UsbMassStorage::bulk_transfer_in`/`bulk_transfer_out`として
  実装し、`hcd::run_packet`をBulk分類・対象エンドポイント番号で呼ぶ形にした。
  `in_toggle`/`out_toggle`フィールドで転送をまたいだトグル持続を管理し、
  エラー時は`CLEAR_FEATURE(ENDPOINT_HALT)`（標準リクエスト、endpoint
  recipient）で回復を試みる。実機での検証結果はStage 3参照（初のBulk OUT
  実データ送信であるCBW送信を含め、STALLを一度も踏まずに成功した）**

## Stage 3: BOT (Bulk-Only Transport) CBW/CSW と SCSI INQUIRY ✅ 完了（実機確認済み）

- CBW（Command Block Wrapper、31byte固定）: signature `"USBC"`、tag（呼び出し
  ごとにインクリメントし、対応するCSWのtagと一致するか検証する）、
  data transfer length、flags（bit7=方向、IN=1）、LUN（0固定）、CB length、
  CDB（最大16byte、SCSIコマンド本体）
- CSW（Command Status Wrapper、13byte固定）: signature `"USBS"`、tag、
  residue、status（0=Passed、1=Failed、2=Phase Error）
- 最初のSCSIコマンドとしてINQUIRY（opcode `0x12`、6byte CDB）を実装する:
  CBWをBulk OUT → 36byteのINQUIRYデータをBulk IN → CSWをBulk IN →
  signature/tag/statusを検証する、という一連の流れを`msc::inquiry`として
  実装する
- `usbmsc`コマンドを拡張し、INQUIRYレスポンスのVendor
  Identification/Product Identification/Product Revision Level
  （ASCII、末尾スペースパディング）を表示する
- ゴール: 実機のUSBメモリで`usbmsc`がVendor/Product文字列を正しく表示し、
  CSW statusがPassed（0）であることを確認する
  → **完了。実機のUSBメモリ（Stage 1で確認したSony 054C:0243）で`usbmsc`が
  `Vendor: Sony      Product: Storage Media     Rev: 1.00`を表示した。
  Bulk OUT（CBW送信）・Bulk IN（INQUIRYデータ＋CSW受信）とも初回から成功し、
  STALL回復（`CLEAR_FEATURE(ENDPOINT_HALT)`）の実機経路は今回踏んでいない
  （Stage 0で想定した罠のうち、Bulk OUT自体の初検証は成功で通過。STALL
  回復ロジックは未検証のまま残る）**

## Stage 4: TEST UNIT READY / READ CAPACITY(10) ✅ 完了（実機確認済み）

- TEST UNIT READY（opcode `0x00`、6byte CDB、データフェーズ無し）でメディア
  準備状態を確認する。CSWがFailedを返した場合はREQUEST SENSE（opcode
  `0x03`）でセンスキーを読む処理を用意するが、対象をUSBメモリに絞るため
  詳細なリトライ・エラー分類は行わず、ログ出力にとどめる
- READ CAPACITY(10)（opcode `0x25`、10byte CDB）で最終LBAとブロック長
  （通常512）を取得する。`sdmmc.rs`の`SdCard.capacity_bytes`と対になる
  フィールドとして`UsbMassStorage`に持たせる
- ゴール: `usbmsc`が実機USBメモリの容量（バイト数、既知の実容量と一致する
  ことを目視確認）を表示する
  → **完了。実機のUSBメモリで`usbmsc`が`media ready`の後、
  `capacity: ... blocks x 512 bytes = ... MiB`を表示することを確認した。
  正確な公称容量との厳密な突き合わせは行っていないが、概ね妥当な値
  （ユーザー確認）。TEST UNIT READY／READ CAPACITY(10)ともCSW Passedで
  完走しており、REQUEST SENSE経路（media not ready時）は今回実機で
  踏んでいない**

## Stage 5: READ(10) によるブロック読み込み ✅ 完了（実機確認済み）

- SCSI READ(10)（opcode `0x28`、10byte CDB、LBAと転送ブロック数を指定）で
  1ブロック以上を読み込む。関数シグネチャは`sdmmc::read_block`/
  `read_blocks`に合わせ、`msc::read_blocks(&device, lba, buffer) -> bool`
  とする（呼び出し側からSD/USBの違いを吸収しやすくするため、Stage 6の
  前準備を兼ねる）
- `shell.rs`に`usbread <lba>`コマンドを追加（`sdread`と同じ構成:
  `sdmmc::dump_block`と同じダンプ関数をそのまま流用してUARTへ32行の
  16進ダンプを出す）
- この方式は`sdmmc.rs`と同じく「シェルコマンドのたびに`init()`から
  やり直す（永続状態を持たない）」パターンを踏襲する。Bulkトグルの
  持続はコマンド1回の実行内で閉じるため、複数コマンドをまたいだ
  トグル管理は考えない
- ゴール: 実機USBメモリの`usbread 0`が末尾510-511byteに`55 AA`の
  ブートシグネチャを含む、妥当なMBR/ブートセクタ内容を表示することを
  確認する（`sdread`のLBA0確認と同じ検証方法）

**踏んだ罠（実機、修正済み）**: `usbread`単体では読み込みに失敗する（Bulk IN
タイムアウト）が、先に`usbmsc`（INQUIRY→TEST UNIT READY→READ CAPACITY(10)を
実行）を叩いた直後だと成功することがある、という不安定な壊れ方が実機で
確認された。UARTログでは`USB MSC: bulk IN timed out`が、CBW送信（Bulk OUT）
ではなくREAD(10)のデータフェーズ（Bulk IN）で発生していた。

原因は2点と判断した。

- **`SET_CONFIGURATION`直後にいきなりREAD(10)を投げていた**こと。実際の
  BOTクラスドライバは初回READ/WRITE前にTEST UNIT READYで準備完了を
  ポーリングするのが通例だが、`usbread`はStage 5実装時点でこれを省略して
  いた。ドライブのファームウェアが列挙直後まだ内部処理中で、TEST UNIT
  READYを1往復以上要求するタイプだった可能性が高い（`usbmsc`が先に
  INQUIRY/TEST UNIT READY/READ CAPACITYの3コマンドを叩くことで、結果的に
  この「慣らし運転」を済ませてしまっていたために不安定に再現していたと
  考えられる）
- **`BULK_TIMEOUT_ITERATIONS`（`CONTROL_TIMEOUT_ITERATIONS`と同じ
  2,000,000）がREAD(10)のデータフェーズには短すぎた**可能性。INQUIRY/TEST
  UNIT READY/READ CAPACITYはファームウェアが即答できる小さな応答だが、
  READ(10)は実際にフラッシュへアクセスするため、同じタイムアウト予算では
  足りないことがある

対策として、`UsbMassStorage::wait_until_ready`（TEST UNIT READYを最大10回・
100ms間隔でポーリング）を追加して`usbread`が`read_blocks`の前に必ず呼ぶ
ようにし、`BULK_TIMEOUT_ITERATIONS`も2,000,000→20,000,000（10倍）へ拡大した。
→ **修正後、実機で`usbread 0`を`usbmsc`を挟まず複数回単体実行し、安定して
読み込めることを確認した。**

## Stage 6: ブロックデバイス抽象化とMBRパースの共通化（本計画のゴール） ✅ 完了（実機確認済み）

- `shell.rs`の`cmd_sdmbr`からMBRパース処理（446byte目からの4パーティション
  エントリ走査、`55 AA`シグネチャ確認、`partition_type_name`によるタイプ名
  変換）を、デバイスに依存しない形で新規`src/mbr.rs`へ抽出する。
  受け取るのは読み込み済みの`&[u8; 512]`のみとし、SD/USBのどちらから
  読んだかを一切知らない関数にする（例:
  `mbr::show(console: &mut Console, sector: &[u8; 512])`）
- SD/USB間でディスパッチする最小限の抽象を用意する。このプロジェクトは
  現状`dyn`/トレイトオブジェクトを一切使っておらず（`usb::connect_keyboard`の
  直結/ハブ分岐も`if`で完結する具体型のみ）、本計画の「最小限の抽象化」という
  方針にも合わせ、trait objectではなく列挙型で表現することを推奨する:

  ```rust
  enum BlockDevice {
      Sd(sdmmc::SdCard),
      Usb(usb::UsbMassStorage),
  }

  impl BlockDevice {
      fn read_block(&self, lba: u32, buffer: &mut [u8; 512]) -> bool {
          match self {
              BlockDevice::Sd(card) => sdmmc::read_block(card, lba, buffer),
              BlockDevice::Usb(dev) => msc::read_blocks(dev, lba, buffer),
          }
      }
  }
  ```

  これにより将来デバイス種別が増えても`match`を1箇所足すだけで済み、動的
  ディスパッチのオーバーヘッドも発生しない
- `shell.rs`の`sdmbr`/新設`usbmbr`は、それぞれ対応する`BlockDevice`を組み
  立てて共通の`mbr::show`を呼ぶだけの薄いコマンドにする。1コマンド+
  引数（例: `mbr sd`/`mbr usb`）へ統合する案もあるが、既存の`sdmbr`が
  無引数コマンドとして定着していることと、このプロジェクトの「新しい
  引数解析surfaceを増やすより薄いコマンドを並べる」既存の傾向
  （`sdread`/`sdreadn`/`sdwritetest`/`sdzero`が別コマンドなのと同様）に
  合わせ、**`sdmbr`/`usbmbr`を別コマンドのまま維持する**ことを推奨する
- ゴール（この計画全体のゴール）: `usbmbr`が実機USBメモリのパーティション
  テーブルを`sdmbr`と同じ表示形式で表示し、`sdmbr`が新しい共通コード
  （`mbr::show`）へ差し替えた後も既存のSDカードで従来通り動作すること
  （リグレッション確認）を実機で確認する
  → **完了。実機で`usbmbr`が`sdmbr`と同じ表示形式でパーティション
  テーブルを表示し、`mbr::show`への差し替え後も`sdmbr`が従来通り動作する
  （リグレッションなし）ことを確認した。これで本計画のゴールを達成した**

**実装時の変更点（計画からの差分）**: `BlockDevice`列挙型は実装しなかった。
`sdmbr`/`usbmbr`を別コマンドのまま維持する方針にした結果、各コマンドは
最初から自分がどちらのデバイスを相手にするか確定しており（`sdmbr`は
常に`sdmmc::read_block`、`usbmbr`は常に`UsbMassStorage::read_blocks`）、
実行時にSD/USBを切り替える呼び出し元が1つも無い。この状態で
`BlockDevice`を導入すると、一度も`match`されない列挙型と一度も呼ばれない
メソッドが残るだけになり（`dead_code`警告の対象）、本計画の
「タスクが要求する以上の抽象化をしない」という方針に反する。そのため
Stage 6は「`mbr::show`によるパース処理の共通化」だけを実装し、
デバイス選択の抽象化はまだ導入していない。将来Stage 4b（ファイル
システム層）が「SD/USBのどちらが挿さっていても同じコードで読む」という
実行時ディスパッチを本当に必要とした時点で、`BlockDevice`（または同等の
抽象）を導入するのが適切と判断する。

- `src/mbr.rs`（新規）: `mbr::show(console, sector: &[u8; 512])`。
  `cmd_sdmbr`から抽出したパーティション走査・`55 AA`シグネチャ確認・
  `partition_type_name`をそのまま移設。デバイスについて一切知らない
- `shell.rs`: `Line`（行フォーマッタ）を`pub(crate)`にして`mbr.rs`と共有。
  `cmd_sdmbr`は`sdmmc::read_block`でLBA 0を読んで`mbr::show`を呼ぶだけの
  薄い実装に置き換えた。新設`cmd_usbmbr`（`usbread`と同じ機器起動手順:
  `probe_port`→`enumerate_device`→`UsbMassStorage::attach`→
  `wait_until_ready`）が`read_blocks(0, ...)`で読んだLBA 0を同じ
  `mbr::show`へ渡す

## 範囲外（将来検討）

- FAT/exFAT等ファイルシステム本体の解析。`SD_CARD_PLAN.md`のStage 4b以降と
  共通のタスクとしてまとめて着手する想定（`BlockDevice`抽象はそのまま
  ファイルシステム層の下敷きにできる見込み）
- WRITE(10)による書き込み。SD側で`sdwritetest`実装時に踏んだ「書き込み後
  カードがビジー状態になる」に類する罠がUSB側にもある可能性が高く、
  読み込みが安定してから別途着手する
- 複数LUN対応、複数パーティションを跨いだGPT解析（`sdmbr`と同じくGPT
  保護MBRの検出止まりとする）
- MSCデバイスの複数同時使用、抜き差し耐性（`UsbKeyboard`の
  `needs_reinit`相当の自己回復）の作り込み。まずは1回のシェルコマンド
  実行内で完結する動作を優先し、`sdmmc.rs`/既存USBコマンドと同じ
  「コマンドのたびに再初期化」で十分とする
- リムーバブルメディア（USBカードリーダー等）のメディア差し替え検出
  （UNIT ATTENTION、SCSIのメディア変更通知）
- **USB MSCのハブ経由接続**（→ `USB_REFACTOR_PLAN.md` Stage Fで対応済み。
  以下は本計画完了時点の記録）。`usb::connect_keyboard`が持つ「直結／ハブ経由の
  判定」（`enumerate_device`をハブのポート越しに呼ぶ分岐、
  `USB_HOST_PLAN.md`Stage 4-5）に相当する`connect_mass_storage`のような
  関数は未実装。`usbmsc`/`usbread`/`usbmbr`はいずれも`usbinfo`と同じく
  `usb::enumerate_device(usb::ROOT_DEVICE_ADDRESS, false)`でUSB-A直結
  デバイスのみを対象としており、ハブの下に挿したUSBメモリは扱えない
  （Stage 1完了時点のゴール文言が「直結でもハブ経由でも可」としていたのは
  誤りで、実装・実機確認とも直結のみ。修正済み）。Low-Speedデバイス自体も
  MSC用途では通常存在しないため、`hcd::Endpoint::low_speed_via_hub`絡みの
  検証もしていない

## 想定される罠（実装前メモ、実機で要検証）

`SD_CARD_PLAN.md`・`USB_HOST_PLAN.md`同様、以下は実機でしか踏めない可能性が
高い項目として着手前に注意しておく。

- **Bulk OUTの初検証**: これまでOUT方向のBulkデータフェーズを一度も実機で
  送っていない。コントロール転送のOUTステータスステージ（0byte、
  トグル固定）とは別物であり、CBW送信（31byte、Bulk OUT）で初めて
  実データを伴うOUT転送を行うことになる
- **トグルの持続とチャネル0共有**: `hcd.rs`は「チャネル0を1パケットごとに
  全部書き切る、呼び出し間の状態を持たない」設計（`USB_HOST_PLAN.md`の
  モジュール構成参照）。Bulkのトグルだけは例外的に呼び出し側
  （`msc.rs`）が持続管理する必要があり、既存の設計方針と初めて
  部分的に食い違う。トグルの初期値（Configured直後はDATA0）を
  取り違えると、STALLではなく「データ化けだけして見た目は成功する」
  という気づきにくい壊れ方をする可能性がある
- **STALL回復の要否**: `CLEAR_FEATURE(ENDPOINT_HALT)`を実装しないまま
  STALLを踏むと、`UsbKeyboard`のような自己回復（`needs_reinit`）が
  無い限りそのセッションは詰む。最初のINQUIRY実装時点でSTALLが
  起きるかどうかは実機依存
- **CBWのtag管理**: CSWのtagがCBWのtagと一致することを確認しないと、
  古い応答を新しい応答として誤読する可能性がある（BOT仕様が要求する
  検証）
- **32bit LBA上限**: READ CAPACITY(10)は32bit LBAまで（2TiB相当）。
  対象はUSBメモリのため通常問題にならない想定だが、大容量デバイスで
  容量が異常値になった場合はREAD CAPACITY(16)への切り替えが必要になる
  可能性がある

## モジュール構成（実際）

- `src/usb/msc.rs`: BOT CBW/CSW組み立て、Bulk転送プリミティブとトグル
  管理（`bulk_transfer_in`/`bulk_transfer_out`）、STALL回復
  （`CLEAR_FEATURE(ENDPOINT_HALT)`）、SCSIコマンド（INQUIRY/
  TEST UNIT READY/`wait_until_ready`/REQUEST SENSE/READ CAPACITY(10)/
  READ(10)）、`UsbMassStorage`構造体。`find_msc_interface`（Stage 1）も
  同じファイル
- `src/usb.rs`: `msc`のサブモジュール宣言・`UsbMassStorage`/
  `find_msc_interface`の再エクスポートを追加。直結/ハブ経由の判定
  （`connect_keyboard`相当の`connect_mass_storage`）は未実装のまま
  （`usbmsc`/`usbread`/`usbmbr`は`usbinfo`と同じく直結デバイスのみを
  対象にしている。ハブ経由対応は範囲外のまま）
- `src/mbr.rs`（新規）: `cmd_sdmbr`から抽出したMBRパース処理
  （`&[u8; 512]`のみを受け取り、デバイスについて何も知らない）。
  `shell.rs`の`Line`（`pub(crate)`化）を使って出力行を組み立てる
- `src/shell.rs`:
  - `usbmsc`コマンド追加（Stage 1〜4の確認用: 列挙・エンドポイント・
    INQUIRY・TEST UNIT READY・READ CAPACITY(10)）
  - `usbread <lba>`コマンド追加（Stage 5の確認用。`read_blocks`前に
    `wait_until_ready`を挟む）
  - `usbmbr`コマンド追加、既存`sdmbr`は共通の`mbr::show`呼び出しへ
    差し替え（Stage 6）

## 各段階の完了条件（実機確認）

`SD_CARD_PLAN.md`・`USB_HOST_PLAN.md`と同じく、各StageはUARTシェル経由で
コマンドを叩いて目視確認できることをもって完了とし、次のStageへ進む前に
必ず実機でログを確認する。
