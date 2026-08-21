# コンソールとシェル

> 索引: [`../DESIGN.md`](../DESIGN.md)

## コンソール

`src/console.rs` は 104 列 × 44 行の固定サイズ端末としてキーボードからのASCII入力を保持します。
各行の先頭には半角`"> "`プロンプトを自動で書き込み、Backspaceはプロンプトより前へは
戻りません（5×7 ASCIIフォントのみのため、全角`＞`ではなく半角`>`を使用しています）。
CardKB v1.1のEscとカーソル（`0xB5`=↑、`0xB6`=↓、`0xB4`=←、`0xB7`=→）、およびUSB HID
BootキーボードのEsc、カーソル、Home/End、Page Up/Down、Insert/Delete、F1〜F12は`input::Key`へ
正規化します。コンソールではEscで現在行を消去し、Left/Right/Home/EndとDeleteで
現在のコマンド行を編集します。Up/Down、ページ、Insert、Fキーはイベントとして取得するだけで、
コマンド履歴などの機能が未実装のため現時点では動作を割り当てません。
Carriage Return、Line Feed、Backspace、Tabと末尾スクロールを処理します。

**セル配列が状態、フレームバッファはその表示**という分担にしています。
セルを変更する`Console`のメソッドは、戻る前に必ず変更したセルを描画し、
その範囲を書き戻します。呼び出し側が後から`flush`する必要はなく、
「描いたが書き戻していない」中間状態も存在しません。ダブルバッファ時代の
描画hint（`Update`）と変更行の記録（`damage`）、および`src/app.rs`側の
遅延再描画は、この分担に置き換えて削除しました。

書き戻しは1行の中の**列範囲**単位です。通常キーでは移動前後のカーソルセルを
含む数セル分、改行では新しい行のプロンプト2セル分、出力行では実際に文字が
入った列までを、それぞれ`flush_rect`1回で書き戻します。CW回転により論理Xの
連続範囲はネイティブ行の連続範囲へ写るため、列範囲での書き戻しは連続した
PSRAM範囲1本になります（逆に、1行の全列を書き戻すとフレームバッファの
ほぼ全体を含んでしまいます）。

セル配列が画面の下でずれる変更——末尾スクロール、`clear`、全画面モードからの
復帰——だけは列範囲で表せないため、セル配列を先に確定させてから全画面を
再生成します。これはこのファームウェアで最も重いPSRAMバーストなので、
上記の各経路はここへ落ちる条件を絞っています。逆に、末尾までスクロールした
状態で複数行を出力するコマンドは、行ごとに全画面再生成を払います。
アンダーランが再発する場合はここが原因なので、表示DMAの`icm`調停優先度と、
全画面塗り・スクロールがPPA／2D-DMA経路を通っていることを確認します。キャッシュ
マスターのレート制限は実機で効果がなかったため使用しません
（[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)を参照）。

`(column, row)`には疑似カーソル（白いブロック）を表示します。カーソル位置は常に
空セル（次の書き込み位置、またはBackspaceが直前に消した位置）なので、`render_cell`は
そのセルだけカーソルブロックかセル本来の内容かを選んで描画すれば、他のセルに手を
入れずに済みます。直前までカーソルだった空セルにグリフの前景ピクセルだけを
重ねると背景の白が残るため、`render_cell`は`draw_ascii_char`に背景色BLACKを
渡し、セル全体を1回で塗り替えて「カーソルの塗り残し」を防いでいます
（カーソルブロック自体は塗り分けのないベタ塗りなので`fill_rect`のままです）。
キー入力のたびに移動前後の
2セルを明示的に再描画するため、Backspace・改行・スクロールでもカーソルの描き残しは
残りません。点滅は`src/app.rs`のフレームループがアイドル時に約30フレーム
ごとに切り替えます。現在の固定リフレッシュレート57.3 Hzでは約500 msに相当し、
実装は`BLINK_INTERVAL_FRAMES = 30`を固定値として持っています。

## シェル

Enterを押すと、プロンプトより後ろに入力された文字列（コマンドライン）を
`Console::submit`が切り出し、`Console`内部の`pending_submission`に保持します。
`src/app.rs`は毎キー`Console::take_submission`でこれを取り出して有無を判定します
（コマンド実行はアプリケーション層の反応であって描画の一部ではないため、
コンソール側では扱いません）。取り出せた場合は`src/app/shell.rs`が解析・実行し、
結果は`Console::write_output_line`でプロンプトなしの出力行として書き込まれ、
最後に`Console::write_prompt`で次のプロンプトを出します。どちらも書き込みと
同時に描画・書き戻しまで済ませるため、`shell::execute`にはコンソールと一緒に
フレームバッファを渡します。対応コマンドは`help`で一覧できます。コマンド数が
増えて全文表示が長くなったため、引数なしの`help`はコマンド名だけを列挙し、
`help <name>`で個別コマンドの使用法と説明を表示する二段構成にしています
（`src/app/shell.rs`の`HELP_ENTRIES`）。

表示帯域の診断には`displaybench <mode> [count] [phase_ms] [burst]`を使います。`mode`は
`idle`、`sync`、`cpu`、`ppa-raw`、`ppa-safe`、`production`のいずれかです。描画系の
各操作はframe境界の0/3/8/12 ms後から開始でき、DMA2D burstは8/16/32/64/128 byteを
一時指定できます。コマンドは終了時にproductionのburstへ戻し、操作ごとにBridgeの
sticky underrun bitを消費します。`ppa-raw`だけは開始前に全走査面を一度
writeback-invalidateし、測定中はCPUが画素へ触れないことでcache整合を保ちます。
出力は指定回数、完了回数、経過frame、1操作の平均µs、underrunした操作数です。

標準比較は短い別名`db [count]`だけで実行できます。省略時は100回で、ICMを15/15へ設定し、
上記6 mode、PPAの4開始位相、DMA2Dの5 burst（重複する基準caseは1回）からなる13 caseを
画面再描画を挟まず連続実行し、最後に一覧表示します。個別条件を再測定するときだけ長い
`displaybench`形式を使います。全画面の試験色はBLACKとREDを交互に使います。以前はBLUEを
使っていたため、正常な濃青frameとBridge underrunの水色を混同しやすい状態でした。

production既定値だけを受入試験するときは`dp [count]`を使います。省略時は100回で、
phase 0 ms、DMA2D burst 128 byte、ICM 15/15だけを実行します。`db`に含まれる意図的に
厳しいshort-burst caseを省くため、通常構成の合否だけを短時間で確認できます。

30分のidle走査受入試験は`di [minutes]`で実行します。省略時は30分で、実測57.3 Hzを
切り上げた1分3,440 frame、合計103,200 frameを待ち、各frameでsticky underrunを回収します。
開始時にICMを15/15へ設定し、終了時に完了frame数とunderrun数を表示します。任意時間を
指定する場合は1〜120分です。

画面遷移の受入試験は`ui`だけで開始します。最初に通常のconsole cell配列を使って画面を
埋め、実際のDMA2D scroll＋露出行再描画を100回繰り返し、各操作後のunderrunを回収します。
試験用行はUARTへmirrorせず、serial出力のbackpressureを表示負荷へ混ぜません。その後、
coordinate chart、paint、multi-touch、axis、desktopを順に開きます。各画面で指示された
操作を行い、任意キーを押すと次へ進みます。各画面の初期描画とconsoleへの復帰後にsticky
underrunを回収し、最後にvisual全体のunderrun数とDMA errorを表示します。

最終複合試験はmicroSDとUSB Mass Storageを挿した状態で`mix [minutes]`を実行します。
省略時は120分です。走査を継続しながら約1秒ごとにBLACK/REDのproduction全画面fill、
microSDとUSB MSCのLBA 0から各4 KiB readと初回内容との比較を行います。それとは別に毎loop、
PSRAM heapへ確保した4 MiB内の4 KiB stripeを順に書き、writeback-invalidate後に全byteを
再検証します。外部mediaへのwrite commandは発行しません。終了時は経過frame、storage I/O
回数、heap検査回数、USB QTD／READ(10)のRecovery再送数、root-port再列挙数、underrun、
DMA errorとPASS/FAILを表示します。BOT Reset Recoveryまで失敗した場合はUSBバスを最大3回
再列挙し、新しいsessionでLBA 0を読み直します。試験開始時の4 KiBと完全一致した場合だけ継続し、
違うmediaまたはdata corruptionは`USB data mismatch`で即時FAILにします。起動時のHub列挙でMSCを
取得できなかった場合も、`mix`自身がroot portを最大3回同期的に再列挙します。MSCのready確認と
基準4 KiBの読出しが成功するまでは計測を開始しません。通常再列挙を使い切った場合はUSB-A VBUSを
1秒offにしてHubと全downstream deviceを一度だけpower-cycleし、再列挙します。同じ試験中の2回目の
power-cycleが必要になった場合は不安定と判定してFAILにします。結果の`power_cycles`で回数を確認できます。

USBだけを先に短く確認するときは`ut [count]`を使います。省略時は100回で、LBA 0の同じ
4 KiBを初回内容と比較します。外部mediaへは書き込みません。`packet_retries`はstatus 1または
約1秒のtimeout後に、同一packetまたは複数packet QTDの未受信suffixを再投入した回数、
`command_retries`はBOT Reset Recovery後に
READ(10)を再送した回数です。`failures`は再送しても失敗した回数、`mismatch`は読出し自体は完了したが
内容が変わった回数です。4 KiBのデータフェーズ自体は1 QTDへまとめて実行します。開始時の
`host=... bulk-in-mps=...`で、速度切替後に実際のendpoint MPSで再列挙されたことも確認できます。

`pf`は次の1 bootだけ、200 MHzの有効なDQS選定後に診断用失敗を注入します。200 MHzで
mode registerとDQSまで設定した状態から、MSPI resetと80 MHz profileの再設定で復旧できる
ことを確認するコマンドです。markerは起動時に消費するため、その次の通常`reboot`では再び
200 MHzを試します。

`rt [count]`は再起動耐久試験を1回の入力で実行します。既定は20回、上限は100回です。
LP scratch registerのSTORE13/14にmagic、総数、残数を保持し、各bootで200 MHz PSRAM初期化、
post-PSRAM DROM/IROM probe、heap初期化、display scanout開始まで到達して初めて1回を合格として
減算します。途中bootはUARTへcompleted/remainingを出して自動再起動し、最終bootはscratchを
消去して画面とUARTに`REBOOT TEST PASS: count/count`を出し、通常のプロンプトで停止します。
途中で80 MHzへfallbackした場合はそのbootを数えず、scratchを消去して`FAIL`で停止します。
電源断はLP scratchを消去するため、試験中止手段にもなります。

`ppafill ... cpu`と`ppafill sweep`のCPU側は診断専用のraw CPU経路を直接呼びます。
productionの`fill_rect`は768画素以上をPPAへ自動転送するため、診断がこの公開APIを
通ると大矩形のCPU測定にならないためです。

`shell::execute`の戻り値は`shell::Outcome`（`Continue`／`Reboot`／`Shutdown`／`Paint`／
`TouchTest`／`CoordTest`／`AxisTest`／`Battery`／`Win`）で、全画面サブアプリはコンソール本体ではなく
`app::run`側の分岐で処理します。各サブアプリが戻った後は`Console::clear`で
画面をリセットしてから通常どおりプロンプトを再描画します。

`pma`はESP32-P4の16本の`pmacfgN`／`pmaaddrN` CSRを読み、PMAの属性付きメモリマップを
表示します。範囲は終端を含まない`[start,end)`で、TOR・NA4・NAPOTをアドレスへ復元し、
R/W/X、有効（E）、ロック（L）、キャッシュ属性（WB=write-back、WT=write-through、
NC=non-cacheable、WNA/RNA=write/read miss no-allocate）、生の設定語を併記します。mode=OFFの
エントリは自身の範囲を持たない一方、`pmaaddrN`が次のTORエントリの下限になるため、`off@`として
そのアンカーアドレスを残して表示します。CSRは読み出すだけで、ブートローダーがロックしたPMA設定を
変更しません。

`pmp`は標準のRISC-V PMP（Physical Memory Protection）を同じ形式で表示します。
ESP32-P4のPMPも16エントリですが、設定バイトは`pmpcfg0`〜`pmpcfg3`の4本のCSRに
4エントリずつ詰め込まれている点がPMA（1エントリ1 CSR）と違います。アドレスは
`pmpaddr0`〜`pmpaddr15`で、範囲の復元（TOR・NA4・NAPOT、`off@`表示）は`pma`と同じです。
表示するのはR/W/Xとロック（L）、生の設定バイトで、PMAと違ってキャッシュ属性はありません。
PMAが「そのアドレスがどう振る舞うか」を決めるのに対し、PMPは「誰が読み書き実行してよいか」を
決めます。読むときの注意が2つあります。1つはエントリに優先順位があること（最も番号の小さい
一致エントリが勝つので、後ろのエントリが前のエントリと重なった部分は死んでいます）、
もう1つは本ファームウェアが常に動いているマシンモードではロック（L）の立っていない
エントリが無視され、どのエントリにも一致しないアドレスは許可されることです。末尾の行に
粒度（ESP-IDFの`SOC_CPU_PMP_REGION_GRANULARITY`と同じ128バイト。4バイト単位のNA4は
このため使えません）とこのマシンモードの規則を出します。TORの上限が前エントリの下限以下で
何にも一致しないエントリには`empty`を付けます。`pma`と同様にCSRは読むだけです。

## 再起動

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

リセットされるのはHP CPUコアだけで、**周辺回路は動き続けます**。とくにDW-GDMAは
前のブートのフレームバッファをPSRAMから読み続けたまま、次のブートの
ブートローダー実行と`psram::init`（MSPIコントローラーのリセットとDQS再調整）に
突入します。そのため`shell::reboot`とブート経路の両方で`lcd::quiesce_dma`を
呼び、チャンネルを`CHEN1`（`DW_GDMA+0x1C`）のアボート要求で止めてから
ブロックをリセットし、あわせて`icm`の調停優先度も既定値へ戻します。

この後始末は以前から必要でしたが、DW-GDMAの優先度をCPUより高くするまでは
表面化していませんでした。優先度を上げた状態でこれを怠ると、PSRAM再初期化中の
CPUアクセスがDMAに負け、`reboot`後に画面が出ずフリーズする、あるいは
PSRAMが壊れた状態で起動してヒープ確保時にPANICする、という形で現れます。
`CHEN0`の有効ビットを落とすだけでは転送中のチャンネルは確実に止まらないため、
アボート要求と完了ポーリングが必要です
（[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md)も参照）。
ブート経路側は、クロックゲートとリセット状態を先に読んで「一度も動いていない
（＝コールドブート）」場合は何もしません。ECO2ではクロックが止まっている
ブロックのレジスタを読むとバスアクセスが返らないためです。

## 全体電源断

`shutdown`（別名`poweroff`）はTab5全体の電源断を要求する引数なしのシェルコマンドです。
メディアへの書き込みなど、アプリケーション側で必要な保存処理を完了してから実行する必要が
あります。コマンドは`src/app/shell.rs`で`Outcome::Shutdown`を返し、`src/app.rs`は
`"shutting down..."`をシングルフレームバッファへ描画・同期してから300 ms待機します。このため、
電源断が即時に行われてもユーザーは操作受付を画面で確認できます。

実際の電源断要求は`src/power.rs`が担当します。ボードI2C（SDA31/SCL32）上の2個目の
PI4IOE5V6408（E2、アドレス`0x44`）のP4は、電源制御回路の`PWROFF_PULSE`へ接続されて
います。P4を出力・非ハイインピーダンスに設定した上で、high 100 ms／low 100 msのパルスを
3回送ります。これはTab5公式ファームウェアの電源断シーケンスと同じ回数・幅です。

E2はUSB-A VBUS、Wi-Fi、充電などの別制御線も共用するため、`src/usb/hcd.rs`の
`set_pi4ioe2_output_bit`は、方向・ハイインピーダンス・出力値の各レジスタを対象ビットだけ
read-modify-writeします。電源断シーケンス中にI2C書込みが一つでも失敗した場合、
`power::shutdown()`は`false`を返し、`app::run`は電源断失敗を表示してシェルを継続します。
正常時は最初のパルス途中で電源が落ちることがあるため、関数が戻ることは保証しません。

起動直後のコンソールには暫定バージョン表記の`"Tab5 Shell 0.1"`を通常の出力行として
表示してからプロンプトを表示します。これはまだ正式なバージョン定義ではありません。

`src/app.rs`のInputManager経由のキー入力ループは、起動シーケンスのUARTログとは別に、キー入力の
たびに発生する診断ログ（キーコード、セル更新完了など）を出力していません。USB
Serial/JTAGへの書き込みはホスト側が読み出していないとFIFOが埋まりタイムアウトまで
スピンするため、これを毎キー実行するとキー入力から描画までの体感遅延が生じます。
エラー系のログ（セル/フラッシュ失敗）のみ残しています。
