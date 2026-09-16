# TLSFグローバルアロケータ移行計画

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 未着手

## 背景

現在のグローバルアロケータは、PSRAMのフレームバッファとRAM diskを除いた
23,322,624 byteを`linked_list_allocator::LockedHeap`へ一括して渡す構成である。
同allocatorは追加の固定metadataをほとんど要さない一方、空き領域をaddress順の
linked listとして持つfirst-fit方式なので、allocationとdeallocationは空きblock数に
対してO(n)になる。小さい`Vec`、`String`、`Box`が増えると探索時間と外部断片化が
allocation履歴に依存する。

当初は小さいallocationだけをslabへ送る二段構成を検討した。しかし固定slab領域は、
小allocationが少ないときにも大きい`Vec`へ返せず、classごとの枯渇、fallback元の
判定、しきい値をまたぐ`realloc`を新たに管理する必要がある。

本計画では先に、ヒープ全体をTwo-Level Segregated Fit（TLSF）で管理する。
`embedded-alloc` 0.7系の`TlsfHeap`は`no_std`と`GlobalAlloc`に対応し、内部では
`rlsf::Tlsf`を使用する。大きさを二段のsize classへ分類し、空でないclassをbitmapで
選ぶため、通常のallocationとdeallocationはヒープ内のblock数によらず定時間である。
隣接する空きblockも解放時に結合する。小allocation専用領域を予約せず、全容量を
大小のallocationで共有できることを、slabより先に試す理由とする。

ただし`embedded-alloc::TlsfHeap`は内部状態を`critical_section::with`で保護する。
本機ではその実装を自前で提供し、allocator metadataを操作する間だけmachine
interruptを禁止する必要がある。また同crateの`used()`と`free()`は全blockを
critical section内で走査するため、runtime診断には使用しない。

## ゴール

- PSRAM heapの開始addressと総容量を変えず、グローバルアロケータを
  `linked_list_allocator::LockedHeap`から`embedded_alloc::TlsfHeap`へ置き換える
- allocation/deallocationのcritical sectionがdisplay、1 kHz tick、USBの割り込みを
  実機で妨げないことを確認する
- `heap_used()`は全block走査をせず、成功したallocationの要求byte数をatomicに
  追跡して返す
- 大容量の連続allocation、browser/TLS、filesystem、USBを含む既存の動作を維持する
- 実測でTLSFの利点がない、または割り込み遅延や断片化が悪化する場合は採用しない

## 対象外

- slab allocatorとの二段構成
- allocatorからのOOM recovery、heap拡張、複数heap region
- ISR内でのallocation
- core 1の起動と複数hartからの同時allocation
- PSRAM、framebuffer、RAM diskの境界変更
- allocation失敗時にabortする既存の`alloc` API自体の方針変更

slabの要否はTLSF移行後のサイズ分布、最大待ち時間、長時間試験を見て別計画で判断する。

## 採用候補と依存設定

`embedded-alloc`は既定featureでLLFFとTLSFの両方を有効にするため、使用しない
linked-list実装を入れないよう次の形を候補とする。実装時には採用versionのrelease noteと
lockfileの実versionを再確認する。

```toml
embedded-alloc = { version = "0.7", default-features = false, features = ["tlsf"] }
critical-section = "1.2"
riscv = { version = "0.16", features = ["critical-section-single-hart"] }
```

`critical-section` 1.2と`riscv` 0.16は現在も`riscv-rt`から推移依存に入っているが、
本firmwareがAPIを直接使い、`riscv`のsingle-hart実装を明示的に選ぶため直接依存として
宣言する。`critical-section-single-hart`は`restore-state-bool`も有効にし、critical
sectionへ入る直前の`mstatus.MIE`を保存してnested callでも元の状態へ戻す。

critical-section実装はbinary全体で一つだけでなければならない。実装時と依存更新時に
`cargo tree -e features`を確認し、別crateが別の`set_impl!`を有効化していないことを
確認する。独自の`critical_section::Impl`は追加しない。

`linked_list_allocator`は比較中には残してよいが、TLSFの受入後に直接依存から削除する。
一つのbinaryに`#[global_allocator]`を二つ置かない。runtime切替も実装しない。

## 設計

### allocator wrapper

`src/allocator.rs`を追加し、`main.rs`がbackend crateを直接操作しない形にする。
公開範囲はcrate内だけとし、概念上の責務を次に固定する。

```rust
pub struct GlobalHeap {
    backend: embedded_alloc::TlsfHeap,
    requested_live: AtomicUsize,
    requested_peak: AtomicUsize,
    // allocator-diagnostics時だけ追加counterを持つ
}

impl GlobalHeap {
    pub const fn empty() -> Self;
    pub unsafe fn init(&self, start: *mut u8, size: usize);
    pub fn requested_live(&self) -> usize;
}

unsafe impl GlobalAlloc for GlobalHeap { /* backendへ委譲 */ }
```

- `alloc`はbackendが非nullを返した場合だけ`layout.size()`を`requested_live`へ加算する
- `dealloc`は呼び出し元から渡された同じ`Layout`のsizeを減算する
- counterは所有権や同期には使わないため`Ordering::Relaxed`とする
- peak更新はCASまたは`fetch_max`で行い、allocator内でformat、UART、`Vec`を使わない
- OOM時はlive byteを増やさず、diagnostics有効時だけfailure countを増やす
- 最初は`realloc`をoverrideしない。`GlobalAlloc`既定のallocate-copy-deallocateにより、
  wrapper自身の`alloc`と`dealloc`を通してcounterを更新する
- `alloc_zeroed`も既定実装を使い、zero fill全体をcritical sectionへ入れない
- backendから返ったpointerが初期化時に記録したheap範囲外、deallocationでlive byteが
  underflow、live byteがheap総容量を超過した場合は、allocator内でpanicやlogを行わず
  stickyなatomic integrity flagを立てる

この値は「要求され、現在生存しているpayload byte」の概算であり、TLSFのblock header、
alignment padding、size classによる丸めを含まない。現在の`LockedHeap::used()`と同じ
絶対値にはならないが、browserのleak検査が必要とする「同じ操作の前後で元へ戻るか」は
維持できる。名称と意味は`PSRAM.md`に明記する。

既定`realloc`は新blockを確保してpayloadをcopyした後で旧blockを解放するため、その間は
新旧両方が`requested_live`へ入る。これによって上がる`requested_peak`はcounter誤差では
なく、realloc中に実在した同時占有量として扱う。

### critical section

`riscv` crateの`critical-section-single-hart`実装を使う。これはM-modeの`mstatus.MIE`を
保存してclearし、release時は直前に有効だった場合だけ再度setする。

- 既に割り込み禁止中、またはcritical sectionがnestedした場合、内側のreleaseで
  割り込みを誤って有効化しない
- allocator初期化は`app::run`とinterrupt installより前だが、その時点でMIE=falseなら
  falseのまま戻す
- 現在はcore 0しか動かさない。MIE操作だけでは別hartと排他できないため、core 1を
  起動する変更より先にhart間lockを追加する
- ISRはheapを使用しない。Stage 0で静的に確認し、今後の契約として`FILE_LAYOUT.md`へ記す

`critical_section::with`はprogram全体で共有する排他APIであり、TLSF専用ではない。現在と
将来の依存crateを含め、allocator以外に長いclosureを渡す使用箇所がないことも監査する。
将来独自実装へ替える場合は、単に既存のMIE helperを呼ぶだけではなく、同APIが要求する
Acquire/Release相当のmemory orderingとcompiler fenceを満たすことを別途証明する。

通常のTLSF allocationがcritical section内で行うのは、bitmapとfree listの更新、最大1回の
block分割、隣接blockの結合である。大きな`Vec`のpayload copyやzero fillは範囲外である。

### diagnostics

`TlsfHeap::used()`と`free()`はruntime経路から呼ばない。これらは全blockを走査し、その間
critical sectionを保持するためである。

既存`mem`コマンドへ、常時利用できる`requested live/peak`を追加する。実機評価用の
`allocator-diagnostics` featureではさらに次を固定長atomic counterとして記録する。

- allocation/deallocation/failure回数
- `0..=31`、`32..=63`、…、`1 MiB超`のsize histogram
- 要求alignmentの最大値
- backendのallocation/deallocation呼び出し別last/max cycles
- stickyなallocator integrity flag

公式RISC-V critical-section実装へ計測codeを混ぜず、`GlobalHeap`がbackendの`alloc`と
`dealloc`を呼ぶ直前・直後を`rdcycle`で挟む。TLSFのbackend呼出時間はほぼcritical section
そのものなので、安全側の近似として扱える。allocationとdeallocationは別々に最大値を
持ち、どちらが遅いかを失わない。診断featureを無効にした通常buildにはhistogramと
cycle計測を残さない。

### USB DMA bufferとcache lineの所有

USB HCDでは過去に、DMA bufferの開始addressまたは同期範囲が64-byte cache lineへ揃って
いないため、ROMのcache writeback/invalidateが拒否される、または隣のownerのdataを
巻き込む事故があった。現在のUSB driverはこの経験を契約にしており、channel 0 payload、
QTD、periodic report、frame listなどのDMA共有objectを64-byte alignedかつcache lineの
整数倍の大きさで保持する。callerの任意sliceは直接DMAへ渡さず、専用staging bufferを使う。

TLSFはallocated payloadの直前などにblock metadataを置くため、allocator変更後も
「型に`align(64)`が付いている」だけで合格にしない。USBの各DMA siteについて次を確認する。

- DMAへ実際に渡した開始addressが64-byte境界である
- writeback/invalidateへ渡す長さを64-byteへ切り上げても、そのallocationまたは専用objectの
  所有範囲から出ない
- payloadとallocator metadata、別のDMA object、CPUだけが所有するobjectが同じcache lineを
  共有しない
- caller sliceをstagingする経路と、直接DMA可能と判定する経路の両方で上記を満たす
- cache-sync refusalの総数とsite別counterが既存baselineから増えない

静的な型とaddress計算の監査に加え、実機ではUSB keyboard、直結MSC、hub配下MSC、
keyboardとMSCの併用を通して確認する。allocator変更と無関係に見えるUSB列挙成功だけでは
受入とせず、cache refusal、packet retry、command retry、data照合まで判定対象にする。

## Stage 0: 現状baselineと契約の固定

コードを変更する前に、次を記録する。

- `cargo build --release`の成否とELF size
- `mem`、`alloctest 1`、`alloctest 8`の結果
- `stress 20`のunderrun数
- browserの状態表示と`bt 1`のheap before/after
- `mix 1`のheap検査、USB retry、display underrun
- ISRから到達可能な関数に`Vec`、`String`、`Box`、明示的な`alloc`がないこと
- `critical_section::with`の使用箇所とcritical-section実装featureを依存treeまで確認し、
  長時間closureと複数の`set_impl!`がないこと
- USB HCDの全DMA siteについて、objectの型、実address、同期長、staging有無を一覧化し、
  64-byte alignmentとcache line単位の所有境界を満たすこと
- `usbhw`と`mix 1`でcache-sync refusalの総数・site別counterを記録する

**完了条件:** 比較に使うUART出力と、allocatorがISRから呼ばれない静的確認結果が
この文書へ人間から伝えられた実機結果として追記される。実機値を得る前に推測で
baselineを書かない。

## Stage 1: `GlobalHeap`とallocation統計を現行backendへ導入

まずbackendは`LockedHeap`のままwrapperとcounterを導入し、TLSF変更と診断値変更を
分離する。

- `src/allocator.rs`を追加する
- `main.rs`のstaticと初期化を`GlobalHeap`経由へ切り替える
- `heap_used()`を`requested_live()`へ切り替える
- `mem`へrequested live/peakを表示する
- hostで試せるcounter更新、peak、OOM非加算の純粋部分はunit testに分ける
- `cargo build --release`とworkspaceのhost testを通す

**実機確認:** browser状態表示を記録し、同じpageの表示・破棄を20回行ってlive byteが
開始値へ戻ること、`bt 1`前後が一致すること、`alloctest 8`が従来どおり通ることを確認する。

**完了条件:** backendを変えていない状態で新counterがleak診断として使える。値の絶対値が
旧`used()`と違うこと自体は失敗にしない。

## Stage 2: RISC-V single-hart critical-sectionの有効化

- `critical-section`と`riscv`を直接依存へ追加する
- `riscv`の`critical-section-single-hart` featureを有効にし、独自実装は追加しない
- `cargo tree -e features`とlink結果からcritical-section実装が一つだけであることを確認する
- nested call、もともとMIE=false、もともとMIE=trueの3ケースをtarget上の小さな診断で
  区別できるようにする
- allocator以外の`critical_section::with`使用箇所に長時間処理がないことを確認する

**実機確認:** 起動直後、display/USB/tick install後の双方でMIEが呼出前の状態へ戻ること、
1 kHz tickが進み、display frame sequenceとUSB入力が止まらないことを確認する。

**完了条件:** 空のcritical sectionとnested critical sectionを繰り返してもMIE状態が壊れず、
display DMA error、unknown interrupt、tick停止がない。

## Stage 3: backendをTLSFへ置換

- `embedded-alloc`をTLSF featureだけで追加する
- `GlobalHeap`のbackendと`init`を`TlsfHeap`へ変更する
- heap start/sizeは`Psram::heap()`の戻り値をそのまま使う
- `TlsfHeap::used/free`をproduction codeから呼ばないことを`rg`で確認する
- `linked_list_allocator`を使う箇所がなくなったことを確認し、直接依存を削除する
- `cargo build --release`、workspaceのhost testを通す
- `cargo tree -e features`でLLFF featureが入らず、critical-section実装が一つであることを
  再確認する
- release ELFの増減をStage 0と比較して記録する

**実機確認:** `mem`、`alloctest 1`、`alloctest 8`、browser page表示、TLS接続、RAM disk上の
file読み書きを行う。allocation failure countは0、live byteは一時処理の終了後に開始値へ
戻ることを期待する。

USBについては、USB keyboard単体、MSC直結、hub配下MSC、keyboard＋MSC併用の各構成で
列挙と転送を行う。DMA bufferの実addressと同期長が64-byte境界を満たし、cache-sync
refusalの総数・site別counterがStage 0のbaselineから増えず、読み出したdataが一致することを
確認する。

**完了条件:** 大小のallocationとalignmentを含む既存経路が成立し、再起動やpanic、heapの
不一致がない。ここではまだ性能改善を採用理由にしない。

## Stage 4: 割り込み禁止時間とallocation workloadの実測

`allocator-diagnostics`を有効にしたrelease buildを実機へ書き込み、次の順で負荷をかける。

1. 起動直後に`mem`を記録する
2. `alloctest 1`と`alloctest 8`
3. `hs`またはbrowserでHTTP/TLS pageを20回取得・破棄する
4. `bt 1`
5. USB keyboardとUSB Mass Storageを接続して`mix 1`
6. 再度`mem`を記録する

CPU周波数は固定値を仮定せず`startup::cpu_hz()`で換算する。backend呼出時間を
critical sectionの安全側の近似として使い、通常alloc/deallocの最大値は**目標100 us未満**、
**1 ms以上なら不採用**とする。100 us以上1 ms未満はUSB retry、tick、frame underrunの変化と
発生workloadを見て判断する。

- 1 msはSYSTIMER周期であり、これを超える区間はtick eventをまとめてしまう恐れがある
- display frameは実測平均約17.468 msであり、そこへ近づく値は無条件で不採用とする
- max値だけでなく、どの操作後に更新されたかを段階ごとの`mem`で特定する
- `TlsfHeap::used/free`の全block走査を測定に混ぜない
- USB試験の前後でcache-sync refusalの総数・site別counterを比較し、1件でも新規refusalが
  あればcritical-section時間に関係なく不採用とする
- stickyなallocator integrity flagが一度も立たないことを確認する

**完了条件:** 最大backend呼出時間が採用基準内で、tick停止なし、display underrun増加なし、
USB packet/command retryがbaselineより悪化せず、failure countが0である。結果は人間から
伝えられた後にこの文書へ記録する。

## Stage 5: 長時間回帰と採用判断

短時間試験に合格した場合だけ通常release build（diagnostics featureなし）で確認する。

- `stress 20`
- `mix`既定120分
- browser/TLSの反復試験
- `alloctest 8`
- cold boot 10回と`reboot` 20回

期待値は次のとおり。

- heap mismatch、allocation failure、panicが0
- browser/TLS試験後にrequested live byteがbaselineへ戻る
- display underrunとDMA errorが既存の受入値から悪化しない
- USB command retryが0で、packet retryやrescanが既存範囲を外れない
- 起動ごとのPSRAM heap容量が23,322,624 byteのまま変わらない
- stickyなallocator integrity flagが一度も立たない

**採用:** すべて満たした場合、`PSRAM.md`のグローバルアロケータ節、`FILE_LAYOUT.md`、
`DIAGNOSTICS.md`を現状実装へ同期し、この計画へ実機結果を記録して完了とする。

**不採用:** backend呼出時間が1 ms以上、回帰試験失敗、最大連続allocationの悪化、または
再現するheap破損があれば`LockedHeap`へ戻す。失敗条件と再現手順はこの文書へ残す。
slab追加で隠さず、TLSF backend自体の採否を先に確定する。

採用後に`embedded-alloc`、`rlsf`、`critical-section`、`riscv`の実versionまたはfeatureが
変わる依存更新を行う場合は、通常のlibrary更新だけで済ませずStage 3〜5相当のbuild、
割り込み時間測定、USB DMA回帰、長時間試験を再実施する。

## 想定される罠

### 公式single-hart実装を別実装やspin lockで置き換えない

single hartでforegroundがlockを保持したままinterruptにpreemptされ、ISRが同じlockを取ると
永久待ちになる。現状はISR allocation禁止だが、`TlsfHeap`が要求する排他は`riscv` crateの
MIE保存・禁止・復元で満たす。将来のmulti-hartではこのfeatureをそのまま使わず、MIE禁止に
加えてhart間lockとmemory orderingを満たす実装へ計画的に置き換える。

### `used()`を便利なcounterとして使わない

`TlsfHeap::used()`と`free()`はO(1)のallocation algorithmとは別物で、全block走査である。
GUIのframe loop、network poll、定期diagnosticsから呼ばない。正確なbackend占有量を一度だけ
調べたい場合も、display/USBへの影響を許容する明示的な診断として別途判断する。

### payload byteと実占有量を混同しない

atomic counterはleakの差分を見る値で、残り最大連続領域やOOMまでの余裕を保証しない。
header、alignment、丸め、外部断片化があるため、`heap_size - requested_live`を
「確保可能byte」と表示してはならない。

### allocator内部からallocationしない

format、ログ文字列、可変長collection、panic message構築をwrapperやcritical-section実装へ
入れると再帰する。統計は固定長atomicだけにし、文字列化は`mem`コマンド側で行う。

integrity異常も同じ理由でallocator内からpanicまたはUART出力しない。範囲外pointer、counter
underflow、容量超過はsticky flagへ畳み、foregroundの`mem`が後から表示する。flagが立った
buildは、その後の見かけ上の動作にかかわらず受入失敗とする。

### OOMの原因をcounterだけから断定しない

allocation failureは総空き容量不足、alignment、外部断片化のいずれでも発生し得る。
`heap_size - requested_live`は実際の最大確保可能量ではない。通常運用で原因を分類するために
`TlsfHeap::free()`の全走査を追加せず、failure時の要求size/alignment、live/peak、直前の
workloadを固定長counterへ残して再現試験で切り分ける。

### instrumentation自身の影響を分離する

histogram、`rdcycle`、peak CASはallocation hot pathへ追加負荷を持つ。Stage 4は最悪値を
見つける診断buildであり、Stage 5は診断を外したproduction相当buildで再確認する。

### heap上のDMA payloadとallocator metadataを同じcache lineへ置かない

TLSFがalignment要求どおりのpayload pointerを返しても、同期長を上へ丸めた結果がpayloadの
要求sizeを越える場合は安全とは限らない。特にUSBでは、cache lineより短いobjectや末尾が
line途中のobjectをそのままDMA共有しない。`repr(align(64))`、64-byteの整数倍のstorage、
専用stagingの三つを一体の契約として維持する。違反時にcache範囲を下へ丸めて通す修正は、
allocator metadataや隣接objectを巻き込むため行わない。
