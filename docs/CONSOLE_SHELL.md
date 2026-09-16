# コンソールとシェル

> 索引: [`../DESIGN.md`](../DESIGN.md)

## GUIとの境界

Consoleは上端余白8 pixel、156列×44行を維持し、system barを表示しない。
`win`／`browser`と各専有GUIへの入口がcoordinatorへrouteを返し、通常のcommandはConsole内で完結する。
引数なし`wifi`はサブコマンド一覧を表示し、`help wifi connect`等で詳細を表示する。
Wi-Fi操作は`wifi <subcommand>`へ統合し、旧単独コマンド名は受け付けない。
`battery`／`batinfo`は廃止。引数なし`win`はデスクトップを開く。バッテリー詳細はsystem barから開く。
GUI終了後はデスクトップへ戻り、ConsoleはLauncherから明示的に開く。
GUIから戻った時に進行中のradio操作があれば、競合するWi-Fi／networkコマンドは完了後の再実行を案内する。
詳細と実機未確認の統合経路は[`SYSTEM_BAR.md`](SYSTEM_BAR.md)。

## コンソール

`src/console.rs` は 156 列 × 44 行の固定サイズ端末です。1セルは8×16 pixelで、
16px bitmapフォントの半角glyphそのまま（拡大なし）です。
各行の先頭には半角`"> "`プロンプトを自動で書き込み、Backspaceはプロンプトより前へは
戻りません（コンソールは1文字1セルの半角固定端末なので、全角`＞`ではなく半角`>`を
使用しています）。

### セルは文字ではなく表示glyph ID

セルは`char`ではなく1 byteの表示用glyph ID（`font::console::Id`）を持ちます。
フォント側の契約は[`FONT.md`](FONT.md)にあります。
出力はbyte単位ではなくUnicode scalar単位で走査し、**どのscalarも必ず1セル**へ写します。

- ASCII、Latin-1、半角カナはそのglyph
- 空白は空セル
- それ以外——全角文字、combining mark、フォントに無い文字、制御文字——は
  8×16の可視placeholder

空白へは落としません。空白にすると、そこで文字列が終わったように見えるためです。
LCD上で読めなくても、文字が存在することと、おおよその文字数は分かります。元の
UTF-8は`write_output_line`が従来どおりUARTへそのまま出すので、内容はそちらで
確認できます。この変換はLCD表示だけの都合で、UART側を置き換えません。

コマンド入力はASCIIのままです。`Submission`、引数parser、CardKB／USBキーボードの
入力契約は変えていません。cursor、Backspace、Delete、Home／End、入力行の退避と復元は
常に1セル単位で扱い、全角のlead／continuationやUnicode表示幅の状態はコンソールへ
持ち込みません。

`ls`の複数列配置のように桁を揃える側は、UTF-8のbyte数ではなく
`font::console::cell_count`が返すセル数を使います。

104列×44行の`char`セル配列（18,304 byte）から156列×44行の`u8`セル配列
（6,864 byte）になり、静的RAMは11,388 byte減りました。
CardKB v1.1のEscとカーソル（`0xB5`=↑、`0xB6`=↓、`0xB4`=←、`0xB7`=→）、およびUSB HID
BootキーボードのEsc、カーソル、Home/End、Page Up/Down、Insert/Delete、F1〜F12は`input::Key`へ
正規化します。コンソールではEscで現在行を消去し、Left/Right/Home/EndとDeleteで
現在のコマンド行を編集します。Up/Down、ページ、Insert、Fキー、およびHIDキーボード
から来るCtrl＋英字（`Key::Control`）はイベントとして取得するだけで、コマンド履歴などの
機能が未実装のため現時点では動作を割り当てません。Ctrl＋英字を無視する側もワイルドカードではなく
明示的に列挙してあります——シェルに割り当てが無いキーで行編集に文字が入るほうが、
何も起きないより悪いためです。
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
重ねると背景の白が残るため、`render_cell`は`draw_glyph`に背景色BLACKを
渡し、セル全体を1回で塗り替えて「カーソルの塗り残し」を防いでいます
（カーソルブロック自体は塗り分けのないベタ塗りなので`fill_rect`のままです）。
キー入力のたびに移動前後の
2セルを明示的に再描画するため、Backspace・改行・スクロールでもカーソルの描き残しは
残りません。点滅は`src/app.rs`のフレームループがアイドル時に約30フレーム
ごとに切り替えます。現在の固定リフレッシュレート57.3 Hzでは約500 msに相当し、
実装は`BLINK_INTERVAL_FRAMES = 30`を固定値として持っています。

### 入力途中の行への割り込み出力

`write_output_line`は現在行の0桁目から書き始めるので、**入力途中の行の上には
そのまま出せません**。プロンプトも打ちかけのコマンドも消えてしまいます。
コマンドの出力は常にEnterが入力行を消費した直後、空の行に書かれるのでこれで
問題ありませんが、USBの自動マウント（[FILESYSTEM.md](FILESYSTEM.md)）は
利用者が入力している最中に報告します。

そこで`take_input_line`／`restore_input_line`の対を用意しました。前者は編集中の
行（プロンプトを含む）を退避して現在行を空にし、後者はプロンプトと退避した
内容をカーソル位置ごと書き戻します。復元はセル配列を直接埋めてから`draw_span`
1回で描くので、1文字ずつ`put`へ流し込んで文字数分の書き戻しを払う形にはして
いません。

呼び出しは**最初の1行を出す直前まで遅らせます**。突き合わせの大半は報告する
ことが何も無く（定期再スキャンのたびに1回起きます）、そのたびに入力行を
下ろして戻すとカーソルが理由もなく揺れるためです。

## シェル

Enterを押すと、プロンプトより後ろに入力された文字列（コマンドライン）を
`Console::submit`が切り出し、`Console`内部の`pending_submission`に保持します。
`src/app.rs`は毎キー`Console::take_submission`でこれを取り出して有無を判定します
（コマンド実行はアプリケーション層の反応であって描画の一部ではないため、
コンソール側では扱いません）。取り出せた場合は`src/app/shell.rs`が解析・実行し、
結果は`Console::write_output_line`でプロンプトなしの出力行として書き込まれ、
最後に`Console::write_prompt`で次のプロンプトを出します。どちらも書き込みと
同時に描画・書き戻しまで済ませるため、`shell::execute`にはコンソールと一緒に
フレームバッファを渡します。

### コマンド表が唯一の出典

コマンドは`src/app/shell.rs`の`HELP_ENTRIES`という1本の表で定義します。各項目は
名前、別名、`Group`、`Cmd`、使用法、説明行を持ちます。`execute`は打たれた名前を
`wifi`の場合は先頭2語を`wifi connect`等の名前にまとめ、残りを引数として扱う。
`lookup`でこの表から引き、見つからなければそこで`unknown command`を返します。
見つかった場合は`entry.id`（`Cmd`）で`match`します。

**名前がコマンドになる場所を1箇所にするための構造**です。以前は`execute`の
`match`が`b"..."`のバイト列を直接見ており、表と`match`という2つの一覧が
独立していました。実際に両者は6個ずれていて、`version`・`reset`・`poweroff`・
`batinfo`・`browsertest`・`httpstream`は動くのに`help`から辿れませんでした。

現在は3方向ともコンパイラが検査します。

| ずれ方 | 結果 |
| --- | --- |
| `Cmd`から variant を消し、表と`match`に残す | エラー（`no variant named ...`） |
| `match`のarmだけ消す | エラー（`non-exhaustive patterns`） |
| 表の項目だけ消す | 警告（`variant ... is never constructed`） |

別名は`match`のarmではなく表の`aliases`に持ちます。動く名前は必ず`help`が
説明できる名前になり、上のような「動くのに載っていない名前」は構造的に
作れません。

この形は、将来コマンドを`#[cfg(feature = ...)]`で落とせるようにするための
下地でもあります。`Cmd`のvariant・表の項目・`match`のarmの3箇所に同じ`cfg`が
要りますが、付け忘れは上の表のとおり全部コンパイル時に出ます。

### `help`の2分類

コマンド数が増えて全文表示が長くなったため、引数なしの`help`は名前だけを
列挙します。さらに、名前の羅列に日常的なコマンドが埋もれるため、`Group`で
2つに分けて既定では`Product`だけを出します。

| 指定 | 出るもの |
| --- | --- |
| `help` | `Group::Product`の41個 |
| `help all` | 製品と足場（`Group::Scaffold`の57個）を見出し付きで |
| `help <name>` | 使用法、別名があれば`also:`行、説明 |

`all`はコマンド名より先に判定します。この語をコマンド名に使うと、そのコマンドは
`help`から説明できなくなります。

分け方の軸は「誰に有用か」ではなく「**いつ不要になるか**」です。足場は実装が
安定するまでの診断用で、安定すれば大半は不要になります。外から見れば、
調停レジスタを読むコマンドも表示を2時間焼くコマンドも同じくらい使い道が
ありません。両者を分けるのは、それを書かせた作業より長く生き残るかどうかです。

各コマンドの割り当てと、足場を生かしている作業への紐づけは
[`CONSOLE_COMMAND_REVIEW.md`](CONSOLE_COMMAND_REVIEW.md)、削除計画は
[`COMMAND_RETIREMENT_PLAN.md`](plans/proposed/COMMAND_RETIREMENT_PLAN.md)にあります。

### 引数の区切りとダブルクォート

コマンド名は最初の空白で切り出します。引数側は`split_argument`が1個ずつ切り出し、
**ダブルクォートで囲まれた引数は中の空白を含めて1個**として扱います。

```text
cat "/tmp/Hello World.txt"
```

ファイル名に空白が入り得るためです。FATの長い名前は昔から空白を許しており、
引数を2つ取るコマンド（将来のコピーなど）では、位置だけを見てどこで1つ目が
終わるかを判断する規則が作れません。クォートがそれを言う役目を持ちます。

クォート内にエスケープはありません。閉じクォートで終わりです。`"`はパス要素として
拒否される文字なので（[FILESYSTEM.md](FILESYSTEM.md)）、ファイル名がエスケープを
必要とする文字を含むことがないためです。

クォートを開いて閉じなかった場合は`unterminated quote`で拒否します。残りを丸ごと
1引数とみなす方が親切に見えますが、閉じ忘れならそれこそが誤りです。

引数を1つしか取らないコマンドは、余った文字列を無視せず
`unexpected extra argument; quote paths containing spaces`で拒否します。
たいていの原因はクォートし忘れたパスで、無視すると切れた前半をファイルシステムへ
渡して「そんなファイルは無い」と報告することになり、原因から遠ざかるためです。

### オプション（フラグ）

`ls`と`mount`がオプションを取ります。規則は`ls`が決めたものをそのまま他の
コマンドへ広げた形です。

- フラグはパスより**前**に置きます
- `-`と1文字以上の英字で1個。`-la`は`-l -a`と同じです
- **最初のフラグでない語より後ろは、フラグとして読みません**。これにより
  `-`で始まる名前のファイルはクォートすれば普通に渡せます
- 知らない文字は`ls: unknown option -x`と拒否します。usage行だけを出すと、
  どの1文字が拒否されたのかを読み手が探すことになるためです
- `-`単体はフラグではなくパスとして扱います

`mount [-r] [<volume>]`の`-r`はFATボリュームを読み取り専用でマウントします。
引数なしはマウント一覧、ボリューム名だけなら形式に従った既定
（FATは読み書き、exFATは読み取り専用）です。逆向きのフラグ——exFATを読み書きに
する——はありません。VFSが果たせない要求で、用意すれば使う時点で断ることに
なるためです（[FILESYSTEM.md](FILESYSTEM.md)）。成功行は
`mounted sd0p1 on /vol/sd0p1 (read-write)`のように、確定したモードまで出します。

### カレントディレクトリとパス引数の解決

シェルは`shell::State`にカレントディレクトリ（`fs::path::Path`）を1つ持ちます。
VFSは持ちません。VFSに入るパスが正規化済みの絶対パスであるという不変条件を
保つためと、利用者が複数になったときに「誰のカレントディレクトリか」を
決めずに済ませるためです（[FILESYSTEM.md](FILESYSTEM.md)）。

パス引数を取るコマンドは`split_argument`の直後に`absolute`を通します。
`/`で始まらない引数はカレントディレクトリと連結し、`fs::path::join`が
正規化して絶対パスにします。`.`と`..`はここで畳まれるので、相対パス用の
解決規則は別に存在しません。解決は1箇所だけなので、`ls`／`cat`／`write`／
`append`／`mkdir`／`umount`のどれでも相対パスの意味は同じです。

連結後の長さが`MAX_PATH_BYTES`（255 byte）を超えると`path too long`で
断ります。`..`で短くなる場合も同様です。`normalize`が入力長を先に測るのと
同じ理由で、そこを緩めても拒否の位置が変わるだけだからです。

`cd`は移動先を`Vfs::metadata`で確かめてからカレントディレクトリを更新します。
存在しない場所やファイルへは移動できません。引数なしの`cd`は`/`へ戻ります
（ホームディレクトリの概念が無く、`/`は何がマウントされていても必ずあるため）。
現在地は`pwd`で表示します。**プロンプトには出しません。**`Console`の`PROMPT`は
固定長で、`MAX_LINE`もカーソルの左端も行編集もその長さに依存しているためです。

カレントディレクトリは文字列であって、ボリュームを掴んでいるわけではありません。
指しているボリュームを`umount`しても、媒体が入れ替わっても、カレントディレクトリは
そのままです。以後のコマンドは`no filesystem mounted on that path`で失敗し、
同じ場所にマウントし直せばまた通ります。`/`へ戻す案を採らなかったのは、
`umount`と媒体チェック（と将来の自動アンマウント）の全経路がシェルの状態を
書き換えに行くことになるためです。

### 診断・受入試験コマンド

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
coordinate chart、font sheet、paint、multi-touch、axis、desktopを順に開きます。各画面で指示された
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
約1秒のtimeout後に、安全な1 packet QTDを同一DATA PIDで再投入した回数、
`command_retries`はBOT Reset Recovery後に
READ(10)を再送した回数です。`failures`は再送しても失敗した回数、`mismatch`は読出し自体は完了したが
内容が変わった回数です。4 KiBのREAD data phaseはendpoint MPS単位のQTDへ分割します。開始時の
`host=... bulk-in-mps=... fifo=RX/NPTX/PTX`で、速度切替後に実際のendpoint MPSで再列挙された
ことと、root-port reset後に再適用されたDWC FIFOの実レジスタ値を確認できます。
ESP32-P4のESP-IDF balanced値は`fifo=512/256/128`です。

USB BOT/HCDの受入試験は接続構成ごとに`usbcheck [reads] [lba]`を1回実行します
（[`USB_BOT_HCD_REFACTOR_PLAN.md`](plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md)）。前後のcounterを自分で
採り、read soakと、LBAを指定した場合は同じLBAへのwrite roundを10回実行し、最後に
**差分**とGo条件ごとのPASS/FAILを出します。LBAを省略するとread専用で、mediaへは
書き込みません。10回なのはStage 0のmatrixがその回数で、Full-Speedハブ経路の復元WRITEが
10回中8回失敗したのを捕らえた回数だからです。USB raw WRITEが不安定な間はfilesystem側の
`fswritetest`を受入に使いません。`usbrawcheck <lba> [writes] [span] [gap_ms]`がfilesystem外の犠牲範囲へ
READを挟まず単一block WRITEを発行し、最後にまとめて照合・復元します。既定は1 blockへ
32 WRITE、spanは1〜8、gapは成功したWRITE間の0〜2000 ms（既定0）です。失敗で復元できなくてもfile／directoryは残らず、`usbrescan`後に
同じ犠牲範囲で次の試験を続けられます。

以前はこれを`usbhw`→`ut 100`→`usbwritetest`×10→`usbhw`と打ち、2つの10行blockを目視で
突き合わせていました。counterの絶対値には起動時の列挙とidle HIDのpollが全部乗っているので、
差分にしないと比較になりません。構成と媒体の組み合わせだけ繰り返す作業なので、
差分と判定をコマンド側に持たせています。

`usbcachefail`はDMA cache同期の拒否をdata IN phaseへ注入し、その転送がchannelをarmする前に
失敗すること、宛先bufferへ1 byteも公開されないことを確認します。同じ形で古い世代の完了、
短いOUT、FIFO flush timeoutも注入し、合わせて4つの契約を確認します。正常なhardwareはどれも
起こさないので、注入以外にこれらの経路へ到達する方法がありません。ドライバ自身の論理の試験
なので接続構成ごとに繰り返す必要はなく、全体で1回実行すれば足ります。注入は自分で減るcounterで、
1回ごとに1消費され、終了時に残りを解除します——armされたまま残る状態は作れません。最初の3つは
2回連続の失敗になるためMSC sessionは設計どおり使用不能になります（FIFO flush timeoutの回だけは
commandが始まらないのでsessionは残ります）。各注入の間は最大3回のfull rescanで
MSCを再取得し、最初のdescriptor要求だけが失敗しても残りの検査を飛ばしません。全注入の終了後は
`usbrescan`してください。

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

ファイルシステム書き込み経路の受入試験は`fswritetest <dir> [rounds] [KiB]`だけで
実行します。**媒体へ書きます。** 作るものは全て`<dir>/FSWTEST`の中で、実行後に
消します。その名前が既にある場合は何もせず中止するので、既存のファイルを
上書きすることはありません。手順は[FILESYSTEM.md](FILESYSTEM.md)の
「書き込み経路の受入試験」にあります。

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

`httpget`はhostだけを渡すと従来どおり平文です。TLSにするには`https://`から
始まるURLを明示します。コマンド名だけで暗号化の有無を変えることはありません。
`hs`もURLのschemeでtransportを選びます。どちらのhttps取得も**未認証**です
（[BROWSER.md](BROWSER.md)）。

`tls <host>[:port] [path]`はTLS 1.3接続を1回張り、その上で1ページ取得して、
handshakeが何を証明したかを報告します。既定portは443、既定pathは`/`です。
接続は**未認証**で、serverの`CertificateVerify`とFinishedは提示された鍵に対して
検証しますが、その鍵がこのhostのものかは確認しません（[NETWORK.md](NETWORK.md)）。
表示は`TLS UNVERIFIED`で、`SECURE`にはなりません。本文は数えて捨てます。

報告するのはhandshake時間、総時間、poll回数、そして**1回のpollの最長時間**です。
最後の値がブラウザのframe loopが実際に感じる停止時間で、署名検証はpollの内側の
分割できない処理なので、ここが100 msを超えるかどうかが
[TLS_PLAN.md](plans/archive/TLS_PLAN.md)の中止条件になります。

`entropy`はTLSがCSPRNGの種に使うSAR ADCノイズ源を検査します。引数なしで1回種を取り、
32 byteと有効化／停止の累計回数を表示します。`entropy test [count]`は既定100回、
「ノイズ源を立ち上げる→CSPRNGへ種を入れる→32 byte引く」というTLS接続と同じ経路を
繰り返し、(1)有効化と停止の回数が必ず一致すること、(2)連続する2回が同じ32 byteを
出さないことを確認します。統計的なランダム性検定は行いません。真性であることを
証明できない一方、同じ値の再出現は真性でないことを証明するからです。

`entropy fail on`はハードウェアに触れずに種の取得を失敗させるテストフックです。
真性乱数が用意できないとき呼び出し側が1 packetも送らないことを確認するために使い、
`entropy fail off`で戻します（[TLS_PLAN.md](plans/archive/TLS_PLAN.md) Stage 2）。

## 再起動

通常GUIからはLauncherのPower... → Rebootで同じ処理を呼ぶ。入力・資源解放の境界は
[`SYSTEM_BAR.md`](SYSTEM_BAR.md)を参照。GUI経由の動作は実機未確認。

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

**ESP32-C6も同じ理由で後始末が要ります。** C6は別チップで、電源をゲートする
I2Cエクスパンダごとリセットされないため、**アクセスポイントにアソシエートした
まま再起動をまたいで生き続けます**。そして次に`sdio::init`がリセット線を
叩いた瞬間、アソシエート中に何も告げずに消えます。APには「応答しなくなった
ステーション」のエントリが残り、**その無通信タイムアウトが次のアソシエーション
に降ってくる**ので、再起動後の最初の`wifi connect`が`reason 4
DISASSOC_DUE_TO_INACTIVITY`で失敗し、2回目は成功する、という形で現れます。
対処は2つで、役割が違います。

- `shell::reboot`がリセット前に`wifi::station::disconnect`を投げます。
  deauthが1フレーム飛べばAPはエントリを即座に落とすので、**亡霊がそもそも
  作られません**。ただしP4側にセッションがある場合しか投げられません
- ブート経路が`sdio::power_down_c6`でC6の電源を落とします。エントリは
  作られてしまいますが、**作られる時刻が「操作者が接続を頼んだとき」から
  「起動したとき」へ前倒しになる**ので、APの無通信タイマが先に回収できます。
  セッションが無い経路（Wi-Fiに触らず再起動した次の起動）はこちらが受けます。
  リセット線を先にLowにしてから電源を切るのは、Highのまま電源だけ抜くと
  保護ダイオード経由でチップを食わせてしまうためで、ついでに電源復帰時は
  リセットを保持したままレールが立ち上がる望ましい順序になります

後者は競合を消すのではなく前倒しするだけです。起動直後に`wifi connect`を
打てば同じことが起きえます。確実に消せるのは前者だけです。

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

通常GUIからはLauncherのPower... → Shutdownで同じ処理を呼ぶ。GUI経由では電源断処理が
戻った際に結果を表示し、キー入力後にConsoleへ戻る。

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
RAMディスクのformat、初期ツリー作成、`/` mountがすべて成功した場合、容量や初期化時間は
ここへ表示しません。容量は明示的な`mem`コマンドで確認できます。RAMルートを確立できない
場合はUARTへ原因を出してpanicし、プロンプトへ進みません。

`src/app.rs`のInputManager経由のキー入力ループは、起動シーケンスのUARTログとは別に、キー入力の
たびに発生する診断ログ（キーコード、セル更新完了など）を出力していません。USB
Serial/JTAGへの書き込みはホスト側が読み出していないとFIFOが埋まりタイムアウトまで
スピンするため、これを毎キー実行するとキー入力から描画までの体感遅延が生じます。
エラー系のログ（セル/フラッシュ失敗）のみ残しています。
