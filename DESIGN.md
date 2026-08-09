# 設計資料

## 対象と方針

このプロジェクトはM5Stack Tab5のESP32-P4 ECO2（chip revision v1.3）を対象に
しています。ESP-IDFやRTOSをリンクせず、`riscv-rt`とレジスタ操作だけで起動、
PSRAM、MIPI-DSI、GDMAを初期化します。

実機で確認した構成は次のとおりです。

- ESP32-P4 ECO2、eFuse block revision v0.3
- 16 MiB SPI Flash
- Hex-DDR PSRAM
- ネイティブ走査720×1280のMIPI-DSI LCD
- USB Serial/JTAG

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

- `0x40000020`: アプリケーション記述子とXIP位置調整用パディング
- `0x40001040`: 4 byteのXIP互換セグメント（実行しない）
- `0x4ff40000`: 実行コード、読み取り専用データ、データ、BSS、スタック

アプリケーション記述子は`src/main.rs`の`EspAppDesc`です。

Rust本体はフラッシュから実行せず、2nd-stage bootloaderが内部HP SRAMへロードします。
容量上の理由ではありません。本体は約34 KiB、イメージ全体でも約39 KiBで、16 MiBの
フラッシュにも4 MiBのXIP窓にも収まります。SRAM実行を選ぶ理由は、`src/psram.rs`が
実行中にMSPIのPHY電源とクロックを張り替え、ROMのキャッシュ・MMU操作を呼ぶためです。
ESP-IDFはこの種のコードを`IRAM_ATTR`で内部RAMへ退避しますが、本プロジェクトには
その仕分けがないため、全体をSRAMへ置く方が単純で安全です。GDMA完了ISRの応答時間が
フラッシュキャッシュの状態に依存しなくなる利点もあります。

一方でESP-IDF v5.5.3のESP32-P4ブートローダーは、XIP領域にあるセグメントが
ちょうど2本であることを要求します（`bootloader_utility.c`の`unpack_load_app`、
`assert(rom_index == 2)`）。SRAM実行にすると本来XIPセグメントは記述子だけの1本に
なるため、4 byteの互換セグメントを追加して2本に揃えています。あわせて先頭セグメントの
長さを調整し、次のイメージセグメントの物理アドレスと仮想アドレスの64 KiBページ内
オフセットを一致させることで、`espflash`による余分なパディングセグメントを防ぎます。
これが崩れるとXIPセグメントが3本になり、ビルドは通るのに起動しなくなるため、
`memory.x`の末尾で両方の条件をリンク時に検査しています。

結果としてイメージはXIP 2本とRAMロード1本になり、アプリ本体はフラッシュキャッシュ
を経由せずに実行されます。`.data`先頭には`BOOT_LAYOUT_MARKER`も配置しています。

## RAMの範囲

ECO2ではL2キャッシュがL2MEMの上位から確保されるため、使用できるRAMは
`0x4ffc0000`からキャッシュサイズを引いたアドレスで終わります。下端の
`0x4ff40000`はROM予約領域（ROMスタック`0x4ff3cfc0`、ROMの`.data`/`.bss`が
`0x4ff40000`の直前まで）の上端です。2nd-stage bootloader自身は`0x4ff2cbd0`から
配置されるため、ロード先とは重なりません。

キャッシュサイズを決めるのは2nd-stage bootloaderで、ESP-IDFの既定は128 KiB
（上限`0x4ffa0000`）、ハードウェアのリセット値は256 KiB（上限`0x4ff80000`）です。
リンク時には判別できないため、`memory.x`は両方で安全な`0x4ff80000`を上限として
います。実際の分割は`startup::log_ram_limit`が起動時に出力するので、実機で
128 KiBだと確認できれば`0x00060000`まで広げられます。

なおchip revision v3以降はキャッシュがL2MEMの下位から確保され、上限は
`0x4ffaefc0`になります。この値をECO2に適用してはいけません。

## 起動シーケンス

```text
riscv-rt
  → RTC watchdog停止
  → CPUクロックを90 MHzから360 MHz(CPLL/1)へ引き上げ
  → USB Serial/JTAG初期化
  → L2キャッシュ分割とRAM上限をログ出力
  → PSRAM電源・クロック・DQS調整・MMU割り当て
  → 2面のRGB565コンソール画面を描画してキャッシュを同期
  → LCDリセット・D-PHY・パネル初期化
  → DSI BridgeとDW-GDMAを準備してvideo modeを開始
  → CardKB入力時に変更セルだけを両面へ描画・部分同期
```

正常経路ではDSI HostのVideo Pattern Generatorを使用しません。ECO2では動作中の
VPGからBridge入力へ切り替えると、設定値、FIFO量、underrun状態が同一でも稀に黒画面に
なることを実機で確認したためです。最初のDMAデータをFIFOへ充填してからHostのvideo
modeとBridge出力を初めて有効化します。VPGのカラーバーはPSRAMまたはDMA経路が失敗
した場合の独立した診断表示としてだけ使用します。

## PSRAM

`src/psram.rs`が次の処理を担当します。

1. LDO2を1.8 Vに設定し、MSPI PHYの電源とクロックを有効化
2. 480 MHz SPLLを6分周し、PSRAMを80 MHzで駆動
3. MSPI3経由でモードレジスタを読み書き
4. コマンド経路の読み書き試験
5. DQS位相とdata/DQS delayの実機調整
6. 先頭4 MiBを`0x48000000`へMMU割り当て
7. キャッシュ経由の読み書き試験

1面は720×1280×2 byteで1,843,200 byteです。2面で3,686,400 byteを使用し、
4 MiBの割り当て内に収めています。

DQS調整では、この実機で繰り返し選択された`phase=0, data=0, dqs=0`を最初に
100回読み出して検証します。合格時は31点の全探索を省略し、不合格時だけ従来の
フル探索へ戻ります。高速経路でも各候補に対するESP-IDFと同じ検査回数を使用します。

CPUが描画した内容をGDMAから参照できるよう、転送前にROMの
`Cache_WriteBack_Invalidate_Addr`をL1 DCache、L2 Cacheの順に呼び出します。
その後、両面の既知画素を再読出しし、外部PSRAMへ同期されたことを確認します。

## LCDとパネル初期化

Tab5のLCDリセットはESP32-P4のGPIOへ直結されていません。GPIO31/32上の
ソフトウェアI2CからPI4IOE1を操作し、P4のLCD resetをパルスします。バックライトは
GPIO22です。

D-PHYにはLDO_VO3から2.5 Vを供給し、2レーン、約960 Mbps/laneで動作させます。

確認した本体と箱にはST7123と記載されていますが、タッチコントローラーの
ファームウェア値は`1`でした。M5Stack BSPはこの値に対してST7121互換シーケンスを
選ぶため、本プロジェクトも実機で表示できた`src/lcd/st7121.rs`のシーケンスを
使用しています。

## 映像データ経路

```text
CPU描画
  → PSRAM上のRGB565フレームバッファ
  → L1/L2キャッシュ書き戻し
  → DW-GDMA channel 0
  → DSI Bridge FIFO
  → DSI Host
  → 2-lane D-PHY
  → LCDパネル
```

フレームバッファ入力はRGB565、ネイティブ走査は720×1280です。DSI Hostの
DPIクロックは実クロック80 MHzを使用し、実機で動作確認した水平・垂直
タイミングを設定します。

DW-GDMAは64 bit幅でPSRAMから読み、DSI Bridge FIFOへ固定アドレス書き込みを
行います。1回のブロック転送が1画面全体に対応します。

## フレーム割り込み

ESP32-P4 ECO2のDSI BridgeにはVSync割り込みがありません。ESP-IDF v5.5.3も
ECO2では1画面分のGDMA完了イベントを擬似VSyncとして扱います。本プロジェクトも
同じ方式を採用しています。

GDMA完了ISRは次の最小処理だけを行います。

1. 割り込み状態をクリア
2. 要求されているフレームバッファのアドレスを設定
3. 次の1画面転送を開始
4. 表示中の面とフレーム番号を更新

表示面の変更は転送完了時だけ反映されるため、走査途中でフレームバッファが
切り替わることを防ぎます。

ECO2は初期版CLICを使用します。汎用`riscv-rt`のトラップ入口はCLIC拡張された
`mcause`を保存しないため、`src/interrupts.rs`に専用入口を置いています。この入口は
全整数レジスタ、`mcause`、`mstatus`、`mepc`を保存・復元します。DW-GDMA割り込み
source 24をCPU external line 1、CLIC interrupt 17へ接続しています。

## 描画API

`src/framebuffer.rs`の`DoubleBuffer`は次のRGB565描画処理を提供します。

- `fill`、`draw_pixel`
- `draw_line`
- `fill_rect`、`stroke_rect`
- `fill_circle`、`draw_circle`
- `blit_rgb565`
- `draw_text`

`draw_text`は`src/framebuffer/font.rs`の5×7 ASCIIフォントを使用します。倍率と
前景色、任意の背景色を指定できます。大文字・小文字を区別して表示します。

描画APIの論理解像度は1280×720 Landscapeです。CW回転のため、論理座標を
ネイティブフレームバッファへ次のように変換します。

```text
native_x = logical_y
native_y = 1279 - logical_x
```

DSIの解像度やパネル初期化コマンドは変更せず、全描画プリミティブと画像転送に
同じ座標変換を適用します。

`src/console.rs` は 69 列 × 28 行の固定サイズ端末としてCardKBのASCII入力を保持します。
各行の先頭には半角`"> "`プロンプトを自動で書き込み、Backspaceはプロンプトより前へは
戻りません（5×7 ASCIIフォントのみのため、全角`＞`ではなく半角`>`を使用しています）。
通常キーでは変更された1セルだけを非表示面、表示面の順に描画し、回転後のセルを含む
約26 KiBのPSRAM範囲だけを書き戻します。改行時も新しい行のプロンプト2セルだけを
同様に描画・書き戻し、末尾スクロールが発生したときだけ全画面を再生成します。毎キーの
全画面再描画によるGDMA帯域不足を避けつつ、2面を同じ内容に保ちます。Carriage Return、
Line Feed、Backspace、Tabと末尾スクロールを処理します。

`(column, row)`には疑似カーソル（白いブロック）を表示します。カーソル位置は常に
空セル（次の書き込み位置、またはBackspaceが直前に消した位置）なので、`render_cell`は
そのセルだけカーソルブロックかセル本来の内容かを選んで描画すれば、他のセルに手を
入れずに済みます。文字を描画する側（`draw_ascii_char`）はグリフの前景ピクセルしか
描かないため、直前までカーソルだった空セルにそのまま文字を重ねると背景の白が
残ります。`render_cell`は文字を描く前に必ずセルをBLACKで塗り潰してからグリフを
描画し、この「カーソルの塗り残し」を防いでいます。キー入力のたびに移動前後の
2セルを明示的に再描画するため、Backspace・改行・スクロールでもカーソルの描き残しは
残りません。点滅は`src/lcd.rs`のフレームループがアイドル時に約30フレーム
（パネルの約57 Hzから逆算した約500ms）ごとに切り替えます。

Enterを押すと、プロンプトより後ろに入力された文字列（コマンドライン）を
`Console::submit`が切り出し、`Console`内部の`pending_submission`に保持します。
`src/lcd.rs`は`push()`の戻り値（描画hint、`Update::None/Cells/Full`）とは
独立に、毎キー`Console::take_submission`でこれを取り出して有無を判定します
（コマンド実行はアプリケーション層の反応であって描画hintではないため、
あえて`Update`に混ぜていません）。取り出せた場合は`src/shell.rs`が解析・実行し、
結果は`Console::write_output_line`でプロンプトなしの出力行として書き込まれ、
最後に`Console::write_prompt`で次のプロンプトを出します。コマンド出力は複数行・
スクロールをまたぐことがあるため、この経路では常に全画面を再生成します
（末尾スクロールと同じ理由）。対応コマンドは`help`で一覧できます
（`echo`／`clear`／`about`／`mem`／`uptime`／`backlight on|off`／
`color <name>`／`paint`／`reboot`）。

`shell::execute`の戻り値は`shell::Outcome`（`Continue`／`Reboot`／`Paint`）で、
`reboot`と同様に`paint`もコンソール本体ではなく`lcd::run_console`側の分岐で
処理します。`Paint`が返ると`lcd::run_console`は`paint::run`を呼んでキー入力を
待ち、戻ってきたら`Console::clear`で画面をリセットしてから通常どおり
プロンプトを再描画します。

`reboot`は`src/startup.rs`の`reboot()`が実装しており、HPCPU 0自身の
ソフトウェアリセットビット（`LP_CLKRST_HPCPU_RESET_CTRL0_REG`のbit13、
`HPCORE0_SW_RESET`、write-1-to-trigger）を1回書き込むだけです。これは
ESP-IDFの`esp_restart_noos`（ESP32-P4版）が実際に使っている
`cpu_utility_ll_reset_cpu(0)`と同じレジスタ・同じビットで、ESP-IDFの
`esp32p4/register/soc/lp_clkrst_reg.h`から確認済みです。当初はLPウォッチドッグ
（`init`が無効化しているのと同じ`0x5011_6000`）にstage0=system resetを
仕込んで発火を待つ実装でしたが、実機でリセットされずフリーズしました。
原因は二つあり、(1) `CONFIG0`を丸ごと上書きしていたため`WDT_SYS_RESET_LENGTH`
（既定値から0まで）が短くなりすぎてリセットパルスが伝播しなかった可能性、
(2) そもそもESP-IDF自身もこのウォッチドッグ経路は`esp_cpu_reset()`の背後の
数秒がかりの保険としてのみ使っており、主経路ではありません。現在の実装は
この主経路（HPCORE0_SW_RESET）だけを使っています。

画面上部の見出しには`"Tab5 Console"`を表示します（`render()`内で固定描画）。
見出し帯の色は既定でGREENですが、`color`コマンドで変更できます。

`src/lcd.rs`のCardKB入力ループは、起動シーケンスのUARTログとは別に、キー入力の
たびに発生する診断ログ（キーコード、セル更新完了など）を出力していません。USB
Serial/JTAGへの書き込みはホスト側が読み出していないとFIFOが埋まりタイムアウトまで
スピンするため、これを毎キー実行するとキー入力から描画までの体感遅延が生じます。
エラー系のログ（セル/フラッシュ失敗）のみ残しています。

## タッチ入力とペイント画面

`src/touch.rs`は、Tab5が出荷時期によって搭載しているタッチコントローラーが
異なる問題を吸収します。

- 旧型：GT911単体チップ。ボードI2Cバス（SDA31/SCL32、`lcd.rs`のPI4IOE1と共用）
  上でアドレス`0x5D`と`0x14`の両方をプローブします。GT911のアドレスはリセット時の
  INTピンの状態で決まり、本プロジェクトはそのINTピンを制御しないためです。
- 新型（2025年10月頃以降のロット）：表示ドライバに統合されたSitronix
  ST7121/ST7123が、アドレス`0x55`でタッチも兼務します。実機で確認したのは
  こちらで、GT911は搭載されていませんでした。公開されたレジスタ仕様が
  見当たらなかったため、ESPHomeの`st7123`タッチスクリーンコンポーネント
  （`esphome/components/st7123/touchscreen`）の実装を参照しています。

`Touch::init()`はGT911を先にプローブし、失敗したらST7123にフォールバックします。
どちらも16-bitビッグエンディアンのレジスタアドレッシング（レジスタ番号2byteを
送ってからデータを読み書き）を使うため、`touch.rs`内の`read`/`write`ヘルパーを
共用しています。

ST7123は「設定されたタッチ点数ぶんのレポートテーブル全体を読み切る」ことを
もって次のサンプルをラッチする挙動でした。最初の実装は先頭の1点（7 byte）だけを
読んでいましたが、実機では最初のタッチ以降座標が更新されなくなりました。
`init()`でレジスタ`0x0009`から`max_touches`（最大10）を読み取って保持し、
`poll()`では毎回ヘッダ4 byte＋`max_touches`点ぶん（最大74 byte）を読み切って
から先頭の1点だけを使うことで解消しています。

どちらのコントローラーでも、タッチ座標はまずコントローラー自身のネイティブ
解像度（レジスタから読み取り、0なら720×1280へフォールバック）でスケーリング
し、続けて`framebuffer.rs`の`native_offset`と同じCW回転の逆変換で論理座標
（1280×720 Landscape）に変換します。パネルの物理解像度やDSI側の設定を
変更しない点は描画APIの座標変換と同じです。

`src/paint.rs`はシェルの`paint`コマンドから呼ばれる全画面お絵描きモードです。
`lcd::run_console`のフレームループと同じ「非表示面→表示面の順に描画・書き戻し」
パターンを使い、直前のタッチ点から現在のタッチ点まで`fill_circle`をスタンプ
しながら補間することで線を描きます（`framebuffer.rs`に太線用の新しい
プリミティブは追加していません）。タッチが持ち上げられたら次のタッチを
新しい線の起点として扱います。CardKBの任意のキーを押すとシェルへ戻ります。
タッチコントローラーが見つからない場合もUARTへログを出したうえで、キー入力
だけで抜けられる画面を表示します（`paint::run`はキーボードが存在しなくても
`Option<CardKb>`のまま動作するため、この場合だけ画面から抜ける手段が
なくなる点に注意）。

## ファイル構成

- `src/main.rs`: 起動順だけを定義
- `src/startup.rs`: watchdog停止、CPUクロック引き上げ、L2キャッシュ分割とRAM上限の確認
- `src/uart.rs`: USB Serial/JTAG出力
- `src/psram.rs`: PSRAM、DQS調整、MMU、キャッシュ同期
- `src/framebuffer.rs`: ダブルバッファと描画API
- `src/framebuffer/font.rs`: 5×7フォント
- `src/console.rs`: CardKB入力エコーとコマンドライン切り出し用コンソール
- `src/shell.rs`: `console.rs`から渡されたコマンドラインを解析・実行する簡易シェル
- `src/gpio.rs`: GPIO/IO_MUXのピン単位操作（オープンドレイン設定、low/release/level）
- `src/i2c.rs`: `gpio.rs`の上に実装した汎用ソフトウェアI2C（bit-bang）。SPI等の別インターフェースを追加する場合も同じ構成（`gpio.rs`の上に載せる独立モジュール）に従う
- `src/cardkb.rs`: PORT.AのCardKBドライバ（`i2c.rs`のI2Cバスを使用）
- `src/touch.rs`: GT911／ST7121・ST7123タッチコントローラードライバ（`i2c.rs`のI2Cバスを使用）
- `src/paint.rs`: `paint`コマンドで起動するタッチお絵描き画面
- `src/lcd.rs`: I/O expander（`i2c.rs`のI2Cバスを使用）、D-PHY、パネル、DSI Bridge、DW-GDMA
- `src/lcd/st7121.rs`: パネル初期化コマンド
- `src/interrupts.rs`: CLICトラップ入口とGDMA ISR
- `src/sdmmc.rs`: SDHOSTコントローラー初期化、SDカード活性化、DMA（IDMAC）
  経由のブロック読み書き。`gpio.rs`は使わずIO_MUXを直接操作する点は`psram.rs`と
  同じ構成。詳細・実機で踏んだ罠は[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照
- `memory.x`: ESP32-P4用メモリとイメージ配置
- `.cargo/config.toml`: ターゲット、リンカー、`espflash` runner

`esp-idf-reference/`には、レジスタ設定との比較に使用したESP-IDF v5.5.3版の
参照実装があります。

`src/`以下のコードコメント（`//`・`///`・`//!`）はすべて英語で書きます。
`DESIGN.md`・`README.md`など、この設計資料と人間向けドキュメントは日本語のままです。

各ファイル末尾の`read`/`write`/`modify`（任意の`usize`アドレスを読み書きする
MMIOプリミティブ）は`unsafe fn`として定義します。呼び出し元の`address`が
有効なレジスタである保証はシグネチャからは得られないため、これはRustの
安全性の観点で本来unsafeであるべき操作です。一方、これらを呼び出す各関数
（`enable_dsi_clock`など）は、既知のハードウェア定数アドレスしか渡さない
ことでその安全性を担保するので、`unsafe fn`にはせず、関数内で`unsafe { ... }`
ブロックにまとめて使います（呼び出し1つずつを`unsafe`で囲むのではなく、
関数単位でまとめるのが方針です）。

`README.md`は人間がメンテします。AIは指示された場合を除き編集しないでください。

## 診断ログ

正常時の主要な通過点は次のとおりです。

```text
RAM: L2 cache bytes=0x...
RAM: usable top=0x...
RAM: stack top=0x...
PSRAM: ready for two RGB565 framebuffers
LCD: D-PHY 4/4 ready
LCD: DCS init complete
LCD: DMA 3/3 full-frame interrupt installed
LCD: RGB565 framebuffer DMA active
```

SDカード関連は起動シーケンスに含まれず、シェルコマンド（`sdinfo`/`sdread`/
`sdreadn`/`sdwritetest`/`sdzero`）実行時にのみ`SDMMC: ...`という接頭辞で
UARTへ出ます。正常時は`SDMMC: card activated`の後にCID/CSDの生値が続きます。
失敗パターンの詳細は[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)を参照してください。

主な失敗ログ:

- `CPU: unexpected boot clock source, staying at 90 MHz`: ブートローダーがCPLL/4以外の経路でCPUを構成した（分周比を書き換えず90 MHzのまま継続）
- `RAM: stack top is inside the L2 cache area`: `memory.x`の`RAM`範囲が広すぎる
- `PSRAM: mode-register transaction failed`: MSPI3コマンド経路
- `PSRAM: no valid DQS phase`: DQS位相調整
- `PSRAM: mapped memory test failed`: MMUまたはキャッシュ経路
- `LCD: PI4IOE1 reset control failed`: ソフトウェアI2CまたはI/O expander
- `LCD: D-PHY lock timeout`: D-PHY電源、クロック、PLL
- `LCD: DCS FIFO timeout`: パネルコマンド経路
- `LCD: DMA interrupt error`: DW-GDMA転送

## 既知の問題

末尾スクロール（`Update::Full`）の瞬間、画面の大半が一瞬水色になることがあります。以下をすべて
個別に試しましたが、どれも解消しません。

- 非表示面へ描画してから表示面を切り替える方式（表示中バッファへの直接書き込みをやめる）
- 全画面書き戻しを64 KiBずつのチャンクに分割する
- 書き込み中DW-GDMAチャンネルを停止し、書き込み完了後に再始動する
- 停止から再始動までの間隔を空ける（20ms）

再描画処理そのものを無効化し画面内容を変えないテストでも同じ現象が再現したため、原因は
このコードパスが書き込む内容やPSRAMアクセスのタイミングそのものではなく、`Update::Full`
という処理経路に入ること自体に関連した、まだ特定できていない要因だと考えられます。

調査中にDW-GDMAチャンネルの停止方法に関する別の不具合を発見し、修正済みです。チャンネルが
転送中の場合、`CHEN0`（`DW_GDMA+0x18`）の有効ビットをクリアするだけでは確実に停止せず、
その後の再始動が不安定になります。正しくはESP-IDFの`dw_gdma_ll_channel_abort`と同じく
`CHEN1`（`DW_GDMA+0x1C`）へアボート要求を書き込み、完了をポーリングする必要があります
（この停止方式自体は現在のコードでは使用していません）。

SDHOST（SDMMCコントローラー）にも、ESP-IDFの実ドライバが一度も踏んでいないと
思われる実機固有の制約が2つ見つかっています（詳細と切り分け過程は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)のStage 2/3を参照）。

- `SDHOST_BUFFIFO_REG`へのCPU/APB直接読み出しはポップ動作をしない。
  `STATUS.FIFO_COUNT`はカードからの実データ到着どおりに増え続けるのに、
  固定アドレス・FIFO窓内でのインクリメントアドレスのどちらで読んでも同じ
  ワードが返り続ける。ESP-IDFは常に内蔵DMA（IDMAC）を使っており、この
  CPU直接読み出し経路を検証していないため、ドライバの誤りというより
  この経路自体が実機で機能しないと考えられる。ブロック読み書きは
  すべてIDMAC経由（`sdmmc.rs`の`read_block`/`read_blocks`/`write_blocks`）。
- DMA転送が実際に成功していても（`STATUS.FIFO_EMPTY`が転送後に1へ戻る、
  `RINTSTS`のDTOビットも正しく立つ）、`SDHOST_IDSTS_REG`のRI
  （Receive Interrupt）ビットは実機で一度も立たない。`SDHOST_CTRL_REG`の
  `int_enable`を含め試したが変化しなかった。DMA完了判定は`IDSTS`ではなく
  `RINTSTS.DTO`のポーリングで行っている。

## 制約

- ECO2で確認したレジスタ値とROM APIアドレスを使用しています。
- PSRAMは先頭4 MiBだけを固定アドレスへ割り当て、汎用allocatorには登録しません。
- DSIタイミングとパネルシーケンスは確認したTab5個体向けです。
- 日本語フォント、省電力制御は未実装です。
- SDカードは識別クロック（400kHz）と1bitモードのみ動作確認済みです。
  高速クロックへの切り替え、4bitモード、パーティション/ファイルシステム
  （FAT/exFAT）の解析は未実装です（[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)の
  Stage 4以降）。
