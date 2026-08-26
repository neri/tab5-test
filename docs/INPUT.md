# 入力（キーボード・ポインタ・タッチ）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`INPUT_MANAGER_PLAN.md`](INPUT_MANAGER_PLAN.md)、
> [`SOFT_I2C_REFACTOR_PLAN.md`](SOFT_I2C_REFACTOR_PLAN.md)

## ソフトウェアI2C

`src/i2c.rs`は`src/gpio.rs`の上に実装したbit-bangのI2Cマスターです。物理バス
ごとに一つの`SoftI2c`を静的に持ちます。

- ボードI2Cバス（SDA31/SCL32）: PI4IOE1／PI4IOE2、タッチ、BMI270、INA226、
  RX8130CEが共用します。起動時に一度だけ初期化とバス復旧を行います。
- CardKBコネクタのバス（GPIO53/54）: PORT.A専用で、入力の初期化時に一度だけ
  初期化します。
- Tab5 Keyboardのバス（GPIO0/1）: Ext.Port1専用で、入力の初期化時に一度だけ
  初期化します。

ビット当たりの遅延（`delay_us`）とSCLストレッチ待ちの上限
（`scl_wait_iterations`）はバスごとに`SoftI2c::new`で与えます。

通常の呼び出し側は`write`／`read`／`write_read`を使います。これらはアドレス
バイト、repeated START、読み出し最後のNACK、STOPまでを含む1トランザクションを
所有します。読み出し長がトランザクション中にしか分からないプロトコルだけが
`transaction`（クロージャ型の逐次API）を使います。

## キーボード

- CardKB v1.1（`src/cardkb.rs`）: I2Cアドレス`0x5F`。読み出しでキーを1バイト
  返します。カーソルキーはCardKB固有の非ASCIIバイト（`0xB5`=↑、`0xB6`=↓、
  `0xB4`=←、`0xB7`=→）です。バスの両線がアイドルhighのときだけ初期化に成功
  するので、未接続でも無害です。
- Tab5 Keyboard（`src/tab5_keyboard.rs`）: Ext.Port1のGPIO0（SDA）／GPIO1（SCL）
  に接続する70キー専用キーボードです。I2Cアドレス`0x6D`でファームウェア版を
  プローブしてからHIDモードへ切り替え、HID modifier／usage IDのイベントキューを
  読みます。キーリリース（usage ID 0）と未対応usageはドライバ内で消費します。
  GPIO50のactive-low INTは使わず、フレームごとのポーリングでキューを読みます。
- USB HID Bootキーボード（`src/usb/hid_keyboard.rs`）: 詳細は
  [`USB.md`](USB.md)。

3種とも`src/input.rs`の`Key`へ正規化します。Tab5 KeyboardとUSBは同じHID usage
ID変換を共有します。`Key`は`Ascii(u8)`、`Control(u8)`と、Escape、
カーソル4方向、Home/End、PageUp/PageDown、Insert、Delete、`Function(u8)`を
持ちます。入力を受け取る側は、キーがどのキーボードから来たかを知る必要が
ありません（`KeyEvent`の`source`で区別できますが、コンソールは使いません）。
コンソールがどのキーに何を割り当てているかは
[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)を参照してください。

### Ctrlは制御コードではなく`Control(letter)`

`Control(u8)`が持つのは小文字の英字そのもので、その英字が表すC0制御コードでは
ありません。制御コードには既に別のキーの席があり——`Ctrl+H`は0x08、`Ctrl+I`は
0x09、`Ctrl+M`は0x0D——`Ascii`で流すと、`Ctrl+H`に何かを割り当てた画面が
Backspaceにも同じものを割り当ててしまいます。別扱いにしておけば、この重なりを
どうするかはバイトを変換する場所1つで決まります。

- HID（Tab5 KeyboardとUSB）: modifierのleft Ctrl（bit 0）／right Ctrl（bit 4）が
  立ったusage ID 0x04〜0x1Dを`Control`にします。数字や記号キーはCtrlが付いても
  印字どおりの文字のままです（割り当てが無く、落とすと入力が減るだけのため）
- CardKB: **CardKB v1.1にCtrlキーはありません。** `key_from_ascii`に`Control`へ
  の変換は置いていません。制御コードを`Control`として拾う分岐を書くことは
  できますが、押せないキーのための分岐になります

**実機確認済み**: HID経路（Tab5 Keyboard／USB）でCtrlが届き、ブラウザの
`Ctrl+Q`と`Ctrl+L`が効きます。CardKBにはCtrlキーが無いので、CardKBだけで
操作するときはブラウザの`q`（終了）と`F2`（アドレス欄）を使います。

## InputManager

`InputManager`（`src/input.rs`）はキー、USBマウス、タッチをアプリケーション層で
統合します。USB-Aの列挙とハブ状態、キーボード以外のUSBデバイスを所有するのは
`usb::UsbHost`のままで、`InputManager`はそれを内側に持ちます。

フレーム境界ごとに`service`（接続状態の保守）と`poll_key`（キーの読み出し）を
それぞれ1回呼びます。CardKBとTab5 Keyboardが不在のときは各60フレームごとに
再初期化を試みます。Tab5 KeyboardはI2C読み出し失敗を切断として扱って保持中の
ドライバを破棄するため、抜去後の再接続でも再プローブとHIDモード設定が行われます。
短時間の抜き差しでI2Cエラーを観測できずキーボードだけがリセットされた場合にも、
60フレームごとのHIDモード確認が復帰させます。
USB側のスキャン周期は[`USB.md`](USB.md)を参照してください。全画面アプリが
共通で使うキー待ちは`wait_for_key`です。USB root-portの物理接続変化はISRが記録し、
`service`がフレーム境界でtakeして即時再列挙します。割り込みを取り逃した場合に備え、
rootが空の間の低頻度fallback scanも残します。

rootへFS/LSで直結、またはFull-Speedハブ配下のUSB keyboard／mouseがperiodic channel 1〜4を確保できた場合、
`poll_key`／`poll_mouse`はフレームごとにUSB transactionを発行せず、ISRが完了済みにしたreportだけを
takeします。idle NAKの処理はDWCのframe list側で継続します。最大4 endpointで、割当て不能時と
High-Speedハブ配下（SplitとのDMA mode調停前）は従来のframe pollです。実機確認済みなのは
root直結LS keyboardとroot直結Full-Speed mouseのchannel 1経路です。Full-Speedハブ配下の
複数slotはHigh-Speedハブを`usbfs on`でFull-Speed列挙する代替試験により、keyboardをchannel 1、
mouseをchannel 2へ同時登録して確認済みです。
High-Speedハブ配下のLow-Speed HIDはperiodic channelではなくSplitのframe pollを使います。
旧ハブ＋keyboard／mouseでは動作実績がありますが、今回のハブ＋Low-Speed HIDでは
第19版で列挙・attach後、最初のInterrupt INのCSPLITが`HCINT=0x82`になりました。第20版は
Split tokenのendpoint typeをBulk偽装から実descriptorどおりInterruptへ修正したため文字入力まで
進みましたが、idle時のCSPLIT NYETをControl／Bulkと同様に最大5000 round追い続け、長いfreezeと
`giving up mid-split`を繰り返しました。第21版はInterrupt Splitだけを1 High-Speed full frame内の
SSPLIT＋最大3 CSPLITに制限し、窓切れを通常の「reportなし」として次のframe pollへ戻します。
実機ではエラーとfreezeが消えて入力可能になりましたが、3906 packet／11761 roundの観測は約57 Hzの
描画frameごとに1 pollしか行っていないことを示し、LS HIDの`bInterval`より遅いため反応が鈍い状態でした。
第22版は既存の1 kHz tickによる非描画wakeでSplit keyboardだけを`bInterval` msごとにpollし、受信keyを
16 eventのqueueへ保存します。描画、I2C input、接続保守は従来どおりframe境界で処理します。
新しいHigh-Speedハブ＋Low-Speed keyboardの実機で入力遅延とエラーがなく、50,661 packet／
202,076 round、mode conflict 0、stale token 0、port event 0を確認しました。

第22版でkeyboardを物理的に抜くと、root HPRTはハブが接続されたままなので変化せず、Splitの
`XACTERR`をstale sessionとしてroot bus全体を再列挙していました。セルフパワーハブの変化中portを
古い接続状態で再attachし、MSCも巻き込んで認識不能になる場合がありました。第23版はstaleになった
Split HIDの下流portだけを`GET_PORT_STATUS`とdebounceで確認し、切断／connection changeなら
該当slotだけを破棄します。root resetは行わず、MSCと他portのsessionを維持します。再挿入は空きportの
増分スキャンで列挙します。抜去・再挿入動作は第23版で実機確認済みです。

第23版ではエラーログなしでもキーを多く取りこぼしました。`usbhw`は145,394 packet／436,167 roundで、
ほぼ全pollが3 roundでした。`bInterval=1ms`のSYSTIMER wakeがUSB SOFと固定位相になり、遅い
microframeから同じSplit scheduleを繰り返していたうえ、CSPLITのNAK（通常のreportなし）後に同じ
1ms窓で新しいSSPLITを始めていました。第24版はperiodic SSPLITを次のHigh-Speed microframe 0へ
揃え、CSPLITを同一full frame内に確保します。NAKはTT bufferが解放された安全なidle完了として即座に
終了し、次のpollまで新しいSSPLITを出しません。またUSB2.0でLow-Speed Interruptの最小intervalは
10msなので、descriptorの不正な1msを10msへ補正します。毎秒約1000 packet／3000〜4000 IRQ相当の
過剰pollを毎秒約100 packetへ抑えつつ、旧57Hzより短い10ms samplingを維持します。起動時は
`invalid Low-Speed bInterval, descriptor=1`と`Split foreground poll interval ms=10`を表示します。
High-Speedハブ＋Low-Speed keyboardの実機では入力が安定し、エラーログも発生しませんでした。
キーを押さない10秒間の`IRQ split: packets`増加も約1,000回（約100 packet/秒）で、
10ms周期どおりに過剰pollが抑えられていることを確認済みです。
同じHigh-SpeedハブへHigh-Speed MSCを追加した`ut 100`もretry 0で完走し、Split側は
conflict 0、stale token 0、port event 0のままでした。

ハブ配下のHID periodic channelは、ハブのoccupied portをすべて列挙した後に開始します。
初回スキャン中に低い番号のHID portから開始すると、まだ残っているportのchannel 0列挙controlと
競合し、HIDを挿したままハブを接続した場合だけ認識しない実機症状があったためです。HIDを
後挿しする増分スキャンも同じく、その回の全port走査後に転送方式を選びます。

給電中のセルフパワーハブを接続した場合も、各HID portの新しいreset完了を待ってから列挙します。
upstream切断中も保持された古いaddress/configurationへaddress 0の要求を送らないためです。

同じbusへMSCも登録された場合は、RX FIFOを共有するpersistent HID DMAとbulk DMAを
同時稼働させません。registryがperiodic channelを停止して次のDATA PIDを引き継ぎ、HIDを
channel 0のframe pollへ戻します。device resetや再列挙ではないため接続状態は維持されます。
MSCが無い構成では上記のperiodic経路をそのまま使用します。

## ポインタ（USBマウス）

`poll_mouse`は`usb::MouseUpdate`をそのまま返し、キーのようには正規化しません。
キーはそれ単体で意味を持ちますが、マウスの移動量は相対値であり、「何の上を
動くか」を決めた側で初めて位置になるためです。カーソル位置と利得は描画側
（`src/app/win.rs`）が持つため、USBマウス部分はフレームバッファの寸法に依存しません。
利得と端数の持ち越しは[`APPS.md`](APPS.md)の
「Windows 95風デスクトップ」にあります。

タッチは絶対座標なので、`InputManager`がコントローラーの初期化・不在時の60フレーム
ごとの再試行を所有し、`poll_touch_points`で論理座標の全接触点を返す。タッチ診断と
ペイントはこのAPIだけを見る。ドライバ固有の接触IDは公開せず、1本指のポインタ操作が
必要な画面だけが`poll_primary_touch`を使う。これは最初に検出した接触をGT911では
ハードウェアID、ST7121/ST7123では固定レポートスロットで保持し、当該接触が離れるまで
追加の接触へ切り替わらない`Pressed`／`Moved`／`Released`のストリームである。

## タッチコントローラー

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

ドライバ層の`Touch::init()`はGT911を先にプローブし、失敗したらST7123にフォールバック
する。アプリケーションはこの型を直接初期化せず、`InputManager`のタッチAPIを使う。
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
変更しない点は描画APIの座標変換と同じです（[`GRAPHICS.md`](GRAPHICS.md)）。

タッチを使う画面（`paint`／`touchtest`／`win`）は
[`APPS.md`](APPS.md)を参照してください。
