# 外部FAT書き込みリファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> 現状のファイルシステム: [`FILESYSTEM.md`](FILESYSTEM.md)、
> ブロックI/O: [`STORAGE.md`](STORAGE.md)、
> 元の段階計画: [`FILESYSTEM_PLAN.md`](FILESYSTEM_PLAN.md)、
> USB書き込みの実機記録: [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)

## 状態

**Stage 0〜5を実装済み。RAMディスク、SD、USBの1-round実機受入は合格。USBの頻発する
transport recoveryは継続調査中。**

RAMディスクだけに限定していたFAT書き込み経路を、SDカードとUSB Mass Storage上の
FAT12/16/32へ広げる計画である。実装の現状は[`FILESYSTEM.md`](FILESYSTEM.md)と
[`STORAGE.md`](STORAGE.md)にある。

静的確認はすべて通過している——`cargo build --release`、変更ファイルの`rustfmt`、
`clippy`（新規警告なし）、ホストテスト、ELF配置検査、`git diff --check`。`README.md`は
未変更（不一致箇所は下記）。

### 実機受入の到達点

受入は`fswritetest <dir> [rounds] [KiB]`が実行する（[`FILESYSTEM.md`](FILESYSTEM.md)の
「書き込み経路の受入試験」）。

| 媒体 | 結果 |
| --- | --- |
| RAMディスク（`/tmp`） | **合格** |
| SDカード | **合格** |
| USB Mass Storage | **12検査PASS**（1 round、45,383 ms）。途中のtransport recoveryは多い（下記） |

USBの検査2は64 KiBを1ブロックずつ、**WRITE(10) 128本の連続成功**である。検査3
（短縮置換）まで通っているので、**Stage 0のtruncate修正はUSB上でも検証済み**。

コマンドで代替できない受入項目が2つ残っている。PCでの`fsck.fat -n`と、再起動を
挟んだ読み直しである。SDについては未実施。

### 中断理由

1回目、短縮ログ版の3回目、opcode追加後の4回目はUSBの検査4で
`bulk IN timed out during CSW`。明示的な
`usbrescan`直後の2回目は検査4を通過し、検査5の64 KiB書き込み中のREAD data IN失敗から
Reset Recoveryも完了できず中断した。3回目も検査2中のREAD(10)（LBA `0x7DF0`、8 blocks、
READ再同期1回、WRITE再同期188回の時点）でdata INが一度無応答になったが、Reset Recovery後の
再送で復帰している。opcode追加後の4回目で、検査2のREAD(10)（LBA `0x7E20`、8 blocks、
tag `0x14F`）も同様に復帰した。検査4の失敗はopcode `0x00`、tag `0x9E4`、data 0、IN方向で、
検査先頭の媒体確認が最初に発行するTEST UNIT READYと確定した。
いずれもポートは`connected enabled powered`のままで、過電流・デバイス脱落、cache
writeback拒否の記録はなく、明示的な再列挙で再attachできた。したがって固定の検査やLBAに
依存するファイルシステム不具合より、コマンド累積後にBOT sessionが不調になる症状を示す。

3回目は`usbrescan`前に`usbhw`を採取した。`submit=25722`、`reap=25715`、`cancel=7`で
収支が一致し、stale token、pending channel、unknown IRQ、port event、cache writeback拒否は
すべて0だった。Mass StorageとHID 2台もregistryに残っている。controllerのslot取りこぼしや
物理切断ではなく、接続中のMSC device／BOT sessionだけが応答しなくなる証拠が強まった。
TEST UNIT READYはdataも媒体変更も伴わないため、READ(10)と同様にReset Recovery成功後の
1回再送を追加した。回復済みsessionを試さず`DeviceNotPresent`へ変換してmountを落としていた
直接の失敗経路はこれで塞いだ。5回目は検査4と検査8のTEST UNIT READY障害を再送で復帰し、
途中4回のREAD(10)障害も既存の再送で復帰して、検査9まで通過した。検査10先頭の媒体確認では
READ CAPACITY(10)（opcode `0x25`、tag `0x1597`、8 bytes）のdata INがtimeoutし、Reset
Recovery成功後に再送せずmountを落とした。同じ問題が次のINQUIRYへ移るのを防ぐため、
TEST UNIT READY、READ CAPACITY(10)、INQUIRY／INQUIRY(EVPD)を再送安全な照会として共通化し、
いずれもRecovery後に1回だけ再送する。WRITE(10)を再送しない方針は変更しない。
この修正版で`fswritetest /vol/usb0p1 1 1`は12検査を45,383 msで完走した。

ただし回復の頻度は正常なUSB MSCとして受け入れる水準ではない。失敗はREAD(10)のdata IN、
TEST UNIT READYのCSW、READ CAPACITY(10)のdata INと、媒体上の特定sectorでは説明できない
commandにまたがる。別runでも最初のREAD障害は同じtag `0x14F`、検査4のTEST UNIT READYは
同じtag `0x9E4`で再現した一方、READのLBAは`0x7DF0`、`0x7E20`、`0x7E50`と動いた。
不良flash cellより、command列に依存してBulk INのhost／device状態がずれ、Reset Recoveryの
DATA0復帰で戻る症状に整合する。controllerのslot収支、port event、cache拒否は正常なので、
controller silicon単独よりMSC device firmwareと独自BOT/HCD実装の相互運用を第一候補とする。

**これは[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)が第1版から追って
根本原因未特定のまま残している症状であり、本計画の変更が持ち込んだものではない。**
続きはあちらの調査として扱うのが適切である。

なお**Stage 4の受入条件のうち「失敗時にWRITEを自動再送せず、MSC sessionと該当mountだけが
使用不能になる」は実機で満たされている**。満たされていないのは成功経路のほうである。

### この作業で見つかった不具合

4件見つかり、**3件は本計画の変更とは無関係な既存の問題**だった。`fswritetest`が
バイトを照合するようにしたことで表に出たものである。

| # | 内容 | 出所 | 対処 |
| --- | --- | --- | --- |
| 1 | `hadris-fat` 2.1.0の`FileWriter::new_append`が、ファイル長がクラスタサイズの整数倍のとき最終クラスタを上書きする | 依存ライブラリ（既存） | 2.2.0へ更新。`Cargo.toml`に下限である理由を記載 |
| 2 | VPDページ`0x80`の要求に標準INQUIRYを返すデバイスを検出できず、シリアル番号として指紋へ混ぜていた | 自コード（既存） | ページ`0x00`の一覧に載るページだけ要求する |
| 3 | 2 GiBを超えるFAT16が`hadris-fat`のクラスタ上限（32 KiB）でマウントできない | 依存ライブラリの制限 | 媒体をFAT32へ。拒否理由をUARTへ出すようにした |
| 4 | 複数ブロックのWRITE(10)でUSB転送層が戻らない | 転送層（本計画が初めて発行した形） | `MAX_WRITE_BLOCKS = 1`で回避 |

いずれも詳細は[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)にある。4番は
[`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)にとって**初の決定論的な
再現手順**なので、あちらにも記録した。

否定した仮説も残す。**WRITE(10)のOUT経路でキャッシュ書き戻しが拒否されている**という
説は外れだった。`usbhw`が拒否回数を表示するようにしてあり、実機で0である。同プランの
未解明候補3（OUT側のcache同期）のうち、この部分は消えた。

### 再開するときの入口

1. `fswritetest /vol/usb0pN 1 1`を実行し、検査4または5でREAD data IN／CSWが失敗するか。
   成功した予防再同期ログは初回と64回ごとだけ出し、失敗時はLBA、block数、FUA、方向別の
   累計再同期回数を出すため、その失敗周辺を採取する。
2. 複数のUSBメモリで試す。この作業では複数の媒体を使っており、**個体差の大きい
   デバイス群**である（VPDに標準INQUIRYを返す、SYNCHRONIZE CACHEを無効なsenseで
   失敗させる、といった規格違反を個体ごとに観測した）。別個体で通れば切り分けになる。
3. [`USB_WRITE_STABILITY_PLAN.md`](USB_WRITE_STABILITY_PLAN.md)の
   「根本原因を追う場合の次の手」。候補1（bulk転送のtimeout予算）は5秒が既に長く、
   同じ予算でREADの`ut 100`が100/100通ることから見込みは薄い。候補2（OUT側data
   toggle）と候補3の残り（OUT側staging）は、4番の決定論的再現手順で直接試せる。

### `README.md`の不一致

人間管理のため未変更。3箇所が実装と合わなくなっており、いずれも**記述を削って
`docs/`へ委ねる**のを第一候補として提案する。

- ファイルシステム行「FAT12/16/32とexFATの**読み出し**」——書き込みも含むようになった
- ファイル操作行「書けるのは`/tmp`だけ」——誤り。この但し書きごと削るのが簡単
- 未対応「ファイルシステム経由でのSDカード・USBメモリへの書き込み」——項目ごと削除。
  代わりに「電断に対する原子性・自動修復」を挙げるなら1行で足りる

既存の`BlockDevice`、`PartitionBlockDevice`、`BlockStream`、FAT VFS APIは維持する。
外部媒体を読み取り専用にしている二重の制限を単に外すのではなく、通常操作の整合性、
媒体ごとの書き込み完了境界、形式に基づく既定mount policyを足す。

## 信頼性の境界

FATを採用している時点で、ジャーナリングファイルシステムと同じ電断耐性は求めない。
本計画が保証するのは、**エラーを返さず完了した通常操作を、その後も同じ内容として
読み出せること**である。

| 扱い | 事象 |
| --- | --- |
| 許容しない | 成功した短縮置換の後に追記すると古い内容が読める、パーティション外へ書く、失敗したblock writeを成功扱いする、成否不明のWRITEを自動再送する |
| 許容する | 書き込み途中の電断・物理抜去で部分ファイルや未回収クラスタが残る、PC側の`fsck`が必要になる、既存ファイルの置換が原子的でない |
| 実装しない | journal、copy-on-write、起動時の自動修復、全write/flush境界のfault injection、意図的な電断試験 |

書き込み途中のI/O失敗は成功へ丸めない。WRITEが一部だけ媒体へ届いた可能性があるため
再送せず、その操作を失敗として返す。下位I/O失敗後のマウントは落とし、利用者が明示的に
再マウントする。専用のjournalや`WriteStateUnknown`からの自動復旧状態機械は持たない。

## 対象範囲

- SDカードとUSB Mass Storage上のFAT12/16/32
- `write`、`append`、`write_stream`、`mkdir`、`rm`、`rmdir`、`mv`
- FAT16 RAMルートの回帰

対象外:

- **exFAT書き込み**。採用ライブラリのexFATは不安定なpreviewであり、従来どおり
  形式として読み取り専用にする
- GPT、extended partition、512 byte以外の論理ブロック
- 本体SPI Flash。FATではなくwear levelingを持つ専用FSの別計画とする
- 複数処理からの同時書き込み。VFSは従来どおり単一threadのforeground利用とする

## マウント方針

FAT12/16/32は手動mountと自動mountのどちらも既定でread-writeにする。exFATは形式として
常にread-onlyにする。FATを意図的に読み取り専用で使うための`-r`だけを用意する。

```text
mount sd0p1             # FATならread-write、exFATならread-only
mount -r sd0p1          # FATも強制的にread-only
mount usb0p1            # FATならread-write、exFATならread-only
mount -r usb0p1         # FATも強制的にread-only
```

- USB自動マウントもFATならread-write、exFATならread-onlyにする
- 自動mountしただけでは媒体へ書かない。`write`、download、mkdir、remove等の変更操作を
  実行したときだけblock writeが発生する
- exFATをread-writeへ変更するoptionは持たない
- `mounts`は従来の`ro`／`rw`表示を使う
- RAMルートは従来どおり常時read-writeで、`mount -r ram`という別経路は作らない
- `files::attach`からVFSへ渡す要求は`Default`／`ReadOnly`の2つとする。VFSがboot sectorで
  形式を判定した後、`Default + FAT`を`ReadWrite`、`Default + exFAT`と`ReadOnly`を
  `ReadOnly`の`MountMode`へ確定する

## Stage 0: `Truncate`と`Append`の通常整合性

現行の`OpenMode::Truncate`は、最初の書き込みで`FileWriter::new`を選ぶだけで、ライブラリの
`FatVolumeWriteExt::truncate`を呼ばない。既存ファイルを短く置き換えてもFAT chainの末尾が
残り、続く`Append`はchain末尾へ書く一方、readerはファイルサイズに対応する先頭側を読む。
これは容量上のごみではなく、成功した通常操作の内容不整合なので外部書き込みより先に直す。

- `Vfs::write`の`Truncate`最初の呼び出しで、既存entryを`truncate(entry, 0)`する
- truncate後の`FileEntry`は先頭clusterが古いsnapshotなので再利用しない。パスからentryを
  引き直してから`FileWriter::new`を作る
- 空bufferの最初の`write`もtruncateを実行する。単に`open(Truncate)`して`close`しただけでは
  現状どおり媒体を変更しない
- `Vfs::write_stream(Truncate)`はbodyを呼ぶ前にtruncateし、entryを引き直す
- 操作末尾の`finish`と`volume.sync`は従来どおり必須とする

RAMディスクで次を確認する。

1. 複数clusterのファイルを1 cluster未満へ置き換える
2. 直後に追記し、置換後の内容と追記内容が連続して読める
3. 削除後に同容量を書き直せ、余分なclusterが恒久的に予約されていない
4. 同じ手順を`write`経路と`write_stream`経路の両方で行う

## Stage 1: ブロック書き込みadapterの共通条件

`src/fs/sd.rs`と`src/fs/usb_msc.rs`の`write_blocks`を、抑止ログだけの実装から実転送へ
置き換える。共通条件は次とする。

- `check_range`を転送前に通し、パーティション相対LBAを
  `PartitionBlockDevice`だけで物理LBAへ変換する
- 長い転送は媒体ごとの実機受入済み上限へ分割する
- 下位driverが書き込みbufferを`&mut [u8]`で要求する箇所は、immutableな
  `BlockDevice::write_blocks(&[u8])`からunsafe castしない。整列済みstaging bufferへcopyするか、
  OUT転送側のAPIを`&[u8]`へ分ける
- 途中まで転送した後の失敗も`Err`とし、部分成功を返さない
- 書き込み失敗を自動再送しない
- 読み取り専用mountからblock writeへ到達した場合を検出できるよう、`MountMode`の拒否は残す。
  adapter側の常時`WriteSuppressed`は、Stage 3または4の媒体受入後に外す

下位I/OエラーがFATライブラリを通った後も通常の`NotAFilesystem`へ潰れないよう、
`FatError::Io`／`IoContext`をVFSのI/O失敗へ写す。容量不足、名前、種別のエラーとは区別する。

## Stage 2: 形式に基づく既定mount mode

媒体adapterを有効にする前に、形式から既定modeを決める入口とpolicyの渡し方を固定する。

- shellの`mount`解析を0個、1個、`-r`付きの3形へ広げる
- `files::attach`へ`Default`／`ReadOnly`の要求を渡し、RAMかどうかからmodeを暗黙決定しない
- automountは既存のattach経路へ`Default`を渡す
- VFSが識別したFATは既定`ReadWrite`、exFATは常に`ReadOnly`へ確定する
- FATでないvolume、既にmount済みのvolume、存在しないpartitionの既存エラーを維持する
- 外部read-write mountの変更操作では、最初のblock writeより前にマウント時のfingerprintと
  現在の媒体を照合する。同容量のSD差し替えやUSBカードリーダー内の媒体交換を、容量一致だけで
  同じ媒体と扱わない。不一致または同一性を確認できない場合は何も書かずmountを落とす
- 書き込み操作中の下位I/O失敗では、そのmountをtableから外して全handleをstaleにする。
  `NoSpace`や`AlreadyExists`など媒体転送ではない失敗ではmountを維持する。RAMルートは
  この外部媒体向けunmount policyの対象にしない
- FATの既定read-write化は、媒体ごとにadapterの受入Stageを終えたbuildで有効にする。
  Stage 3完了・Stage 4未完了の中間状態ではSDだけを既定read-writeとし、USBは現在の
  read-onlyを維持する。最終仕様でUSB FATも既定read-writeへ切り替える

Stage 2と最初の媒体を扱うStage 3は同じbuildで実装してよい。分ける目的は、マウントpolicyと
媒体転送の不具合を同じ論点にしないことである。

## Stage 3: SDカード

SDを先にread-write対応する。USBより書き込み経路が単純で、既存の`sdwritetest`で
CMD25とDMA経路を実機確認済みだからである。

- `SdBlockDevice::write_blocks`から`sdmmc::write_blocks`を呼ぶ
- `sdmmc::wait_data_not_busy`を成否を返す形へ変更する。timeoutをログだけで済ませず、
  `write_blocks`の失敗として上位へ返す
- CMD25成功後にDAT0のbusy解除まで確認できた時点を、SDの書き込み完了境界とする。
  host側に遅延write cacheは無いので、その条件を満たした後の`flush`は成功でよい
- 同じコマンド中に開いた`SdSlot`を使い続け、1操作の途中でカードを再初期化しない

犠牲にできるFAT16またはFAT32カードで次を確認する。

- `mount sd0pN`がFATをread-writeでmountし、作成、置換、追記、mkdir、rename、removeが通る
- `mount -r sd0pN`では同じ変更操作が`read-only`で拒否される
- 短縮置換後のappend
- 数MiBの`fill`とネットワーク受信の`.part`から完成名へのrename
- アンマウント後にPCで全ファイルを読み、`fsck.fat -n`で致命的な不整合が無い
- 再起動後に再マウントして内容が一致する

意図的な途中抜去試験は完了条件にしない。自然発生した失敗では自動再送せず、マウントが
落ちて明示再マウントを要求することだけを確認する。

## Stage 4: USB Mass Storage

USBはSDと別Stageで受け入れる。WRITE(10)自体は実装・実機確認済みだが、書き込みが
transport故障の強い引き金であり、各WRITE直前の予防的BOT再同期を必要とするためである。

- `UsbMscBlockDevice::write_blocks`から`UsbMassStorage::write_blocks`を呼び、既存の4 KiB上限で
  分割する
- USB FATの手動mountとautomountを既定read-writeへ切り替える。exFATはread-onlyのままにする
- WRITE(10)直前の予防的BOT再同期と、失敗したWRITEを再送しない方針を維持する
- `flush`は`SYNCHRONIZE CACHE(10)`とready待ちを行う
- `CacheSync::Flushed`は成功、`CacheSync::Failed`はI/O失敗とする
- `CacheSync::Unsupported`は本計画の信頼性境界では**best effortの成功**とする。ただしsession中
  最初の1回は`flush unsupported; removal durability is not guaranteed`をUARTへ出す。
  WRITEがdeviceへ受理されたこと以上の永続化は保証しない
- DATA PROTECTを判別できた場合は`write protected`として表示する。詳細なsense分類をVFS全体へ
  公開することは完了条件にしない

犠牲にできるFAT媒体で、High-Speed直結と既に受入済みのハブ構成を確認する。

- Stage 3と同じファイル操作一式
- 100回以上の小ファイル作成・追記・削除。ファイルシステムはdata、FAT、directoryを別WRITEに
  するため、rawの1 block試験よりWRITE回数を多くする
- USB HID併用中もHIDを巻き添えにしない
- 失敗時にWRITEを自動再送せず、MSC sessionと該当mountだけが使用不能になる
- アンマウント後、PCで内容確認と`fsck.fat -n`

## Stage 5: 現状文書と回帰

実装と同じ作業で次を更新する。

- [`FILESYSTEM.md`](FILESYSTEM.md): mount mode、FAT/exFATの保証表、失敗時挙動、コマンド
- [`STORAGE.md`](STORAGE.md): SD/USB adapterのwriteと媒体別flush保証
- [`CONSOLE_SHELL.md`](CONSOLE_SHELL.md): FAT/exFATの既定modeと`mount -r`の構文・表示
- [`FILE_LAYOUT.md`](FILE_LAYOUT.md): adapterが書き込み抑止だけを持つという現状記述
- [`../DESIGN.md`](../DESIGN.md): 「書けるのはRAMルートだけ」という制約

`README.md`は人間管理なので変更しない。記述が古くなった場合は、最終報告で不一致箇所と
削除して現状文書へ委ねる案を報告する。

静的確認:

- 変更ファイルの`rustfmt --check`
- `cargo test --workspace --target <host>`
- `cargo build --release`
- ELF配置検査
- `git diff --check`
- `README.md`にこの作業による差分が無いこと

## 完了条件

- 成功した`Truncate`後の`Append`が正しい内容を返す
- SDとUSBのFAT12/16/32が手動mount・automountとも既定read-writeになる
- `mount -r`でFATを明示的にread-onlyにできる
- exFATは手動mount・automountともread-onlyになる
- write、finish、sync、媒体flushのどれかが失敗すれば操作全体が失敗になる
- 成否不明のWRITEを自動再送しない
- 実機で書いた媒体をPCと再起動後のTab5の両方から読める
- 電断原子性、自動修復、完全な耐障害性を保証したとは表示しない
