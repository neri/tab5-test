# ファイルシステム実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。実装を開始した後の仕様は現状文書とコードを優先します。

## 状態

**Stage 1完了（2026-08-24、実機確認済み）。** ブロックデバイス層、MBR判定、RAMディスク
（PSRAM固定8 MiB、起動時FAT16 format）、SD／USB／RAMのadapter、`devices`／`blkread`
コマンドを実装し、実機でRAMディスクのFAT判定、SDのMBRとpartition相対読み出し、USBのMBRを
確認した。FAT/exFATライブラリは`hadris-fat`を採用した（下記の選定ゲート）。VFSは未着手である。
現状の実装は[`STORAGE.md`](STORAGE.md)の「ブロックデバイス層」節と[`PSRAM.md`](PSRAM.md)の
「32 MiBの分割」節を参照する。

Stage 1〜4は完了、Stage 5は実装済みだが完了条件の一部が未検証である（下記各Stage）。

Stage 1の実機確認中に、SDカードのIDMACバッファが64 byte境界を要求するのに守られておらず、
キャッシュ無効化の失敗が握り潰されていた既存の不具合が見つかった。症状は「エラーなしで
全ゼロが返る」で、呼び出し場所のスタック配置によって成否が変わっていた。詳細は
[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)と[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)の追補にある。

Stage 0の破損イメージによるホストテストは**別タスクへ保留**した（利用者判断、2026-08-24）。
このリポジトリはターゲット固定の単一バイナリcrateで、ホストでテストを走らせる仕組みが
無いためである。導入するにはworkspace分割かlibターゲット追加が要る。したがって現時点の
Stage 1〜5の完了条件からホストテスト部分は外れており、破損媒体の拒否は未検証のままである。
`mbr.rs`と`bootsector.rs`の検査コードは書いてあるが、実際に壊れたイメージへ通してはいない。

SDカードとUSB Mass Storageは512 byteブロックの読み書きとMBR表示まで実装済みで、
FAT/exFAT、VFSは未実装である。本体Flashは今回のスコープ外とし、
Flash専用ファイルシステムを検討する将来の別計画で扱う。既存実装の詳細は
[`STORAGE.md`](STORAGE.md)を、SDとUSBの実機上の制約は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)と[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)
を参照する。

## 目的と最初の対応範囲

アプリから媒体ごとのI/O手順を隠し、パス名でファイルを扱えるようにする。最初に目指すのは、
PCで作成したFAT媒体から安全にファイルを列挙・読み出しできることとする。書き込みはPSRAM
RAMディスクだけで対応し、SDカードとUSB Mass Storageは全Stageで読み取り専用とする。
exFATは読み出しを先に追加し、書き込みを実装する場合もRAMディスクだけを対象にする。
本体Flash永続化は目的に含めない。

| 媒体 | 初期用途 | 初期のアクセス | マウント候補 | 備考 |
| --- | --- | --- | --- | --- |
| PSRAM RAMディスク | 一時ファイル、FS試験 | 読み書き | `/ram` | 固定8 MiB。リセット・電源断で消える。`RamBlockDevice`として提供する |
| microSD | ユーザーファイル | 読み取り専用 | `/vol/sd0pN` | MBRの各primary partitionを別々にマウントできる |
| USB Mass Storage | 起動媒体・読み出し | 読み取り専用 | `/vol/usbNpM` | 複数MSCと各MBR primary partitionを区別し、取り外しとBOT session失効を検出する |
| SPI Flashの`storage`領域 | 将来の永続データ | 今回のスコープ外 | 将来の`/flash` | 16 MiB Flashのうち`0x400000..0x1000000`、すなわち12 MiBが予約済み。FATは使用せず、Flash専用FSを別計画で検討する |

「読み取り専用」は今回のVFS mount policyがファイル書き込みを拒否する意味であり、
ブロックデバイスの共通interfaceや物理デバイスの書き込み能力とは別である。SD/USBの既存raw
書き込み診断コマンドはこの方針の対象外として残すが、VFSからは呼び出さない。

## RAMディスクの配置と初期化

RAMディスクはheapから動的に8 MiB確保せず、PSRAMの固定領域として最初からheap対象外にする。
現状の32 MiB mappingは次の3領域へ分ける。

```text
0x48000000..0x481c2000  framebuffer（1,843,200 byte）
0x481c2000..0x489c2000  RAM disk（8 MiB）
0x489c2000..0x4a000000  heap（23,322,624 byte、約22.24 MiB）
```

`Psram`は`framebuffer()`、`ram_disk()`、`heap()`を別々に返し、`heap()`はRAM disk終端から
開始する。固定予約により、長寿命の8 MiB allocationによるheap断片化、起動後の確保失敗、
誤解放を避ける。容量のruntime変更は実装しない。

起動ごとにRAM disk全体を初期化してFAT16でformatし、その後`/ram`へread-write mountする。
初期化失敗時は`/ram`を作らず、heapへ領域を返すfallbackも行わない。PSRAM layoutをbootごとに
変えないためである。

## 採用する名前空間

複数ファイルシステムは**Unix型の単一ツリーと固定マウントポイント**で組み合わせる。
ドライブ番号を公開APIにはしない。

```text
/
├── ram/             PSRAM RAMディスク（読み書き）
└── vol/
    ├── sd0p1/       microSDのMBR entry 1（読み取り専用）
    ├── sd0p2/       microSDのMBR entry 2（読み取り専用）
    ├── usb0p1/      1台目のUSB MSCのMBR entry 1（読み取り専用）
    └── usb1p1/      2台目のUSB MSCのMBR entry 1（読み取り専用）
```

将来のFlash対応では同じ名前空間へ`/flash`を追加できるが、今回のmount tableと完了条件には
含めない。

- 初期実装は固定の`/ram`と`/vol`をrootに持つ。一つの媒体にある複数のprimary partitionは
  `/vol`直下の別mountとして同時に扱う。bind mount、overlay、相対パスは初期範囲外とする。
- APIとシェルは絶対パスを使う。例えば`/vol/sd0p1/photo.jpg`である。ドライブ番号のような
  `0:/`表記は、必要になってもUI上の別名に留め、内部のパス解決規則を二重化しない。
- マウント操作は明示的に行う。VFSでは利用可能な全partitionを同時に別々の場所へ
  マウントできる。起動データの検索順を決める場合も、mountの有無とは分離する。
- USB root/hub portの物理的なconnection-changeを観測した時は、対応するmountを直ちに
  `stale`にし、open済みhandleを`MediaRemoved`で無効化する。転送障害による通常のrescanでは
  直ちに無効化せず、下記の同一性確認が終わるまで`Suspended`にする。

### デバイスとpartitionの識別

mount名は`<device-id>p<partition-number>`とする。

- オンボードmicroSDスロットは常に`sd0`。
- USB MSCは`UsbHost`レジストリ内の物理トポロジー順（root、hub port昇順）で`usb0`、
  `usb1`、...を割り当てる。この番号はそのレジストリ世代内だけ有効で、永続的な媒体IDではない。
- USB Addressは列挙のたびにhostが再割り当てし、抜去後に別デバイスへ再利用できるため、
  `DeviceId`には使わない。SCSI commandのrouting情報としてadapter内部だけに保持する。
- `p1`〜`p4`はMBRの4個のprimary entry番号をそのまま表す。空entryにはmountを作らない。
- USB serial、SCSI device identifier、SD CID、FAT/exFAT volume serialはfingerprintの構成要素に
  使い、`devices`と`mounts`にも表示する。ただし欠落・重複・cloneがあり得るため、どれか一つを
  単独の内部IDにはしない。volume labelは表示だけに使う。
- mountとfile handleは`VolumeId { device_id, partition_number, media_generation }`を保持し、
  番号が再利用されても古いhandleを新しい媒体へ向けない。SDにもmedia generationを持たせる。
- mount tableは`/vol/usb1p2`の文字列を直接デバイス検索に使わず、解決済みの`VolumeId`と
  partitionの開始LBA・長さを保存する。表示名の再割り当てとopen済みhandleの同一性を分離する。

現状の`UsbHost`は複数slotへMSCを保持できる一方、`mass_storage_mut()`は最初の1台だけを返す。
複数USB対応時はslotを指定してMSCを借りるAPIと、全MSCを列挙するread-only inventory APIを
追加し、ファイルシステム層から`mass_storage_mut()`を使用しない。

### USB rescanとgeneration

generationはUSB Addressや列挙sessionの世代ではなく、VFSから見た**媒体同一性の世代**とする。
`UsbHost::rescan()`にはreasonを渡し、少なくとも`Recovery`、`Manual`、
`PhysicalConnectionChange`、`PowerRecovery`を区別する。
rescan開始時に未処理のroot/hub connection-changeを先に採取し、存在すれば呼出元が指定した
`Manual`/`Recovery`より`PhysicalConnectionChange`を優先する。rootの物理edgeは配下全slot、
hub portのedgeはそのportのslotだけを対象にする。softwareによるport resetやVBUS power recoveryは
それだけでは物理抜き差しと見なさない。

1. rescan前に、slotごとの旧fingerprint、generation、mount、open handleを保存し、対象mountを
   `Suspended`にする。`Suspended`中のI/Oはhandleを破棄せず`TemporarilyUnavailable`を返す。
2. 再列挙後、同じ物理トポロジーで次を比較する。
   - USB device/config descriptor（USB Addressを除く）と、`iSerialNumber`が非0ならserial string
   - 対応していればSCSI VPD page `0x80`（Unit Serial Number）と`0x83`（Device Identification）
   - MSC standard INQUIRY、論理block長、総block数
   - LBA 0のMBR disk signature/partition tableと、mount中partitionのboot sector fingerprint
   - FAT12/16/32またはexFATのvolume serial
3. reasonが`Recovery`、`Manual`または`PowerRecovery`で、deviceとvolumeのfingerprintが一致すれば、
   新しいUSB Address・endpoint/sessionだけを`BlockDeviceRegistry`へrebindする。generation、mount、
   logical handle、read-only sector cacheは維持して`Active`へ戻す。ファイルシステムライブラリの
   内部session/handleは信用して持ち越さず、mountを再生成してopen fileを同じpathでreopenし、
   VFSが保持する確定済みoffsetへseekする。再openできないhandleだけを`RebindFailed`にする。
4. fingerprintが変わった場合は別媒体としてgenerationを進め、旧mountとhandleを
   `MediaChanged`で無効化する。カードリーダー本体が同じでも、容量、MBR、partition boot sectorが
   変われば挿入メディアが変わったと判断する。
5. root HPRTの物理connection edge、hubの`C_PORT_CONNECTION`、明示的なnot-connected状態を
   rescan前に観測した場合はreasonを`PhysicalConnectionChange`にする。この場合は再接続後の
   fingerprintが完全に同じでもgenerationを必ず進め、旧handleを再利用しない。
6. 再列挙またはfingerprint取得に失敗しただけなら同一性は未確定であり、generationを進めず
   `Suspended`のままにする。後続rescanで一致すれば復帰し、物理edgeまたは不一致が確定した時だけ
   無効化する。明示`umount`はいつでも`Suspended`状態を破棄できる。

現在の`rescan()`は全driverを捨ててroot portをresetし、USB Addressを無条件に再割り当てするため、
このsnapshot/rebindはファイルシステム導入時に追加する。rescan自身のresetが生成したconnection
change bitは現在どおり捨て、rescan開始前に捕捉した物理edgeと混同しない。
`power_cycle_and_rescan()`も`clear()`より前にsnapshotを取り、電源復旧後のfingerprint比較に使う。

fingerprintは同一性の実用的な確認であり、同一内容へcloneした媒体を数学的に区別するものではない。
ただし観測済みの物理edgeを常に優先してgenerationを進めるため、同じUSBメモリを抜いて挿し直した
場合も旧handleが復活することはない。

USB `iSerialNumber`はoptionalで、未実装、空文字、全個体で同じ値の製品がある。SCSI VPDも
USBメモリによっては未対応である。FAT12/16/32とexFATのvolume serial、classic MBRのdisk
signatureはいずれも32 bitで、一般的な表示で「UUID」と呼ばれる場合があっても128 bit UUIDではなく、
0や重複値、formatやimage cloneによる複製があり得る。したがって、取得できたserial/identifierが
変われば即座に別媒体と判定し、一致した場合はトポロジー、容量、partition表、boot sectorを含む
複合fingerprintの一致条件として使う。識別用UUIDを媒体へ新規書き込みすることは今回行わない。

microSDはUSBとは別に、初期化時に取得済みのCID全体（MID/OID/PNM/PRV/PSN/MDTを含む）を
device fingerprintへ使う。CIDが変われば同じ`sd0`スロットでもmedia generationを進める。

この復帰のため、VFSのlogical file handleは採用ライブラリのhandleそのものではなく、
`VolumeId`、正規化済みpath、open mode、最後に成功したI/Oまでのoffsetを持つ。readが途中で
失敗した場合はoffsetを進めず、再bind後に同じ位置から再試行できるようにする。directory iteratorも
directory pathと確定済みentry位置を保持し、必要なら先頭からそこまで読み飛ばして復元する。

## 層構成

提案する境界は次のとおりである。「パーティション」はブロックデバイスを作る層であり、
ファイルシステムドライバと同じ層には置かない。

```text
VFS（パス解決、mount table、ファイルハンドル、媒体世代）
  └─ ファイルシステムドライバ（FAT / exFAT）
       └─ PartitionBlockDevice（DeviceId、開始LBA・長さで範囲を切る）
            └─ BlockDeviceRegistry（DeviceIdから実デバイスへdispatch）
                 ├─ MBR parser（LBA 0を検査してprimary partitionを列挙）
                 └─ DiskBlockDevice adapter（SD / USB MSC / RAM）
                      └─ ハードウェア・転送ドライバ
```

同じディスクの`p1`と`p2`は二つの`PartitionBlockDevice`になるが、USB/SDドライバを二重に
所有しない。各partitionは`DeviceId`とLBA範囲だけを持ち、I/Oのたびに中央の
`BlockDeviceRegistry`へdispatchする。VFSは単一threadで一度に一つのI/Oだけを進めるため、
同じ物理デバイス上の複数ファイルシステムも安全に直列化できる。

最下層は、RAM、SD、USBで共通の同期`BlockDevice`を一つだけ定義する。read-only用と
read-write用にtraitを分けない。

- `BlockDevice::geometry()`は論理ブロック長と総ブロック数を返す。LBAと容量は`u64`で
  表し、現在の512 byte SD/USBだけに型を固定しない。
- `BlockDevice::read_blocks(lba, buffer)`は、バッファ長が論理ブロック長の整数倍であること、
  範囲内であることを検査する。失敗時に部分成功を成功として返さない。
- 同じ`BlockDevice`が`write_blocks(lba, buffer)`と`flush()`も持つ。`PartitionBlockDevice`も
  read/writeの両方で同じ範囲検査とLBA変換を行う。
- 今回VFSからwriteを呼ぶのは`RamBlockDevice`だけである。SD/USB adapterにも同じmethodを
  用意してinterfaceを固定するが、ファイルシステム経由の呼び出しと書き込み受入試験は
  物理媒体書き込みを実装する将来Stageまで行わない。
- SDとUSBの既存ドライバをこのtraitのために作り直さない。`sdmmc.rs`と`usb/msc.rs`の上に
  小さなadapterを置き、媒体ごとの初期化、DMA制約、USB BOT recoveryを下層に残す。

FATライブラリがbyte単位の`Read`/`Write`/`Seek`を要求する場合は、PartitionBlockDeviceの上に
一つだけadapterを置く。任意byte offsetを毎回1セクタ読み直す実装にはせず、PSRAMに小さく
固定長のsector cache（初期値4 KiB/mount）を持たせる。内部SRAMはSD/USB DMA stagingと
descriptor用に残す。PSRAM cacheをDMA転送先にする場合は、既存のcache writeback/invalidateを
adapter境界で行う。表示DMAとの帯域競合、mount数に比例する使用量、read-ahead有無は実測する。

## パーティションと媒体の判定

初期対応は**classic MBRの4個のprimary partitionだけ**とする。Extended partitionのEBR、GPT、
保護MBR、superfloppy、複数LUNは対象外である。

1. LBA 0の`55 AA`、各エントリの開始LBA・長さ・ディスク範囲を検査する。
2. 長さ0、範囲外、`u64`加算のoverflow、相互に重なるentryはmount候補から除外し、理由をログに残す。
3. 指定されたprimary partitionだけを`PartitionBlockDevice`にする。初期シェルは
   `mount sd0p1`または`mount usb1p2`のようにdeviceとentry番号を明示させ、自動で
   「最初のFATらしい領域」を選ばない。標準mount先はそれぞれ`/vol/sd0p1`、
   `/vol/usb1p2`から機械的に決める。
4. `55 AA`だけではMBRとFAT/exFAT superfloppyを確実に区別できない。MBR側はboot indicatorが
   `0x00`/`0x80`、少なくとも一つの非空entry、開始LBA≧1、容量内、非重複を必須にし、同時に
   LBA 0をFAT/exFAT boot sectorとしても検査する。MBRだけが妥当ならMBR、boot sectorだけが
   妥当なら`SuperfloppyUnsupported`、両方が妥当なら`AmbiguousLayout`として拒否する。
   crafted imageまで含めた完全な識別はできないため、曖昧な媒体をmountしないことで安全側に倒す。

これにより、MBRの種類byteはヒントにしか使わない。実際のFAT/exFAT判定は、切り出した
partitionのboot regionをファイルシステムドライバが検査して行う。

## FAT/exFATライブラリの選定ゲート（決定済み: `hadris-fat`）

**2026-08-24に`hadris-fat` 2.1.0を採用した。** 3候補を同じターゲット
（`riscv32imafc-unknown-none-elf`、rustc 1.98.0 stable）のrelease buildで比較した結果は
次のとおりである。

| 候補 | 結果 | 根拠 |
| --- | --- | --- |
| `hadris-fat` 2.1.0 | **採用** | `no_std`＋`alloc`でstable buildが通る。FAT12/16/32＋LFN、exFATは`unstable-exfat`のsync専用preview。`FatVolume<DATA>`がI/Oを所有するのでvolumeごとに独立し、global current volumeが無い。MIT |
| `fatfs_embedded` 0.1.0 | 不採用 | `FF_VOLUMES = 1`で単一volume固定。複数volume同時mountという必須条件を満たさない。加えて`riscv32-unknown-elf-gcc`を要求する（このリポジトリに他のCツールチェーン依存は無い）、`embassy-sync`のMutexに依存する |
| `fatfs`（rust-fatfs）0.3.6 | 不採用 | `no_std`経路が要求する`core_io`のbuild scriptがrustc 1.98を認識できず`Unknown compiler version`でpanicする。stableでbuildが成立しない。exFATも無い |

採用条件の充足状況:

- 複数volumeを同時に独立mount: **満たす**。`FatVolume::open(io)`がI/Oを所有し、mountごとに
  別インスタンスになる
- stable Rustでtargetのrelease buildが通る: **満たす**（`rust-version` 1.88、手元は1.98）
- 既存アロケータで上限を説明できる: **概ね満たす**。読み出し経路の`alloc`はLFN名の`String`、
  ディレクトリ走査の一時バッファ（クラスタサイズ）、`FileReader`のクラスタバッファとchain
  キャッシュである。上限はクラスタサイズとchain長に比例するので、mount時のクラスタサイズから
  説明できる
- 読み出しAPIに媒体世代の検査を差し込める: **満たす**。ライブラリはbyte単位の
  `hadris_io::{Read, Write, Seek}`しか見ないので、`PartitionBlockDevice`の上に置く
  adapter（sector cacheを持つ）が全I/Oの通り道になり、そこがgeneration検査とエラー変換の
  差し込み点になる。エラー型は`Error::Source(E)`で自前の型をそのまま運べる
- 壊れたimageをpanicせず拒否: **未検証**。ホストテストを保留したためである（上記「状態」）

`unstable-exfat`はAPI安定性の対象外で、sync専用、fragmentedなallocation bitmap／upcase、
directory growth、TexFATを扱わないと明記されている。Stage 5でこの範囲が実媒体に足りるかを
確認し、足りなければ読み出し専用の自前parserを再検討する。

read-only mountでの暗黙write抑止は、`write` featureを外す方法と、ブロック層の
`WriteSuppressed`で止める方法の二つがある。`/ram`の書き込み（Stage 4）に`write`が要るため
feature単位では分けられない。したがって物理媒体のwrite抑止は`sd.rs`／`usb_msc.rs`の
`write_blocks`が担い、ライブラリが暗黙writeを出した場合はコマンド発行前に
`WriteSuppressed`で失敗する。

### 実測したコードサイズ

`tools/check_elf_layout.py`のIROM／DROM値である。DROMはどの段階でも130,776 byteで変化しない。

| 段階 | IROM | 増分 |
| --- | --- | --- |
| 本計画着手前 | 414,268 | — |
| Stage 1（ブロック層、MBR判定、RAMディスク、format、`devices`／`blkread`） | 429,224 | +14,956 |
| ＋`hadris-fat`のmount・root列挙・ファイル読み出し（read/lfn/sync/alloc） | 441,716 | +12,492 |
| Stage 2（VFS、path、BlockStream、seed、`mount`／`ls`／`cat`ほか） | 480,056 | +50,832 |
| Stage 3（fingerprint、media generation、connection epoch、複数USB） | 495,778 | +15,722 |
| Stage 4（`write` feature、書き込み経路、RTC clock、`write`／`append`） | 521,616 | +25,838 |
| Stage 5（`unstable-exfat`、read-onlyのexFAT経路、`ls`のタイムスタンプ） | 543,764 | +22,148 |

`write`と`unstable-exfat`を有効にしても、呼び出していない間はLTOが落とすので増分は0だった。
実際の増分はStage 4とStage 5で測り直す。

Stage 2の増分はStage 1比で+50,832 byte（ライブラリ本体の+12,492を含む）。差分はVFS、
path正規化、`BlockStream`、seed、シェルコマンド6個である。

ヒープはRAMディスク予約により31,711,232 byteから23,322,624 byteへ減った。

## 実装着手時の決定ゲート

Stage 0とStage 1を始めるために、追加の利用者判断は必要ない。ライブラリ採用は事前に名前だけで
決めず、Stage 1のPoC結果を設計ゲートとする。どの候補も採用条件を満たさない場合だけStage 2へ
進まず、自前read-only parserまたは限定ラッパーのどちらへ進むかを再検討する。

初期のMBR/FAT/exFAT mount経路が受理する媒体の論理block長は**512 byteだけ**とする。
`BlockGeometry`とLBAの型は将来の異なるblock長を表せるままにするが、512 byte以外の媒体は
`UnsupportedBlockSize`としてpartition解析前に拒否する。現時点のSD/USB adapterが512 byteであり、
異なる論理block長におけるMBR LBA、filesystem sector、cacheの単位を推測で混在させないためである。
将来対応は実媒体と仕様を確認した別Stageで追加する。

ライブラリPoCでは単にroot directoryを読めるだけでなく、次を満たすadapter境界を確認する。

- mountごとに独立したfilesystem contextを作れ、単一のglobal current volumeを要求しない。
- 採用ライブラリのfile/directory handleをVFS公開型へ埋め込まず、pathとoffsetから再openできる。
- I/Oごとに`VolumeId`のgeneration確認と`BlockDevice`エラー変換を差し込める。
- read-only mountではformat時刻やアクセス時刻の更新を含め、暗黙のwriteを完全に止められる。

次の事項は実装を止める事前決定ではなく、該当Stageに入る時にテストとともに固定する。

- Stage 2開始時: path/component長、FAT名の大文字小文字比較、synthetic rootの列挙順、open handleの
  資源上限、明示`umount`時の`Busy`規則。
- Stage 3開始時: optionalなUSB serial/VPDを取得できない場合のfingerprint確度、再試行回数と
  `Suspended`のtimeout。物理edgeを優先する原則は変更しない。
- Stage 4開始時: create/exclusive/appendなどのopen mode、close/flush失敗の扱い、同一fileを複数
  handleから書く場合の制限。
- Stage 5開始時: exFAT固有のUnicode比較と検証上限。FATだけの都合を先にVFS公開仕様へ漏らさない。

## 書き込みと整合性の方針

- 今回のread-write mountは`/ram`だけとする。`/vol/sd0pN`と`/vol/usbNpM`は常にread-onlyで、
  mount optionによってread-writeへ変更する機能も実装しない。
- mount tableの`MountMode`と、ファイルシステムdriverをread-onlyで開く設定の両方で
  SD/USBへのwriteを拒否する。テスト用の記録adapterで、read-only操作中に`write_blocks`が
  一度も呼ばれないことをホストテストする。
- 初期のSD/USB adapterで`write_blocks`が呼ばれた場合は、LBA、block数、呼出し元のmount IDを
  診断ログとcounterへ記録し、媒体へcommandを出さず`WriteSuppressed`エラーを返す。成功を
  偽装すると上位層がmetadata更新済みと誤認するため、dry-runを成功扱いにはしない。
- FATではファイルデータ、FAT、ディレクトリエントリの更新順、flush、unmountを一つのFS driverに
  閉じる。このwrite経路はRAM disk imageとホストテスト用imageだけで使う。
- RAMディスクの`flush()`はFS metadataをRAM imageへ反映した時点で成功し、永続化は保証しない。
  write-back cacheを追加する場合もRAMの固定容量内だけとする。
- 将来SD/USB書き込みを追加するときも`BlockDevice`、`PartitionBlockDevice`、ファイルシステム
  driverのinterfaceは変更しない。媒体ごとのflush保証と障害時挙動を実装・受入した後、対象mountの
  policyをread-writeへ変更する。
- 将来の物理媒体read-write mountで、未flushのcacheまたは成否不明のwriteがある状態でtransportが
  失効した場合は、fingerprintが一致してもopen handleを自動復帰させない。`WriteStateUnknown`で
  強制unmountし、再mountと整合性確認を要求する。上記のgeneration維持/rebindは、今回の
  read-only物理mountまたはflush済みでcleanと確認できる状態だけに適用する。
- VFS自身は単一スレッドのforeground利用を前提にする。割り込みハンドラからVFSを呼ばず、
  1媒体につき一操作だけを進行させる。非同期化は、USB/SDの待ちがUIを止めると実測で判明してから行う。

## 将来課題: 本体Flash（今回のスコープ外）

SPI Flashは消去単位（通常4 KiB以上）と書き換え寿命を持つNOR媒体であり、512 byte更新を前提に
するFATを予約領域へ直接載せると、FATとディレクトリエントリが同じerase blockへ集中して摩耗する。
よって本体FlashにはFAT/exFATを使用せず、今回の実装Stageにも`/flash`を含めない。

将来対応では、wear levelingと電源断回復を持つFlash専用ファイルシステム
（LittleFS相当）を別計画で比較・選定する。選定時はerase回数、書き込み増幅、停電回復、
コード/RAM量を評価する。`partitions.csv`の`nvs`、`phy_init`、`factory`を動的に触らず、
`storage`をformatする操作は明示的な破壊的コマンドにする。バックアップ手順、format識別子、
`/flash`への統合方法もその別計画で定める。

raw SPI NORの「block size」は一つではなく、読み出し単位、page program単位、erase sector/block
単位が異なる。erase単位はSD/USBの512 byte論理sectorより大きい。Flash専用FSでは
`BlockDevice`の論理blockへ無理に合わせず、`read`、`program`、`erase`と各geometryを持つ
専用interfaceを検討する。実際の単位は対象Flashの仕様とドライバ実装時に確定する。

## 追加で固定すべき仕様

- パスはUTF-8入力とし、`/`、空要素、`.`、`..`、最大長をVFSで正規化・検査する。FATのLFNを
  読み書きし、exFATのUTF-16名も扱う。表示不能文字はescapeしてログへ残す規則を決める。
- RTCは既にあるため、書き込みを始める前にFAT timestampへ渡す時刻、timezone、RTC未設定時の
  timestampを決める。RTC未設定時はon-disk timestamp fieldを0にする。これはUnix epochではなく
  FAT上の未設定値として扱い、ライブラリが0を受け付けない場合はtimestamp更新を行わない。
- FAT/exFAT boot record、FSInfo、cluster chain、directory entryはすべて未信頼入力である。範囲、
  reserved value、循環chain、ファイルサイズとchain長、exFAT boot checksumを検証し、panicや
  無限ループにしない。
- 取り外し、SDの応答喪失、USBのUNIT ATTENTION/No Medium、USB session失効を共通エラーへ
  変換する。物理edgeまたは媒体不一致は`MediaRemoved`/`MediaChanged`、同一性確認待ちは
  `TemporarilyUnavailable`と区別する。mount tableには媒体固有のgenerationを記録し、open時と
  各I/O完了後に照合する。
- 既存のUSB初回利用可能化予算（connect 1,000 ms、MSC ready 4,000 ms）は、起動時の自動mountを
  導入するときだけ適用する。通常の`mount usbNpM`は同じ上限と失敗理由を表示し、SDへの
  暗黙フォールバックはしない。

初期VFS APIは`mount`、`umount`、`open`、`read`、`write`（`/ram`のみ）、`seek`、`close`、
directory iteratorに限定する。directory iteratorは`ls`に必要な名前、file/directory種別、
サイズを返す。rename、remove、mkdir、truncate、カレントディレクトリ、相対パスは初期範囲外とし、
採用ライブラリ固有のhandleやlifetimeをVFS公開APIへ出さない。

## テスト環境の分担

壊れたファイルシステムをSDカードやUSBメモリに用意する実機試験は行わない。破損データの
作成・再現・後始末が難しく、誤って他の媒体を壊す危険もあるためである。

- MBR/BPB、FAT、cluster chain、directory entry、exFAT boot region/checksumなどの破損試験は、
  ローカルで生成したdisk imageをホストの`BlockDevice`実装から読み込ませる。
- 正常imageをformatツールで生成し、検査対象byteだけを決定的に書き換えるスクリプトまたは
  テストfixtureを用意する。期待するエラーと変更位置をテスト内に記録する。
- block write途中の失敗は、N回目のwrite/flushを意図的に失敗させるホスト用fault-injection
  `BlockDevice`で再現する。実機の電源を意図的に切って破損媒体を作る試験は行わない。
- 実機の物理媒体試験は、PCで正常にformat・検査したSD/USBのmount、列挙、読み出し、抜き差し、
  通常のreadエラーに限定する。書き込みの実機試験はRAMディスクだけで行い、破損imageの拒否は
  ホストテストの完了条件とする。

## 段階分けと完了条件

### Stage 0: 仕様の固定とテスト素材

- mount名、MBR primaryのみ、superfloppy/ambiguous layout拒否、明示mount、物理媒体read-only、
  エラー表示をこの計画どおりに固定する。
- FAT12/16/32、FAT32 LFN、exFATの正常imageと、壊れたMBR/BPB/cluster chainのimageを
  ローカルに生成する。
- `fsck`相当のホスト検査とSHA-256を記録し、各imageが何を検証するかをテスト名に残す。

完了条件: 破損・境界ケースを含むimage一覧と、期待するmount/read失敗がホストテストで再現できる。
破損imageを実媒体へ書き込む作業は含めない。

### Stage 1: `BlockDevice`、MBR、ライブラリPoC

- `BlockGeometry`、`BlockDevice`、`PartitionBlockDevice`、MBRの検査・列挙を`no_std`で実装する。
- SD、USB、8 MiB RAM disk、ホストimageを同じ`BlockDevice` interfaceで接続する。
  `write_blocks`と`flush`をtraitへ最初から含めるが、このStageのSD/USB受入はreadだけとする。
- `Psram::ram_disk()`を追加し、`Psram::heap()`から固定8 MiBを除外する。起動時にFAT16でformatする。
- 上記候補のFAT/exFATライブラリをhost imageとターゲットrelease buildで比較し、一つを採用する。

完了条件: ホスト上のRAMまたはファイルに置いた既知imageをMBRから切り出し、FAT32とexFATの
mount成功/失敗を期待どおり判定し、releaseのサイズ増分と最大heap使用量を記録する。

### Stage 2: read-only FATと最小VFS ✅ 完了（2026-08-24、実機確認済み）

実装した内容と、開始時に固定した決定は次のとおり。現状仕様は
[`FILESYSTEM.md`](FILESYSTEM.md)を優先する。

- 採用した`hadris-fat`でFAT12/16/32のmount、root/list、open/read/seekを実装した。
  `write` featureは外してあるので書き込み経路はリンクされていない。
- `BlockStream`（`src/fs/stream.rs`）がbyte単位`Read`/`Seek`とmountあたり4 KiBの
  セクタキャッシュを提供する。キャッシュは散らばったブロック集合ではなく連続区間を
  持つ。クラスタチェーンもファイルデータも前方へ進むためである。
- mountはFAT volumeを保持せず、操作のたびに開き直す。file handleは`VolumeId`・
  volume内path・offsetだけを持つ。この形にした理由は[`FILESYSTEM.md`](FILESYSTEM.md)にある。
- `/ram`は起動ごとにformatし、短名・LFN・複数クラスタの3ファイルを書き込む
  （`src/fs/seed.rs`）。ライブラリを通さず直接書くので、`write` featureを外したまま
  読み出しを検証できる。
- Stage 2完了条件の「正常な実媒体で読む」を満たすため、`mount sd0pN`／`mount usb0pN`も
  この段階で実装した。ホストテストを保留した以上、実媒体が唯一の検証手段になるためである。
  複数USB、媒体世代の追跡、抜き差し復帰はStage 3のまま。

実機で確認した内容: `/ram`のマウント、`ls /`と`ls /ram`、LFN名（`Hello World.txt`）の
表示とパス解決、`cat`の短名・LFN・複数クラスタ読み出しとサイズ照合、オフセット指定、
実媒体（SD／USB）のマウントと読み出し、`fswrite`によるread-only拒否、`umount`、
異常系（相対パス、存在しないファイル、ファイルをディレクトリとして辿る、`..`の正規化）。

実装中に見つかった問題: ファイル名に空白が入るとシェルが引数を途中で切っていた。
当初`cat`のoffsetを`:`区切りにして回避したが、引数を2つ取るコマンドへ広がらないため、
シェルにダブルクォートを実装して解決した（[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)）。

開始時に固定した決定:

| 項目 | 決定 |
| --- | --- |
| path/component長 | どちらも255 byte。FATとexFATで上限が違うので、媒体によって受理されるpathが変わらないようVFS側で固定する |
| FAT名の大文字小文字比較 | ASCIIのみ畳む。それ以外はcodepageとexFATのup-caseテーブル次第で、推測するとvolume自身の答えと食い違う |
| synthetic rootの列挙順 | mount table のslot順（＝mountした順） |
| open handleの資源上限 | 4。1つのhandleは「そのvolumeをumountできない」という約束でもあるため小さく取る |
| 明示`umount`時の`Busy`規則 | そのvolumeにopen handleが1つでもあれば拒否する |
| 相対パス | 受け付けない。カレントディレクトリが無いので基準が無い |
| ルートを越える`..` | `EscapesRoot`で拒否する。rootで止めない |
| 空白を含むパス | シェルにダブルクォートを実装して対応する。当初は`cat`のoffsetを`:`で区切って回避したが、引数を2つ取るコマンドへ広がらないため取りやめた（[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)） |

### Stage 2の元の記述

- 選定したdriverでFAT12/16/32のmount、root/list、open/read/seekを実装する。
- `/ram`へFAT imageをmountし、`ls /ram`、`cat /ram/README.TXT`相当のread-only shellを追加する。
- mount table、絶対パス解決、物理媒体のread-only拒否、媒体generationつきファイルハンドル、
  `ls`用directory iteratorを実装する。mountはすべて明示操作とする。

完了条件: ホストテストと正常な実媒体で短名・LFN・cluster chainを読む。破損chainの拒否は
ホストだけで確認する。書き込み系操作はStage 4まで全mountで拒否される。

### Stage 3: 複数SD partitionと複数USBのread-only mount ✅ 完了（2026-08-24、実機確認済み）

実装済み（2026-08-24、実機確認済み）:

- `devices`／`mount`／`umount`／`mounts`。`devices`はUSBストレージを接続台数ぶん列挙し、
  接続位置とVID:PIDも出す。MBRの複数の有効なprimary entryはそれぞれ独立したmount候補になる。
- `UsbHost`に`mass_storage_at`／`mass_storage_inventory`／`mass_storage_location`を追加し、
  ファイルシステム層は`mass_storage_mut()`（最初の1台）を使わなくなった。
- fingerprint（`src/fs/fingerprint.rs`）: SD CID、SCSI INQUIRY、VPD `0x80`／`0x83`、
  MBR disk signatureとpartition表、partitionのboot sector。SCSIのVPD読み出しは
  `usb/msc.rs`へ追加した。`mounts`がdigestと**どの情報源が答えたか**を表示する。
- media generationと`fsverify`。変化していればmountを外し、開いているhandleは以後失敗する。
  デバイス不在・identity取得失敗はmountを残す（どちらも「別媒体である」ことを示さないため）。
- 物理的なconnection changeの観測。`UsbHost::connection_epoch_at`が**ポート単位**で
  進む。mountがこれを記録し、**identityより優先して**判定する。操作のたびに検査する。
  当初バス全体で1つのカウンタにしていたが、2本挿しの実機確認で「ハブから1本抜くと
  全マウントが落ちる」と分かったのでポート単位へ変えた。rootの事象はハブごとバス全部を
  持ち去るので別に数えて全ポートに効かせる。
- バス上の位置の記録と照合。`usbM`の番号は他のデバイスを抜くと繰り上がるので、
  同容量の別デバイスへ番号が向くのを位置で捕まえる。バス通信が無いので操作のたびに検査できる。

Stage 3の作業中に、**既存の穴**が1つ見つかって直した。ハブポートからMass Storageを
抜いても誰も観測せず、スロットが占有されたままポートが死んでいた。`needs_reinit`が
Mass Storageを意図的に除外している（[`STORAGE.md`](STORAGE.md)）ためで、
`detach_disconnected_hub_ports`による定期掃引を追加した（[`USB.md`](USB.md)）。

- `rescan()`へのreason（`Manual`／`Recovery`／`PowerRecovery`／`PhysicalConnectionChange`）。
  rescanは開始時に未処理のedgeを先に採取し、あれば呼出元の理由より優先する
  （[`USB.md`](USB.md)）。以前は自分のリセットが生むedgeと一緒に捨てていたため、
  抜き挿しが再スキャン直前に起きると痕跡が残らなかった。

**計画から変えた点: `Suspended`を状態として持たない。**

計画は「rescan前にfingerprint・generation・mount・open handleをsnapshotし、対象mountを
`Suspended`にする。`Suspended`中のI/Oはhandleを破棄せず`TemporarilyUnavailable`を返す」
としていた。実装は同じ保証を、**保存して復元するのではなく、そもそも壊さない**ことで
得ている。

- mountはFAT volumeもライブラリのhandleも持たないので、rescanがバスを作り直しても
  mount tableは何も失わない。snapshotして復元する対象が存在しない。
- 転送が失敗したセッションは`UsbMscBlockDevice`が`TemporarilyUnavailable`を返し、
  handleは破棄されない。`usbrescan`でセッションが戻れば同じhandleから読み出しを再開できる。
  これは`Suspended`中のI/Oに求められた挙動そのものである。
- 「別媒体になった」「持ち去られた」の判定はgenerationとconnection epochが担う。

したがって`mounts`に`Suspended`という表示は無く、到達できないmountは操作したときに
`temporarily unavailable`を返す。状態として持たせると、mount tableに「今バスがどうなって
いるか」の写しを持つことになり、実際のバスと食い違い得る二つ目の真実を作ることになる。

未実装:

（なし）

複数USB Mass Storageの同時mountも実機確認済み（2026-08-24）。複数SD partitionの
同時mountも検証済み。

### Stage 3の元の記述

- `devices`、`mount sd0pN`、`mount usbNpM`、`umount`、`mounts`を追加する。
- MBRに複数の有効なprimary entryがある場合は、それぞれを独立したmount候補として表示する。
- `UsbHost`へ全MSCの列挙とslot指定accessorを追加し、複数USB MSCを同時にmountできるようにする。
- SDの応答喪失とUSBの抜き差し/session失効で、開いているfileを含め安全に失敗・復帰することを
  検証する。同一fingerprintのrecovery rescanではgenerationとlogical handleを維持し、物理edge後は
  同じデバイスでも旧handleを無効化する。不一致時とfingerprint未確定時も別々に検証する。
- USBのmountは既存のready/first-read処理を用い、失敗理由と待ち時間を出す。

完了条件: 複数partitionを持つFAT32カードと、ハブに挿した複数USBメモリを同時にmountして
各パスから別々のファイルを読み出す。途中取り外し後にクラッシュ、無限待ち、別媒体への
取り違えがない。転送障害だけのrescan後は同じlogical handleから読み出しを再開できる。
物理媒体へwrite commandを発行しない。

### Stage 4: RAMディスクだけのFAT書き込み ✅ 完了（2026-08-25、実機確認済み）

`hadris-fat`の`write` featureを有効にし、`/ram`でopen/create、write、flush、closeを
実装した。`write <path> <text>`／`append <path> <text>`で実機から叩ける。
現状仕様は[`FILESYSTEM.md`](FILESYSTEM.md)を優先する。

開始時に固定した決定:

| 項目 | 決定 |
| --- | --- |
| open mode | `Read`／`Truncate`／`Append`の3つだけ。exclusive createと「途中を上書き」は提供しない。ライブラリのwriterは先頭か末尾から前へ進むだけでシークできないので、下の層が守れない約束になるため |
| 書き込める位置 | 置き換えるために開いたファイルの先頭と、末尾のみ。それ以外は`NotSeekable`。読み直して書き直す模倣はしない（失敗の仕方が違う別の操作） |
| close/flushの失敗 | `finish`（ディレクトリエントリへのサイズ・時刻の確定）と`sync`の失敗は**書き込みの失敗**として扱う。バイトが媒体に届いていてもエントリが指していなければ、その書き込みは無かったのと同じであるため |
| 同一fileを複数handleから書く | 妨げない。VFSはhandleごとに独立したoffsetを持ち、書き込みのたびにvolumeを開き直すので、2つのhandleは互いの結果を上書きし合う。単一threadのforeground利用なので競合状態にはならないが、結果は最後に書いた方になる |
| timestamp | RTCから、ローカル時刻のまま。FATにoffsetを記録する場所が無く、PCが同じカードへ書いた値と食い違わせないため。**操作ごとに1回**サンプルする |
| RTC未設定時 | date/timeに0を書く。FATの「タイムスタンプ無し」であって、1980-01-01ではない |

実機で確認した内容: `write`によるファイル作成・置き換え、`append`による追記、
空白入りの長い名前、`ls`のタイムスタンプ表示、SD／USBへの書き込みが`open`の時点で
拒否されブロック層まで届かないこと。

RTCが読めない場合の`(no time)`経路は**未検証**である（一度設定すると再現できないため）。

**ホストのfault-injection試験は実施しない。** ホストテストを別タスクへ保留した
利用者判断（本文書「状態」）による。したがってwrite/flush境界の中断シミュレーションと、
その結果の再mount時挙動は未検証である。

既知の制限: `Truncate`で既存ファイルを短い内容に置き換えると、末尾より後ろのクラスタが
解放されない可能性がある。破損ではないが容量は減る。`/ram`は起動ごとに作り直すので
影響は1セッション内に限られる。

### Stage 4の元の記述

- `/ram`でopen/create、read/write/seek/closeとflush/unmountを検証する。rename、remove、mkdir、
  truncateは初期VFSの範囲外とする。
- ホストのfault-injection `BlockDevice`で各write/flush境界の失敗を再現し、再mount時の挙動を
  確認する。壊れた結果はdisk imageとして保持し、実媒体には書かない。

完了条件: RAM上で全操作を反復して内容が一致し、ホストの中断シミュレーションがpanic・
無限ループしない。SD/USBにも同じblock write APIが存在するが、read-only mountからは
一度も呼ばれない。

### Stage 5: exFAT（実装済み、完了条件の一部が未検証）

`unstable-exfat` featureを有効にし、**読み出し専用**で追加した。`mounts`が
ボリュームごとに`FAT`／`exFAT`を表示する。現状仕様は[`FILESYSTEM.md`](FILESYSTEM.md)を優先する。

**exFAT書き込みは実装しない。** 計画では「実装する場合はRAMディスクとホストimageだけに
限定」としていたが、実装しない側を選んだ。理由は2つある。採用ライブラリのexFATは
明示的に不安定なプレビューで、作者自身がかけがえのないデータへの使用を推奨していない。
そしてこのファームウェアが到達できるexFATボリュームはSDとUSBだけで、どちらにせよ方針上
読み取り専用である。`/ram`は自前フォーマッタのFAT16なので、書き込み経路を足しても
書く先が無い。

開始時に固定した決定:

| 項目 | 決定 |
| --- | --- |
| Unicode比較 | exFATのパス解決と名前比較はライブラリに任せる。upcaseテーブルはボリュームが持つものが正であり、こちらでASCII比較を被せると volume 自身の答えと食い違う |
| UTC offset | 無視する。exFATは記録できるがFATには場所が無く、こちらはFATへローカル時刻を書いている。exFAT側だけ変換すると同じPCが書いた2枚のカードが一覧上で食い違う |
| サイズ | `valid_data_length`（実データ長）。`data_length`（確保済み長）ではない |
| 検証上限 | ライブラリのものをそのまま使う。cluster loopの検出はライブラリ側にあり（`ClusterLoop`）、こちらで二重に上限を設けない |
| 型の分岐 | `FatVolume`と`ExFatVolume`をenumで持ち、`ls`／`open`／`read`／`write`の4箇所で分岐する。共通traitに包まない |

ライブラリ側のプレビュー制限のうち**読み出しに影響し得る**もの: 断片化したupcase
テーブル、クラスタ境界をまたぐentry set。実媒体で踏めば該当ファイルの読み出しが失敗する。

実機で確認した内容: 実媒体のexFATパーティションのmount、`mounts`での形式表示、`ls`の
一覧・サイズ・更新日時、書き込みの拒否、FAT媒体と`/ram`の回帰。

**完了条件のうち未検証:**

- **Unicode名の読み出し**（2026-08-25、確認手段が用意できず）。上記のプレビュー制限
  （断片化upcase、クラスタ境界をまたぐentry set）が実際に問題になるとすればここなので、
  この経路は最も確認したい部分が確認できていない。
- **大きな連続ファイルの読み出し**（同上）。
- 壊れたboot regionの安全な拒否（ホストテスト保留のため）。

したがってStage 5は「実装して基本的な読み出しは動く」ところまでで、計画が完了条件に
挙げた2項目は満たしていない。

### Stage 5の元の記述

- exFAT boot region/checksum、allocation bitmap、upcase table、directory setをライブラリに任せる場合も、
  その検証範囲とエラーを確認する。
- read-onlyを正常なSD/USBで先に受け入れる。exFAT書き込みを実装する場合はRAMディスクと
  ホストimageだけに限定し、FAT Stage 4と同じホスト中断シミュレーションを行う。

完了条件: 正常な実カードのexFAT媒体で大きな連続ファイルとUnicode名を読む。壊れたboot regionの
安全な拒否は、ローカル生成したimageを使うホストテストで確認する。SD/USBはread-onlyを維持する。

## 実装開始時に更新する現状文書

実装が入った段階で、[`STORAGE.md`](STORAGE.md)に実際の対応媒体、mount規則、コマンド、
read/write保証、未対応形式を追記する。モジュールを追加したら[`FILE_LAYOUT.md`](FILE_LAYOUT.md)も
同じ変更で更新する。本体Flashの将来計画を作成した場合は[`BOOT.md`](BOOT.md)の
Flashパーティション節との整合もその作業で確認する。
