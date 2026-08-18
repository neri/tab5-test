# 起動とメモリ配置

> 索引: [`../DESIGN.md`](../DESIGN.md)

## 起動とイメージ配置

現行のESP32-P4向けHALにはECO5以降を前提とする初期化が含まれるため、汎用の
`riscv-rt`を使用しています。起動直後に`startup.rs`がブートローダーから継承した
RTC watchdogを停止し、続けてCPUクロックを引き上げます。

2nd-stage bootloaderはECO2上で`CPU_CLK_FREQ_MHZ_BTLD`(90 MHz)をCPLL/4として
構成し、アプリ本体の起動処理でCPLL/1(360 MHz)へ引き上げる前提のままCPUを
引き渡します。本プロジェクトはESP-IDFのアプリ起動処理をリンクしていないため、
これを行わないと全ての`delay_ms`/`delay_us`（D-PHYやDCSの待ち時間、CardKBの
I2Cビットタイミングを含む）が実時間で約4倍かかります。`startup::raise_cpu_clock`は
CPLLの新規有効化やregi2cキャリブレーションを行わず、ブートローダーが既に
キャリブレーション済みのCPLL(360 MHz)に対してCPU/MEM/APBの分周比だけを
（APB→SYS→MEM→CPUの順に）書き換えます。CPUクロック源がCPLLでない場合は
何もせず90 MHzのまま継続します。

ESP-IDF v5.5の2nd-stage bootloaderが読み込めるよう、`memory.x`では次の配置を
定義しています。

- `0x40000020..0x4001fff8`: アプリケーション記述子、通常の読み取り専用データ、
  `.eh_frame`、IROMとの位置関係を固定するパディング（DROM）
- `0x40020000`以降: 通常の実行コード（IROM）
- `0x4ff40000..0x4ff80000`: FLASH停止中にも必要なコードと定数、`.data`、
  `.bss`、スタック（内部L2MEM）

アプリケーション記述子は`src/main.rs`の`EspAppDesc`です。

通常のRustコードはFLASHから直接実行し、通常の定数もFLASHから参照します。一方、
エントリポイント、起動処理、独自CLIC trap、ISR、panic最小経路、PSRAM/MSPI初期化、
ROMキャッシュ操作のラッパーは`.iram.text`、それらが参照する定数は
`.dram.rodata`へ分離します。PSRAM初期化中はmachine interruptを禁止し、通常動作中の
キャッシュ同期では表示割り込みを止めない構成です。後者が安全であることは、trap/ISRを
含むFLASH非依存閉包を`tools/check_elf_layout.py`で検査します。

`.data`はFLASH側へロードイメージを複製せず、2nd-stage bootloaderがRAMのVMAへ
直接ロードします。リンク時に`__sidata == __sdata`を検査し、起動時にも複数の非ゼロ
`.data`とゼロ初期化`.bss`を検査します。

ESP-IDF v5.5.3のESP32-P4ブートローダーは、XIP領域にあるセグメントが
ちょうど2本であることを要求します（`bootloader_utility.c`の`unpack_load_app`、
`assert(rom_index == 2)`）。DROMを固定終端までパディングし、続くIROMとの物理・仮想
アドレスの64 KiBページ内オフセットを一致させることで、`espflash`による余分な
パディングセグメントを防ぎます。これが崩れるとビルドは通っても起動しないため、
`memory.x`のリンク時ASSERTと`tools/check_esp_image.py`の変換後イメージ検査を
併用します。最終イメージはDROM/IROMのXIP 2本とRAMロード2本です。

PSRAM初期化前後には、互いに別の64 byteキャッシュラインにあるDROM/IROMプローブを
実行します。初期実装ではPSRAM用dual-MSPIリセットのbit 23/25ではなくFLASH側の
bit 22/24を操作していたため、初期化後の最初の通常DROM参照で停止しました。
ESP-IDF v5.5.3と同じbit 23/25へ修正し、キャッシュヒットで誤通過しないcold probeに
分離した構成で実機確認しています。

## RAMの範囲

ECO2ではL2キャッシュがL2MEMの上位から確保されるため、使用できるRAMは
`0x4ffc0000`からキャッシュサイズを引いたアドレスで終わります。下端の
`0x4ff40000`はROM予約領域（ROMスタック`0x4ff3cfc0`、ROMの`.data`/`.bss`が
`0x4ff40000`の直前まで）の上端です。2nd-stage bootloader自身は`0x4ff2cbd0`から
配置されるため、ロード先とは重なりません。

キャッシュサイズを決めるのは2nd-stage bootloaderで、ESP-IDFの既定は128 KiB
（上限`0x4ffa0000`）、ハードウェアのリセット値は256 KiB（上限`0x4ff80000`）です。
リンク時には判別できないため、`memory.x`は両方で安全な共通部分である
`0x4ff40000..0x4ff80000`（256 KiB）だけを使用します。実機は128 KiB分割を報告し、
`RAM: usable top=0x4FFA0000`に対して`RAM: stack top=0x4FF80000`となるため、さらに
128 KiBの安全余白があります。256 KiB設定でもstack topとusable topが一致し、
キャッシュ領域とは重なりません。

なおchip revision v3以降はキャッシュがL2MEMの下位から確保され、上限は
`0x4ffaefc0`になります。この値をECO2に適用してはいけません。

XIP移行前は通常`.text`と`.rodata`も同じRAM窓を消費し、機能追加がそのまま
スタックを削っていました。最終release配置では通常RAM `.text`/`.rodata`は0 byte、
`.iram.text` 6,172 byte、`.dram.rodata` 908 byte、`.data` 19,088 byte、`.bss`
49,268 byte、残り`.stack` 186,636 byteです。リンク時に通常コードのRAM回帰を禁止し、
stack 128 KiB以上をASSERTするため、通常アプリコードの増加はスタックを圧迫しません。
今後RAM量に効くのはIRAM/DRAMへ明示したcritical処理と可変データです。

同種の変更を足すときは、`llvm-nm --size-sort --print-size`でシンボル単位の
増分を見るのが手早い方法です。増分が大きいのはたいてい新しいコードそのもの
ではなく、そこから展開された既存の大きい関数です。

## 起動シーケンス

```text
riscv-rt
  → RTC watchdog停止
  → CPUクロックを90 MHzから360 MHz(CPLL/1)へ引き上げ
  → USB Serial/JTAG初期化
  → L2キャッシュ分割とRAM上限をログ出力
  → .data/.bssのboot配置を検査
  → PSRAM初期化前のcold DROM/IROMプローブ
  → 割り込みを止め、IRAM内でPSRAM電源・クロック・DQS調整・MMU割り当て
  → PSRAM初期化後の別cache lineによるcold DROM/IROMプローブ
  → RGB565コンソール画面を描画してキャッシュを同期
  → LCDリセット・D-PHY・パネル初期化
  → DSI BridgeとDW-GDMAを準備してvideo modeを開始
  → IROM上の通常アプリへ移り、InputManagerが変更セルだけを描画・部分同期
```

DSI HostのVideo Pattern Generatorは使用しません。ECO2では動作中のVPGからBridge
入力へ切り替えると、設定値、FIFO量、underrun状態が同一でも稀に黒画面になることを
実機で確認したためです。最初のDMAデータをFIFOへ充填してからHostのvideo modeと
Bridge出力を初めて有効化します。

以前はPSRAMやDMA経路が失敗した場合にVPGのカラーバーを診断表示として出していま
したが、削除しました。VPGへ入るにもDMA経路と同じパネル初期化とBridge設定を通る
ため独立した診断にならず、実際の失敗の多くは初期化がそこへ到達する前に止まるか、
逆にPSRAMの検査を通過してしまい発火しませんでした。失敗の切り分けはUSBシリアルの
ログで行います。
