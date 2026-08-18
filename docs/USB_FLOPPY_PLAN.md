# USB Floppy対応 実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
>
> この文書は作業計画です。実装後の仕様は現状文書とコードを優先します。

## 状態: 中断（Stage 1 直結実機確認済み／Stage 2 CBI ADSCで未解決／Stage 3〜4 未着手）

## 中断時点の記録（2026-08-19）

直結実機のSony `054C:002C`は、interface `08/04/00`、Bulk IN `0x81`（MPS 64）、
Bulk OUT `0x02`（MPS 64）、CBI status Interrupt IN `0x83`（MPS 2）として安定して
列挙できた。構成設定まで成功し、VBUS不足または接続喪失を示す状態ではない。

CBI ADSC（12 byte CDBを伴うclass/interface control OUT）は、再列挙直後に直ちに
実行する`usbfloppyprobe`でもSETUP段階で失敗した。最終試験ではdescriptor-DMAの
SETUPパケットにQTDのSETUPフラグだけでなく`HCTSIZ.PID=SETUP`を指定したが、結果は
変わらなかった。

```text
USB: transfer transaction error, HCINT=0x00001002
USB:   HPRT=0x0002140F
USB:   HFNUM before +1ms=0x0CF00033
USB:   HFNUM +1ms=0x04F20034
USB: control transfer failed at the SETUP stage
USB Floppy: CBI ADSC failed
```

`HPRT`は接続・有効のままで、HFNUMも1 msで進行している。構成直後の最初の標準
`GET_STATUS`だけは成功する一方、その直後の次のSETUP（ADSC、`SET_INTERFACE(0)`、
または別の`GET_STATUS`）は失敗した。待機時間と`SET_INTERFACE(0)`の再選択では改善
しなかった。

この時点でFloppy機能の実装を中断する。`src/usb/floppy.rs`は調査再開用に残すが、
`usb.rs`からは読み込まず、レジストリもクラスドライバとして選択しない。`usbfloppy`と
`usbfloppyprobe`は削除した。HCDのSETUP PID修正、制御OUTデータステージ、制御転送の
回復リトライ、HFNUM診断、未対応デバイスログの抑制、MSC用BOT分離は汎用USB改善として
残す。

USB-Aホストに接続した3.5インチ USB Floppy Driveから、**1.44 MB（2HD）
FAT12フォーマット済みメディアを読み取り専用で扱う**。完了時には、ドライブの
列挙、メディアの有無・形式の判定、任意セクタの読み出し、ルートディレクトリの
8.3形式での一覧表示をUARTシェルから行えることをゴールとする。

対象ドライブは USB Mass Storage Class、UFI command set、CBI transport
（`bInterfaceClass=0x08`、`bInterfaceSubClass=0x04`、
`bInterfaceProtocol=0x00` Control/Bulk/Interrupt）を報告するものに限定する。
Linuxで実機のinterface値が`08/04/00`であることを確認済みである。
現在の `usb::msc` は SCSI Transparent（subclass `0x06`）のUSBメモリ用であり、
UFI/CBIを誤って同じドライバへ通さない。

直結実機（VID:PID `054C:002C`）で、`usbrescan`／`usbinfo`によりドライバ登録までを
確認した。使用するendpointはBulk IN `0x81`（MPS 64）、Bulk OUT `0x02`（MPS 64）、
CBI status Interrupt IN `0x83`（MPS 2）である。ハブ配下の確認と既存MSCの回帰確認は
未実施である。

Stage 2の初回実機試験では、メディア認識の最初のCBI ADSC制御要求がEP0のSETUP段階で
`HCINT=0x00001002`（`XCS_XACT_ERR`）となった。一方、直前の`SET_CONFIGURATION`を含む
標準制御要求による列挙は成功しており、HPRTも接続・有効のままである。CBI ADSCの要求値は
Linuxの標準実装と一致することを確認済みで、現在は`SET_CONFIGURATION`直後と同一
セッションの標準`GET_STATUS`、および失敗時のUSBフレーム番号を記録して、ADSC以前の
EP0失効原因を診断している。構成直後のGET_STATUSは成功し、後からのGET_STATUSは
`XCS_XACT_ERR`となる一方で、失敗時もHFNUMのフレーム番号は進行した。次に再列挙直後の
ADSCを`usbfloppyprobe`で検査したところ、構成直後の最初のGET_STATUSだけが成功し、
直後の次のSETUPが失敗した。待ち時間と`SET_INTERFACE(0)`の再選択では改善しなかった。
descriptor-DMA時にQTDのSETUPフラグだけでなく`HCTSIZ.PID=SETUP`を指定すべきところを
DATA0としていたため、HCDを修正して再検査した。しかし上記のとおり結果は変わらず、
本計画は中断している。

フロッピーは実用上ほぼ 1.44 MB FAT12 であることを前提にする。ただし誤った
メディアを通常形式として読まないよう、認識時にSCSI/UFIの容量とブートセクタの
BPBを検査する。対応する論理配置は次で固定する。

| 項目 | 値 |
| --- | --- |
| 論理セクタ | 512 byte × 2,880（最終LBA 2,879） |
| FAT | FAT12、予約領域 1 sector、FAT 2本 × 9 sectors |
| ルートディレクトリ | LBA 19〜32、224 entry（14 sectors） |
| データ領域先頭 | LBA 33 |

BPBは上記値との一致を確認するために読むだけで、任意のFAT12形状への一般化は
行わない。

## 共通方針と範囲

既存の `UsbHost` がUSBバスの単一所有者である構造は維持する。直結および現在
対応済みの1段USBハブ配下で、レジストリがFloppyを発見・保持する。各シェル
コマンドはレジストリの同一セッションを使い、コマンドごとのポート再列挙は
行わない。

UFI/CBIでは、12 byteのCDBをクラス固有の制御転送で送信し、データはBulk endpoint、
コマンド完了はInterrupt IN endpointで受ける。利用するコマンドはTEST UNIT READY、
REQUEST SENSE、READ CAPACITY(10)、READ(10)だけに限る。
USBメモリ用のBOT（CBW/CSW）とは転送方式を共有しない。

新設するシェルコマンドは次を予定する。

| コマンド | 内容 |
| --- | --- |
| `usbfloppy` | UFI/CBIドライブのinterfaceとendpointを表示し、メディア認識結果を表示 |
| `usbfloppyread <lba>` | 512 byteの論理セクタを読み、UARTへダンプ |
| `usbfloppyls` | 固定位置のルートディレクトリを読み、エントリを一覧表示 |

## Stage 1: UFI/CBIクラスドライバの作成（直結実機確認済み）

### 実装

- `src/usb/msc.rs` からBOTの共通処理を `src/usb/bot.rs` へ抽出する。これは既存の
  SCSI Transparent USBメモリの整理であり、UFI/CBI Floppyとは共有しない。
  `UsbMassStorage` の既存シェルコマンドの挙動を変えない。
- `src/usb/floppy.rs` に `UsbFloppy` とUFI用の設定記述子走査を追加する。
  対象は Mass Storage / UFI / CBI の組だけとし、Bulk IN、Bulk OUT、完了通知用
  Interrupt INの3 endpointが揃わない記述子は接続対象にしない。
  `SET_CONFIGURATION`後にCBIセッションを開始する。
- `src/usb/registry.rs` の `DeviceKind` にFloppyを追加し、直結・ハブ配下の
  いずれでも既存のMSCやHIDと同じ走査経路から登録する。
  `mass_storage_mut()`とは別に `floppy_mut()` を用意し、USBメモリをFloppyとして
  誤選択しないようにする。
- `src/usb.rs` の再エクスポートと `src/app/shell.rs` の `usbfloppy` コマンドを
  追加する。Stage 1時点ではVID/PID、インターフェース番号、Bulk endpoint、
  Interrupt IN endpointとMPSを表示して、クラス判定とレジストリ登録を診断可能にする。

### 完了条件（実機）

- ドライブを直結して `usbrescan` 後の `usbinfo` にFloppyとして現れ、
  `usbfloppy` がUFI/CBIのinterface、Bulk IN/OUT、status Interrupt IN endpointを表示する。
- 1段ハブ配下でも同じドライブを認識できる。
- USBメモリを接続したとき、既存の `usbmsc`／`usbread`／`usbmbr` が従来通り
  動き、Floppyとして登録されない。

## Stage 2: メディア認識（実装中、CBI ADSC実機調査中）

### 実装

- `UsbFloppy::probe_media()` を追加し、12 byte CDBのTEST UNIT READYを実行する。
  成功時にREAD CAPACITY(10)を続け、`last_lba == 2879`かつ
  `block_length == 512`だけを1.44 MB候補として受け入れる。
- TEST UNIT READYが失敗した場合はREQUEST SENSEを実行し、ASC/ASCQをコンソールに
  16進で表示する。状態名への分類と、読み取り中のメディア変更検出はStage 3で追加する。
- 容量確認後にLBA 0を読み、BPBの bytes/sector、sectors/cluster、reserved
  sectors、FAT数、root entry数、total sectors、sectors/FAT、`0x55AA`署名を
  上表の1.44 MB FAT12値と照合する。すべて一致したときだけ状態を
  `Ready1440Fat12` とし、以後の読み取り・列挙を許可する。
- 認識済み状態の保持はまだ行わない。各 `usbfloppy` 実行で、TURからBPB検査までを
  改めて実行する。

### 完了条件（実機）

- メディアなしで `usbfloppy` を実行すると、ドライブ未接続と区別して
  「メディアなし」と表示される。
- FAT12で初期化した1.44 MBディスクを挿入すると、`media ready: 1.44 MB FAT12`が
  表示される。
- 非対応容量またはBPB不一致のメディアでは、理由を表示して `usbfloppyread` と
  `usbfloppyls` を拒否する。

## Stage 3: ディスクの読み取り

### 実装

- `UsbFloppy::read_sector(lba, buffer)` を追加する。`Ready1440Fat12`状態だけで
  UFI READ(10)を12 byte CDBとして発行し、512 byteを読み込む。範囲は
  `0..=2879` に限定し、範囲外をデバイスへ送らない。
- `usbfloppyread <lba>` を追加する。LBA 0のブートセクタ、FAT先頭、ルート
  ディレクトリ先頭などを既存のダンプ形式で確認できるようにする。
- CBIのBulk INデータ長と2 byteのstatus Interrupt INを確認し、短い転送や
  異常statusを正常なセクタとして扱わない。失敗時はStage 2と同じセンス取得・
  状態失効を行う。

### 完了条件（実機）

- `usbfloppyread 0` がブートセクタを512 byteダンプし、BPBと末尾の`55 AA`が
  認識時の検査結果と一致する。
- `usbfloppyread 19` と `usbfloppyread 32` がルートディレクトリの先頭・末尾を
  読める。範囲外LBAはUSB転送を起こさず明確に失敗する。
- 同一メディアで複数回読み取り、取り出し後には成功結果を再利用しないことを
  実機で確認する。

## Stage 4: ルートディレクトリの列挙

### 実装

- `src/app/floppy.rs` に、固定のLBA 19〜32を順番に読み込む最小のFAT12ルート
  ディレクトリ表示を実装する。ストレージ層の `UsbFloppy` はFATの知識を持たず、
  ディレクトリ解釈はアプリ層に閉じる。
- 32 byteエントリを224件まで走査する。先頭byte `0x00`で終了、`0xE5`の削除済み
  エントリは無視、属性`0x0F`のVFAT long-file-nameエントリは無視する。
  通常の8.3名、属性（file/directory/volume label）、開始クラスタ、ファイル長を
  表示する。時刻、VFAT名の復元、CP437以外の文字コード変換は行わない。
- `usbfloppyls` はStage 2の認識を確認してから上記を実行する。空ディレクトリ、
  volume label、ディレクトリ属性を誤って通常ファイルとして表示しない。
- ディレクトリ読み取りが途中で失敗した場合は、それまでの不完全な一覧を成功扱い
  せず、読めなかったLBAとメディア状態を表示する。

### 完了条件（実機）

- PCで作成したテスト用1.44 MB FAT12ディスクを使い、ルート直下の複数の8.3形式
  ファイル、空ファイル、ディレクトリ、volume labelを `usbfloppyls` で確認する。
- 削除済みエントリとVFAT補助エントリを一覧に出さず、空ディレクトリでは正常終了
  する。
- ルートに224 entryを置いたディスク、または最後のセクタまで有効なエントリがある
  ディスクでもLBA 32まで正しく走査する。

## 範囲外・将来タスク

- 書き込み全般: UFI WRITE(10)、FAT更新、空きクラスタ管理、ディレクトリエントリの
  作成・更新、フォーマット。読み取りが実機で安定し、専用の捨てメディアを用意した
  別タスクで扱う。
- ルート以外のディレクトリ、FAT12クラスタチェーン、ファイル内容の読み出し、
  VFAT long file name、日時・文字コードの完全な解釈。
- 1.44 MB以外の容量、FAT16/FAT32/exFAT、SCSI TransparentまたはUFI/BOTとして
  報告するFloppy、UFI以外のMSCサブクラス、複数LUN。
- 常駐のメディア変更監視、複数Floppyの同時選択、読み取り性能の最適化。

## モジュール構成（Stage 1時点）

- `src/usb/bot.rs`（新規）: USBメモリ用BOTのCBW/CSW、Bulk転送、トグル、回復。
  `msc.rs`だけが利用し、UFI/CBI Floppyとは共有しない。
- `src/usb/msc.rs`: 既存のSCSI Transparent USBメモリ用ドライバ。BOT共通化後も
  INQUIRY／READ CAPACITY(10)／READ(10)の公開挙動を維持する。
- `src/usb/floppy.rs`（新規）: UFI/CBI記述子の検出、構成設定、endpoint記録、12 byte
  CDBのCBI制御転送、TUR／REQUEST SENSE／READ CAPACITY(10)／LBA 0読み取りによる
  1.44 MB FAT12メディア認識。任意セクタの公開読み取りはStage 3で追加する。
- `src/usb/registry.rs`: Floppyの登録と取得。USBバスの所有権は引き続きここだけに
  置く。
- `src/app/floppy.rs`（新規）: 固定1.44 MB FAT12ルートディレクトリの読み取り専用
  表示。
- `src/app/shell.rs`: `usbfloppy`、`usbfloppyread`、`usbfloppyls` の引数解析と
  表示。

## 実機検証の注意

USB Floppyは機種により、メディアなし時のTEST UNIT READY失敗、回転開始までの待ち、
メディア変更後のUnit Attentionの返し方が異なる。各Stageの完了はUARTログを残して
確認し、少なくとも「メディアなし」「既知の1.44 MB FAT12」「取り出し後の再実行」の
3状態を同じドライブで検証する。複数メーカーのドライブへ対応範囲を広げる判断は、
この最小対応の実機結果を得た後に行う。
