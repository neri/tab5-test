# FLASH XIP移行計画

## 状態: 全Stage完了（実装・自動検査・実機試験合格）

## 実装状況（2026-08-18）

- Stage 0完了。基準コミットは`1b44cb9`（`add win`）。release ELFの基準値は
  本節以下の表のとおり
- Stage 1のDROM/IROMプローブを実装。DROMパターンは`0x40000120`、IROM関数は
  `0x40001040`（44 byte）に配置
- release + fat LTO後も、RAM側からIROM関数へ実際の間接callが残ることを逆アセンブルで確認
- `tools/check_esp_image.py`を追加し、生成イメージがFLASHマップ2本 + RAMロード1本、
  各XIPセグメントの物理/仮想64 KiBページオフセット一致であることを確認
- Stage 1イメージのサイズは225,360 byte
- PSRAM初期化前後のDROM/IROMプローブは実機で初回成功。共有MSPIのリセット・再設定後も
  FLASHのデータ読み出しとコード実行へ復帰できることを確認した
- Stage 1のコールドブート10回と`reboot`20回はすべて成功。Stage 1完了
- Stage 2として`.iram.text`、`.dram.rodata`とflash-critical境界を追加し、起動、
  独自trap、PSRAM初期化、UART最小診断、panic/exceptionを内部RAMへ分離
- PSRAM初期化中はmachine interruptの状態を保存して禁止し、終了時に元の状態へ復元
- release ELFの専用IRAMは5,972 byte、critical用DRAM定数は908 byte、残りスタックは
  133,964 byte
- `tools/check_elf_layout.py`を追加。critical範囲の92 relocationがすべてIRAM、DRAM、
  内部ROMまたは可変RAMだけを参照し、通常`.text`/`.rodata`やXIPへの漏れがないことを確認
- コンパイラ生成の`memset`/`memcmp`/`memcpy`がcritical閉包外へ出る箇所は、volatile
  zero/compare処理へ置き換えて除去
- Stage 2イメージはFLASHマップ2本 + RAMロード2本の計4セグメント、アプリサイズ
  214,256 byte。ECO2ブートローダーが数えるXIPセグメントは2本のまま
- Stage 2配置で`stress 20`は253 ms（1 fill 12 ms、underrun 8/20）で完走し、
  `reboot`5回もすべて成功。PSRAM、LCD ISR、シェルを含めStage 2完了
- Stage 3として通常`.rodata`と`.eh_frame`をDROMへ移行。内部RAMの残りスタックは
  Stage 2の133,964 byteから169,676 byteへ35,712 byte増加
- DROMは`0x40000020..0x4001fff8`、IROMプローブは`0x40020000`。Stage 3イメージは
  FLASHマップ2本 + RAMロード2本の計4セグメント、修正後アプリサイズ305,616 byte
- `.data`はDROMにロードイメージを重複配置せず、従来どおりbootloaderがRAMのVMAへ
  直接ロードする。`__sidata == __sdata == 0x4ff65df8`を自動検査し、起動時には
  非ゼロ`.data` 3語とゼロ初期化`.bss` 3語をvolatile readで検査
- Stage 3初回実機確認では`XIP: post-PSRAM DROM+IROM ok`の直後、最初の通常DROM
  参照で停止した。調査の結果、PSRAM resetがdual-MSPI用bit 23/25ではなくFLASH
  MSPI用bit 22/24を誤って操作していた。ESP-IDF v5.5.3の
  `psram_ctrlr_ll_reset_module_clock`と一致する23/25へ修正
- 従来のpre/postプローブとphase文字列は同じキャッシュラインを再利用しており、
  FLASH設定消失後もcache hitだけで合格していた。DROMプローブを`0x40000120`と
  `0x40000200`、IROMプローブを`0x40020000`と`0x40020100`へ分離し、post側が必ず
  別の64 byteキャッシュラインを使うよう自動検査を追加
- dual-MSPI reset bit修正版は実機でpost側のcold DROM/IROMプローブを通過し、通常の
  DROM定数を使用する画面・シェル起動まで成功
- Stage 3の`stress 10`は130 ms（1 fill 13 ms、underrun 4/10）で完走。コールド
  ブート10回と`reboot`20回もすべて成功し、Stage 3完了
- Stage 4として通常`.text`入力を`.flash.text`へ先取りし、IROMへ移行。通常RAM
  `.text`は0 byte、IROMコードは148,138 byte、残りスタックは317,772 byte
- 実際のCLIC対応`_start_trap`、`ExceptionHandler`、`esp32p4_interrupt`、PSRAM初期化は
  IRAMに維持。使用しない`riscv-rt`標準trapを破棄し、第二のFLASH依存trap経路を除去
- Stage 4 release ELFでもcritical範囲の92 relocationはIRAM、DRAM、内部ROMまたは
  可変RAMだけを参照。`app::run`はIROM `0x40026aa8`に配置
- Stage 4イメージはFLASHマップ2本 + RAMロード2本の計4セグメント、アプリサイズ
  305,360 byte。DROM/IROMの物理/仮想64 KiBページオフセット検査も合格
- Stage 4版は実機でpre/postのcold DROM/IROMプローブ、PSRAM初期化を通過し、IROM上の
  通常アプリケーションによる画面・シェル起動まで成功
- Stage 4の`stress 20`は254 ms（1 fill 12 ms、underrun 8/20）で完走。コールド
  ブート10回と`reboot`20回もすべて成功し、Stage 4完了
- Stage 5としてPSRAM、SD、USBに分散していたROM cache writeback/invalidate呼び出しを
  `iram_cache_writeback_invalidate`へ集約。IROM上の69 call siteが共通IRAMラッパーを
  経由し、ROMキャッシュ処理中のcall/return経路がFLASH命令に依存しないことを確認
- cache wrapper追加後のIRAMは6,172 byte、IROMは147,320 byte、残りスタックは
  317,708 byte。critical閉包検査とESPイメージ2 XIPセグメント検査は合格
- フル画面writeback中にmachine interruptを禁止するとLCDのフレーム完了を失うため、
  割り込みは許可したままとする。ISR、参照定数、atomic状態がIRAM/DRAM閉包内にある
  ことを機械検査し、実機複合負荷で最終判定する
- Stage 5イメージはFLASHマップ2本 + RAMロード2本、アプリサイズ304,624 byte
- Stage 5版は実機でXIP/PSRAM初期化と、共通IRAM cache wrapperを使用する画面・
  シェル起動まで成功
- Stage 5版の`stress 20`は255 ms（1 fill 12 ms、underrun 8/20）で完走し、Stage 4の
  254 ms、8/20と同等。`ppafill sweep`も12x16から1280x720までPPA/CPU両経路を完走
- `ppafill sweep`中の追加DPI FIFO underrunは1回（累計9）。cache wrapper変更による
  trap、DMA error、ハングはなく、`reboot`5回もすべて成功
- `alloctest 8`はPSRAM heapへ8 MiBを書き込み・照合して成功。`membench`もSRAM、
  cached PSRAM、direct PSRAMの全経路を完走し、追加underrunは各試験1回
- paint、touch、axis、winの画面遷移と、依頼したSD/USBコマンドも正常動作。Stage 5の
  機能試験は完了。90分複合試験はRAM窓縮小後の最終配置で1回だけ実施する
- Stage 6としてRAM窓を`0x4ff40000..0x4ffa0000`（384 KiB）から、128/256 KiBの
  どちらのL2 cache設定でも安全な`0x4ff40000..0x4ff80000`（256 KiB）へ縮小
- 256 KiB RAM窓で残りスタックは186,636 byte（約182.3 KiB）。128 KiB下限を
  55,564 byte上回り、通常`.text`/`.rodata`のRAM回帰とstack下限をリンク時ASSERT化
- Stage 6 release ELF配置検査と、304,624 byteのESPイメージに対するXIP 2セグメント・
  ページオフセット検査はすべて合格
- Stage 6版は実機で正常起動。L2 cacheは128 KiB、実際のusable topは`0x4ffa0000`、
  stack topは`0x4ff80000`で、実機構成ではさらに128 KiBの未使用安全余白を確保。
  256 KiB cache構成でもstack topとusable topが一致し、重ならない
- Stage 6最終配置で`stress 20`、`alloctest 8`、コールドブート10回、`reboot`20回は
  すべて成功。Stage 6完了
- 最終release配置は`.iram.text` 6,172 byte、`.dram.rodata` 908 byte、通常RAM
  `.text`/`.rodata` 0 byte、DROM 130,776 byte、IROM 147,320 byte、`.data`
  19,088 byte、`.bss` 49,268 byte、`.stack` 186,636 byte
- 最終イメージは304,624 byte。DROM/IROMのXIP 2本とRAMロード2本で、変換後の
  appdescと物理/仮想ページ内オフセットを自動検査済み
- Stage 7として表示、USB、SDを含む最終配置を起動後2時間連続運転し、挙動の変化、
  新規エラー、ハングがないことを実機確認。90分複合負荷試験の条件を満たし、
  Stage 7およびFLASH XIP移行の全Stage完了

## 背景・目的

移行前のアプリケーションは、ESP32-P4 ECO2の内部L2 MEMに`.text`、`.rodata`、
`.data`、`.bss`、スタックをすべて配置していた。移行前の`memory.x`のRAM領域は
`0x4ff40000..0x4ffa0000`の384 KiBであり、通常コードが増えるたびに利用可能な
スタックが同じ量だけ減る。

2026-08-18の移行開始時点のreleaseビルドは次のとおり。

| 区分 | セクション | サイズ | 移行前の配置 |
|---|---|---:|---|
| 実行コード | `.text` + `.init.rust` | 164,698 byte（約160.8 KiB） | L2 MEM |
| 読み取り専用 | `.rodata` + `.eh_frame` | 35,464 byte（約34.6 KiB） | L2 MEM |
| 初期化済みデータ | `.data` | 19,080 byte（約18.6 KiB） | L2 MEM |
| ゼロ初期化データ | `.bss` | 49,204 byte（約48.1 KiB） | L2 MEM |
| 残りスタック | `.stack` | 124,748 byte（約121.8 KiB） | L2 MEM |

アラインメントを除くと、FLASHへ移せるコードと読み取り専用データは約195.5 KiB
ある。ここをXIP（Execute In Place）へ移し、内部RAMにはFLASHへアクセスできない
期間にも必要なコードと、書き換え可能なデータだけを残す。

本計画の目的は次の3点。

1. 通常のアプリケーションコードと読み取り専用データをFLASHから実行・参照する。
2. PSRAM/MSPI初期化、キャッシュ操作、例外・割り込み処理を内部RAMへ隔離し、
   FLASHキャッシュが利用できない期間にも安全に動作させる。
3. 将来の機能追加で通常コードが増えても、内部RAMのスタックを直接圧迫しない
   配置にする。

## 前提と最大の技術リスク

ESP32-P4はFLASHをMMUとキャッシュ経由で命令・データ空間へマッピングできる。
したがってXIP自体は可能であり、現行イメージにも次の2本のFLASHマップ済み
セグメントが存在する。

- `0x40000020`: アプリケーション記述子と位置調整パディング
- `0x40001040`: 現在は実行しない4 byteの互換スタブ

一方、`psram::init`はPSRAMとFLASHで共有するMSPIブロックに対して、次の操作を
行う。

- MSPI PHYの電源・クロック設定
- MSPI AXI/APBリセット
- PSRAMコマンドパスの設定とDQS調整
- PSRAM用MMUマッピング
- L1/L2キャッシュの無効化

この処理を無条件にFLASH上へ移すと、処理途中で命令または定数をフェッチできず
停止する可能性がある。さらに、直接の呼び出し先だけをRAMへ置いても、その関数が
FLASH上のログ文字列、コンパイラ生成ヘルパー、panic処理、割り込みハンドラを
参照すれば同じ問題が起きる。

そのため、本計画では「関数単体」ではなく、FLASHへアクセスできない期間に到達
可能なコードとデータの**推移閉包全体**をIRAM/DRAMへ置く。

## ゴール

移行完了時の配置は次を目標とする。

| 領域 | 配置するもの |
|---|---|
| FLASH DROM | アプリケーション記述子、通常の`.rodata`、`.eh_frame` |
| FLASH IROM | 初期化完了後に動く通常の`.text` |
| 内部IRAM | `riscv-rt`起動処理、PSRAM/MSPI初期化閉包、FLASHキャッシュ操作中に必要な処理、trap/ISR、panic最小経路 |
| 内部DRAM | IRAMコードが参照する定数、`.data`、`.bss`、スタック |
| PSRAM | 現状どおりフレームバッファと動的ヒープ |

移行中は現在の384 KiB RAM窓を維持する。XIPが安定しIRAM量が確定した最終段階で、
RAM窓をECO2の128 KiB/256 KiBどちらのL2キャッシュ設定でも安全な
`0x4ff40000..0x4ff80000`（256 KiB）へ戻すことを目標とする。

最終的な定量条件は次のとおり。

- FLASHマップされるイメージセグメントがDROM/IROMのちょうど2本であること
- ELFエントリポイント、trap入口、ISR、PSRAM初期化閉包が内部RAMにあること
- IRAM/DRAMから、FLASH停止期間に到達可能なIROM/DROM参照がないこと
- 256 KiBの安全なRAM窓に戻した後も、`.stack`が128 KiB以上残ること
- 通常の`.text`増加が`.stack`の減少につながらないこと
- コールドブート、CPUリブート、PSRAM初期化、LCD、USB、SD、PPAが現状同様に
  動作すること

## 範囲外

- ESP-IDFまたはRTOSの導入
- 2nd-stage bootloaderの作り直し
- FLASHへの書き込み・OTA機能の追加
- PSRAMヒープまたはフレームバッファ配置の変更
- L2キャッシュ容量そのものの変更
- `.eh_frame`を削除する最適化。まずDROMへ移し、不要性の検証は別作業とする
- XIP化と無関係なモジュール分割やアプリケーション機能のリファクタ

## 設計原則

1. **小さなXIPプローブから始める。** 全体のリンカ配置を変更する前に、
   PSRAM初期化の前後でFLASH上の関数と定数へアクセスできることを実機確認する。
2. **読み取り専用データを先に移す。** DROM移行だけの段階を設け、問題を
   命令フェッチとデータフェッチに分離する。
3. **FLASH停止期間を明示する。** MSPIリセット前からFLASHアクセスの安全が
   再確認できるまでをcritical windowとして扱い、割り込みを禁止する。
4. **IRAM閉包を機械検査する。** 人手による`#[link_section]`確認だけに依存しない。
5. **各Stageを単独で実機確認する。** 起動不能になった場合に、どの変更が原因か
   分からない大きな切り替えを行わない。
6. **SRAM実行版の既知正常コミットを保持する。** 各Stageを独立コミットにし、
   実機確認が終わるまで次へ進まない。

## 目標リンカ配置

`memory.x`では最終的に、概念上次のリージョンを持つ。

```text
FLASH_RODATA  0x40000020 ...  appdesc + DROM + pad
FLASH_TEXT    0x400xxxxx ...  IROM
RAM           0x4ff40000 ...  IRAM + DRAM + BSS + stack
```

FLASHセグメントは、イメージ内の物理アドレスと仮想アドレスの64 KiBページ内
オフセットが一致しなければならない。また、ESP-IDF v5.5.3のECO2ブートローダーは
FLASHマップ対象をちょうど2本と仮定している。このため、DROM容量とIROM開始位置は
独立には決められない。

初期候補は、DROMへ128 KiB弱を確保し、IROMを次の64 KiB境界から始める配置とする。

```text
FLASH_RODATA ORIGIN = 0x40000020, LENGTH = 0x0001ffd8
FLASH_TEXT   ORIGIN = 0x40020000
```

この候補では次の関係が成り立つ。

```text
ORIGIN(FLASH_TEXT) - ORIGIN(FLASH_RODATA)
    == LENGTH(FLASH_RODATA) + 8 byte segment header
```

現在のappdesc、rodata、eh_frameは合計約34.9 KiBなので、DROM側に約93 KiBの
将来余裕を持てる。Stage 3ではこの配置を採用し、DROM終端までの未使用部を同じ
セグメント内でパディングした。`espflash`変換後も余分なパディングセグメントは
生成されず、FLASHマップ対象は2本のままである。

`riscv-rt`の既定`link.x`は、`.data`を`REGION_DATA`へ配置し、そのロード元を
`REGION_RODATA`に置く。ただし本実装では、通常rodataの入力セクションだけを既定
出力より先に`.flash.rodata`へ回収し、`REGION_RODATA`自体はRAMのまま維持する。
これによりbootloaderが従来どおり`.data`をRAMのVMAへ直接ロードし、起動時コピー
方式への変更を避ける。`.data`は実行時にはどちらの方式でもRAMを消費するため、
この判断によるRAM削減量の低下はない。次を必ず確認する。

- ELFの`.data` VMAが内部RAMであること
- `__sidata == __sdata`であり、`__sdata..__edata`が内部RAM内であること
- 起動時コピー後の`BOOT_LAYOUT_MARKER`が`0xEC020001`であること
- `espflash`変換後に第3のFLASHマップセグメントが増えていないこと

## IRAM/DRAMへ残す候補

### 起動・例外経路

- `riscv-rt`の`.init`、`.init.rust`
- `.trap.vector`、`.trap.start`、`.trap.rust`などのtrap入口
- `main`の初期化部分と、XIPアプリ本体へ渡す境界
- `ExceptionHandler`
- 最小panic handler

ELFエントリポイントは最終配置でも内部RAMとする。`main`はPSRAM初期化呼び出しを
またぐため、関数全体をIRAMに置き、初期化成功後に別のXIP関数へ明示的に移る。

### PSRAM/MSPI critical window

保守的な初回実装では、`psram::init`とその全呼び出し先をIRAM候補とする。

- 電源・クロック、IOMUX、MSPIリセット
- command read/writeと完了待ち
- DQSチューニング
- MMU設定とマップ後メモリテスト
- critical window内で使用するdelay、MMIOヘルパー
- コンパイラが生成する`memcpy`、`memset`、比較処理などのヘルパー

ログ文字列をすべてDRAMへ複製するとRAMを消費するため、critical window内部は
可能なら小さなエラーコードを返し、FLASHアクセスが安全になった後でログへ変換する。
ただし、復旧前に停止する経路を診断する最小UART出力と、その固定文字列はIRAM/DRAMに
残す。

### 実行時キャッシュ操作・割り込み

- `_start_trap`と`esp32p4_interrupt`
- ISRから参照するatomic/staticデータ
- ROMキャッシュ関数の実行中に割り込みから到達し得る経路
- FLASHキャッシュを一時停止するROM関数の薄いラッパー

LCD DMA割り込みを有効にした後も、PSRAMのwriteback/invalidateは頻繁に実行される。
ROM関数が命令キャッシュを停止するか、対象アドレスのキャッシュだけを操作するかを
推測せず、Stage 5で割り込みを含めて実測する。必要ならROMキャッシュ関数の前後だけ
machine interruptを保存・禁止・復元する。

## セクションと属性の方針

専用セクションは少なくとも次の4種類に分ける。

```text
.iram.text.*     FLASH停止期間にも実行できるコード
.dram.rodata.*  そのコードが参照する読み取り専用データ
.xip.text.*      明示的なXIPプローブ、移行途中のXIPコード
.xip.rodata.*    明示的なXIPプローブ用定数
```

最終的には通常の`.text`と`.rodata`をそれぞれIROM/DROMへ置き、例外だけを
`.iram.text.*`と`.dram.rodata.*`へ逃がす。

LTOによる境界の崩れを避けるため、XIP/IRAM境界となる関数は`#[inline(never)]`とし、
必要な関数には`#[unsafe(link_section = "...")]`を付ける。小さいMMIOヘルパーを
IRAM関数へ`inline(always)`するのは安全だが、IRAM関数をXIP呼び出し元へインライン
展開させてはならない。

## Stage 0: ベースライン固定

変更前のELFとESPイメージについて、後から比較できる基準値を保存する。

- `cargo build --release`
- `llvm-size -A target/riscv32imafc-unknown-none-elf/release/tab5-hello-world`
- `llvm-readelf -S -l -s`でセクション、LOADセグメント、主要シンボルを記録
- `espflash save-image --chip esp32p4 --merge --skip-padding ...`でアプリイメージを生成
- ESPイメージヘッダと各セグメントのロードアドレス、サイズを記録
- コールドブート、`reboot`、PSRAM初期化、LCD表示の正常ログを保存
- 現在存在するコンパイラ警告を記録し、新しい警告と区別する

現行の基準は、ELFエントリ`0x4ff40000`、ESPイメージ全体のセグメント数3本
（FLASHマップ2本 + RAMロード1本）、アプリサイズ223,456 byteである。

**完了条件**: 数値と実機ログを計画書の「実装結果」へ追記し、既知正常コミットを
特定できること。

## Stage 1: 最小XIPプローブ

通常コードはまだRAMへ置いたまま、現在の4 byte互換スタブを次の2つへ置き換える。

- IROM上の`#[inline(never)]`関数。固定値を計算して返すだけで、RAM関数を呼ばない
- DROM上の固定パターン。IRAM側からvolatile readして検査する

プローブは次の順で呼ぶ。

1. `psram::init`より前にIROM関数を実行し、DROM定数を読む
2. PSRAM/MSPI初期化の直前にIRAMログでマーカーを出す
3. PSRAM/MSPI初期化完了後に再びDROM定数を読む
4. 同じIROM関数を再実行する
5. 成功値をUARTへ出して通常起動を続ける

ここで重要なのは「ブートローダーがXIPを設定できるか」だけでなく、
**現行のMSPIリセットとPSRAM設定を通過した後にもXIPへ戻れるか**を確認すること。
後半のプローブで停止する場合、全面XIP移行には進まない。

停止位置を識別できるよう、各プローブ直前にはFLASHを参照しない短いIRAM/DRAM
ログを出す。FLASH read自体がバス待ちで停止する可能性があるため、ソフトウェアの
タイムアウトだけで回復できるとは仮定しない。

あわせて`tools/`にESPイメージ検査スクリプトを追加し、次を自動判定する。

- ESPイメージのmagicとセグメント数
- 各セグメントのロードアドレスとサイズ
- `0x40000000`のXIP窓に入るセグメントがちょうど2本
- appdescが先頭FLASHセグメントの先頭にある
- 余分なパディングセグメントがない

**完了条件**:

- PSRAM初期化前後のIROM/DROMプローブが実機で成功する
- コールドブート10回と`reboot`20回で停止しない
- FLASHマップセグメントが2本のまま
- 通常アプリはまだRAM実行であり、機能退行がない

**中止条件**: PSRAM初期化後のDROM readまたはIROM callが停止する。この場合は
「失敗時の分岐」に従い、MSPI設定の見直しまたはbootloader側初期化を別計画にする。

## Stage 2: IRAM/DRAM基盤と閉包検査

まだ通常コードをRAMへ置いた状態で、最終移行に必要な専用セクションを導入する。

- `memory.x`にIRAM/DRAM出力セクションを追加
- `.init`、`.init.rust`、`.trap.*`を通常`.text`より先にIRAMへ回収
- `main`、`ExceptionHandler`、panic最小経路をIRAMへ配置
- `psram::init`のcritical windowを明示し、必要なコードをIRAMへ配置
- critical windowが参照する定数をDRAMへ配置、またはログをエラーコード化
- critical windowの前後でmachine interruptの状態を保存・復元
- 正常・各エラー経路のどこからでも、FLASHアクセスを再開できる状態にしてから
  XIPコードへ戻る構造にする

fat LTO後の実配置を検査するため、検査用ビルドではリンカに`--emit-relocs`を渡し、
IRAMセクションからIROM/DROMセクションへのrelocationを列挙するツールを追加する。
直接callだけでなく、PC相対の定数参照も対象とする。

機械検査だけでは間接関数呼び出しを完全には証明できないため、次も併用する。

- `llvm-nm`で必須シンボルのアドレス範囲を検査
- `llvm-objdump -d`でIRAM閉包を逆アセンブル確認
- 関数ポインタを使うROM呼び出し先が内部ROMアドレスであることを確認
- critical window内ではtrait objectや動的dispatchを増やさない

**完了条件**:

- 必須IRAMシンボルがすべて`0x4ff40000`台にある
- IRAM→IROM/DROMの禁止relocationが0件
- 現行RAM実行配置のまま全機能が動く
- critical windowの開始・終了がコード上とUARTログで明確になっている

## Stage 3: 読み取り専用データのDROM移行

通常`.text`はRAM実行のまま、通常rodata入力を専用`.flash.rodata`へ先取りする。

- `.flash.appdesc`をESPイメージヘッダ直後に維持
- 通常`.rodata`と`.eh_frame`をDROMへ配置
- `.data`はbootloaderによるRAMへの直接ロードを維持し、VMA/LMA一致を検査
- IRAMコードが必要とする定数だけを`.dram.rodata.*`へ残す
- 固定長Rust staticの`XIP_SEGMENT_PAD`を削除し、リンカでDROM終端まで埋める
- DROM終端とIROM開始位置の関係をリンク時`ASSERT`で検査

起動直後に次を検査してから周辺機器初期化へ進む。

- `BOOT_LAYOUT_MARKER`
- `.data`内の複数の非ゼロ初期値
- `.bss`内の複数の値がゼロ
- DROMプローブ定数

このStageで期待できるRAM削減は最大約34.6 KiB。ただしIRAM用定数分だけ少なくなる。

**完了条件**:

- `__sidata == __sdata`かつ`__sdata..__edata`がRAMにある
- appdesc、セグメント数、64 KiBページオフセット検査が成功
- PSRAM初期化前後のDROMアクセスが成功
- コールドブート10回、`reboot`20回が成功
- `.stack`がStage 2より増えている

## Stage 4: 通常コードのIROM移行

`REGION_TEXT`をFLASH_TEXTへ変更し、通常の`.text`をIROMへ移す。Stage 2で
用意した`.iram.text.*`だけを内部RAMへ残す。

起動の制御境界は次の形にする。

```text
IRAM: riscv-rt start
  -> IRAM: main/startup shell
  -> IRAM: PSRAM/MSPI critical initialization
  -> IROM: app::run and normal application
```

`riscv-rt`の`.init`と`.init.rust`が誤ってIROMへ移ると、MSPI再設定後に戻れない
可能性があるため、出力セクションで明示的に先取りする。trap入口も同様に
既定`.text`より先にIRAMへ回収する。

Stage 2の閉包検査をrelease + fat LTOの最終ELFに対して再実行する。ソース上の
属性ではなく、最終アドレスを合否判定に使う。

このStageで期待できる追加RAM削減は最大約160.8 KiB。実際には起動・PSRAM・ISR
コードをIRAMへ残すため、削減量はこれより少ない。

**完了条件**:

- ELFエントリ、trap、ISR、PSRAM初期化閉包がRAM内
- `app::run`と代表的なアプリ・USB・描画関数がIROM内
- IRAM閉包からの禁止参照が0件
- ESPイメージのXIPセグメントがDROM/IROMの2本
- PSRAM初期化前後のXIPプローブが成功
- 通常起動、エラー復帰、panic診断が少なくとも意図した経路で動作

## Stage 5: 実行時キャッシュ操作と割り込みの堅牢化

起動後にはLCD DMA割り込みが動き続ける一方、フレームバッファ、PPA、SD、USBの
各経路からROMのcache writeback/invalidate関数が呼ばれる。XIP化後はこの組み合わせを
重点的に確認する。

- `_start_trap`と`esp32p4_interrupt`を常にIRAMへ置く
- ISRが参照するデータと定数を内部RAMへ置く
- ROMキャッシュ関数の実行前後で割り込み禁止が必要か実機確認
- 必要な場合は、現在のmstatusを保存して最短区間だけmachine interruptを禁止
- キャッシュ操作中のUARTログやpanicがFLASH定数へ触れないことを確認
- CPU-only reboot後に古いI/Dキャッシュラインが残る場合の無効化順を確認

実機試験は少なくとも次を含める。

- 連続画面更新とスクロール
- `stress 20`とPPA fill
- paint/win/touch/axisなどコード量が大きいアプリへの遷移
- USBキーボード・マウス・ハブ・MSCの列挙と連続操作
- SDカードの複数ブロックDMA読み出し
- PSRAMヒープの確保・解放を伴う操作
- LCD割り込み発生中の部分flush
- 各操作後の`reboot`と再初期化

**完了条件**:

- trap、DMA error、キャッシュ不整合、ハングがない
- UARTの既存エラーログが増えない
- LCD underrunが同一条件のRAM実行版より悪化しない
- 最終RAM配置でUSB/SD/表示を同時に使う90分連続試験が完走する（Stage 6後に一度実施）

## Stage 6: RAM窓縮小と容量ガード

XIP配置が安定しIRAMサイズが確定した後、`memory.x`のRAM長を
`0x00060000`から`0x00040000`へ戻す。

```text
RAM: 0x4ff40000..0x4ff80000
```

この範囲は、ECO2のL2キャッシュが128 KiB設定でも256 KiB設定でも安全である。
現在の384 KiB窓は128 KiBキャッシュ設定を実機確認したことに依存しているが、
XIP後はその依存を外せる見込みが高い。

リンク時に次の`ASSERT`相当を追加する。

- IRAM/DRAM/BSSがRAMリージョン内に収まる
- `.stack`が128 KiB以上
- IRAM専用セクションがFLASHリージョンへ漏れていない
- 通常`.text`と`.rodata`がRAMへ戻っていない

**完了条件**:

- 256 KiB RAM窓でreleaseビルドが成功
- `.stack >= 0x20000`
- `startup::log_ram_limit`が128 KiB/256 KiBキャッシュのどちらでも安全な
  上端を示す設計になっている
- Stage 5の実機試験を再実行して完走する

IRAM閉包が大きすぎてスタック128 KiBを確保できない場合、RAM窓縮小だけを延期し、
XIP化自体は384 KiB窓で運用できる。ただし、どのシンボルがIRAMを消費しているかを
`llvm-nm --size-sort --print-size`で記録し、縮小を断念した理由を残す。

## Stage 7: 性能・回帰確認とドキュメント更新

XIPはキャッシュヒット時には高速だが、初回実行や大きなコード間の遷移では
FLASH待ちが発生する。容量だけでなく、次をRAM実行版と比較する。

- 電源投入から`PSRAM: ready`、画面表示完了までの時間
- シェル入力から画面更新までの応答
- 大きなアプリを初めて開く時間と2回目の時間
- USBポーリング中の入力欠落
- `stress 20`の時間とunderrun数
- 90分連続試験中の例外・DMA error・再列挙回数

明確なホットパスだけが悪化する場合、その関数を個別にIRAMへ戻す。モジュール全体を
根拠なくIRAMへ戻さず、測定値とシンボルサイズを記録して判断する。

実機確認後に次を更新する。

- `DESIGN.md`の起動・イメージ配置、RAM範囲、実測セクションサイズ
- `DESIGN.md`のファイル構成と起動ログ例
- 本計画の状態、各Stageの結果、最終メモリマップ

`README.md`は人間管理のため、本計画の実施では変更しない。実装後にREADMEとの
不一致が見つかった場合も、ユーザーから明示指示がない限り最終報告だけで伝える。

**実施結果**:

- 最終配置の`stress 20`、`alloctest 8`、コールドブート10回、`reboot`20回は成功
- Stage 5配置の`stress 20`は255 ms、1 fill 12 ms、underrun 8/20で、移行前の
  PPA経路と同等。`ppafill sweep`、`membench`、画面遷移、USB、SDも完走
- 最終配置を起動後2時間連続運転し、挙動の変化、新規エラー、ハングは認められなかった。
  所定の90分複合負荷試験を合格

## 自動検査として残すもの

移行時だけの手作業にせず、少なくとも次の検査を再実行可能な形で`tools/`へ残す。

1. ELFセクション・主要シンボルの配置検査
2. IRAMからIROM/DROMへの禁止relocation検査
3. ESPイメージのセグメント数、ロードアドレス、サイズ検査
4. appdesc位置とmagicの検査
5. RAM使用量、IRAM量、DROM/IROM量、スタック残量の一覧

通常の完了確認コマンドは次を想定する。

```text
cargo fmt --check
cargo build --release
<ELF/XIP layout checker>
espflash save-image --chip esp32p4 --merge --skip-padding <ELF> <temporary image>
<ESP image segment checker>
```

一時イメージは`/tmp`へ作り、リポジトリへコミットしない。

## 想定される罠

### IRAM関数がFLASH上の文字列を参照する

関数本体のアドレスだけを確認しても安全とは限らない。`uart::log(b"...")`の文字列、
match table、panic情報、配列定数がDROMにあれば、critical windowで停止する。
DRAM配置またはエラーコード化が必要。

### fat LTOで関数の境界が変わる

debugビルドで正しくてもrelease + fat LTOでインライン化、outlined helper生成、定数統合が
起きる。合否判定は必ずrelease ELFに対して行う。

### `riscv-rt`の起動・trapセクションが通常`.text`へ吸収される

既定`link.x`は`.init`、`.trap.*`、`.text.*`を同じ出力`.text`へ集める。
通常`.text`をIROMへ変える前に、IRAM出力セクションがこれらを先取りする必要がある。

### `.data`ロード元の変更で初期値が壊れる

`REGION_RODATA`をFLASHへ変えると`__sidata`も変わる。VMA/LMAとESPイメージ変換を
両方検査し、`BOOT_LAYOUT_MARKER`だけでなく複数の初期値を実機確認する。

### XIPセグメントが3本になる

appdesc、rodata、textの間にorphan sectionや`espflash`の位置調整用セグメントが
入ると、ECO2ブートローダーの`assert(rom_index == 2)`に当たり、ビルド成功後に
起動だけ失敗する。リンク時ASSERTとESPイメージ解析の両方で防ぐ。

### 割り込みだけが低頻度で停止する

通常動作は成功しても、ROMキャッシュ関数の実行中にLCD DMA割り込みが重なると、
trap入口またはISRのIROM/DROM参照で停止し得る。連続負荷試験と、trap/ISRの完全な
IRAM/DRAM配置が必要。

### XIP化でL2キャッシュ競合が増える

命令・定数のFLASHアクセスがL2キャッシュを使うため、PSRAMデータとの競合や
キャッシュミスが増える可能性がある。容量改善だけで合格とせず、LCD、USB、SDを
同時に動かすStage 5/7の試験で判定する。

## 失敗時の分岐

### Stage 1の初期化後XIPプローブが失敗する場合

全面XIPへ進まない。次を独立に調査する。

1. `reset_mspi`後にFLASH側レジスタまたはMMU設定が失われていないか
2. FLASH用I/Dキャッシュの明示的なinvalidate/re-enableが必要か
3. PSRAM側だけをリセットし、FLASH経路を保存する手順があるか
4. PSRAM初期化を2nd-stage bootloader側へ移す必要があるか

bootloader変更が必要なら本計画の範囲を超えるため、別計画として承認を取る。

### DROMは動くがIROMが不安定な場合

Stage 3までを採用し、rodataだけをFLASHへ移す。これだけでも約35 KiBのRAMを
回収できる。さらに、実測で安全なコールド関数だけを`.xip.text.*`へ個別移行する。

### 特定のホットパスだけ性能が悪化する場合

測定した関数だけをIRAMへ戻す。候補はtrap/ISR、フレーム同期、短周期USBポーリング
などであり、UIや診断コマンド全体をRAMへ戻さない。

### 256 KiB RAM窓へ縮小できない場合

384 KiB窓のXIP構成を維持し、IRAMの大きいシンボルを記録する。スタックが現状より
十分増え、通常コードの追加で減らないことを確認できれば、XIP移行の主目的自体は
達成と判断できる。

## 最終完了条件

次をすべて満たした時点で正式な移行完了とする。2026-08-18時点ですべて満たしている。

- releaseビルドと全自動レイアウト検査が成功する
- DROM/IROMの2本だけがFLASHマップ対象である
- 起動・trap・PSRAM・キャッシュcritical codeがIRAM/DRAMで閉じている
- 通常アプリケーションコードとrodataがXIPになっている
- 可能ならRAM窓が安全な256 KiBへ戻り、スタックが128 KiB以上ある
- コールドブート10回、`reboot`20回、90分複合負荷試験が成功する
- RAM実行版と比較して機能退行や許容できない性能低下がない
- `DESIGN.md`と本計画に最終配置、実測サイズ、実機結果が反映されている
- `README.md`に本作業による差分がない
