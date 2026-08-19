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
- USB HID Bootキーボード（`src/usb/hid_keyboard.rs`）: 詳細は
  [`USB.md`](USB.md)。

どちらも`src/input.rs`の`Key`へ正規化します。`Key`は`Ascii(u8)`と、Escape、
カーソル4方向、Home/End、PageUp/PageDown、Insert、Delete、`Function(u8)`を
持ちます。入力を受け取る側は、キーがCardKBとUSBのどちらから来たかを知る必要が
ありません（`KeyEvent`の`source`で区別できますが、コンソールは使いません）。
コンソールがどのキーに何を割り当てているかは
[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)を参照してください。

## InputManager

`InputManager`（`src/input.rs`）はキーを出しうる全ソースをアプリケーション層で
所有します。USB-Aの列挙とハブ状態、キーボード以外のUSBデバイスを所有するのは
`usb::UsbHost`のままで、`InputManager`はそれを内側に持ちます。

フレーム境界ごとに`service`（接続状態の保守）と`poll_key`（キーの読み出し）を
それぞれ1回呼びます。CardKBが不在のときは60フレームごとに再初期化を試みます。
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

## ポインタ（USBマウス）

`poll_mouse`は`usb::MouseUpdate`をそのまま返し、キーのようには正規化しません。
キーはそれ単体で意味を持ちますが、マウスの移動量は相対値であり、「何の上を
動くか」を決めた側で初めて位置になるためです。カーソル位置は描画側
（`src/app/win.rs`）が持ちます。この分担のおかげで`input.rs`はフレーム
バッファの寸法に依存しません。利得と端数の持ち越しは[`APPS.md`](APPS.md)の
「Windows 95風デスクトップ」にあります。

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
変更しない点は描画APIの座標変換と同じです（[`GRAPHICS.md`](GRAPHICS.md)）。

タッチを使う画面（`paint`／`touchtest`）は
[`APPS.md`](APPS.md)を参照してください。
