# Wi-Fi（ESP32-C6経由）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機で踏んだ罠:
> [`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md)

Tab5の無線はESP32-P4本体ではなく**ESP32-C6**が持ちます。C6には工場出荷時に
Espressifの**ESP-Hosted（esp-hosted-mcu）のslaveファームウェア**が書かれており、
P4から見るとC6は「SDIOバスにぶら下がったWi-Fiコプロセッサ」です。P4は
SDIO上でRPC（protobufメッセージ）を送り、C6上の`esp_wifi_*`を遠隔実行させます。

この文書はリンク層までです。**IPは[`NETWORK.md`](NETWORK.md)側**で、
C6から流れてくる`IF_STA`の受信フレームは`rpc.rs`のキューへ積まれ、
smoltcpのインタフェースが取り出します。ここまでの到達点はAPへ
アソシエートし、その状態を表示するところまでです。

## 接続とハードウェア

| P4側GPIO | C6側ネット | 用途 |
| --- | --- | --- |
| GPIO11 | `SDIO2_D0` | SDIOデータ0 |
| GPIO10 | `SDIO2_D1` | SDIOデータ1（SDIO割り込み線を兼ねる） |
| GPIO9 | `SDIO2_D2` | SDIOデータ2 |
| GPIO8 | `SDIO2_D3` | SDIOデータ3。識別中はSD/SPIモード選択線を兼ねる |
| GPIO13 | `SDIO2_CMD` | SDIOコマンド |
| GPIO12 | `SDIO2_CK` | SDIOクロック |
| GPIO15 | `RESET` | C6のリセット。平常High、Lowパルスでリセット |
| GPIO14 | `IO2` | 用途未確認。**出力にしない**（C6のブートストラップを兼ねるため） |

電源はPI4IOE5V6408（E2、I2Cアドレス`0x44`）の**P0**です。同じ拡張ICにUSB-A
VBUS（P3）と電源断パルス（P4）が同居するので、書き込みはビット単位の
read-modify-write（`usb::set_pi4ioe2_output_bit`）で行います。

SDIOバスはSDMMCコントローラの**カード1（slot 1）**です。カード0のmicroSDが
IOMUX直結なのに対し、slot 1はIOMUXの経路を持たず**GPIO Matrix経由でしか
配線できません**。コントローラは1つしかなく、2枚のカードで共有します。

C6は**2.4 GHz専用**です（Wi-Fi 6 = 2.4 GHzの802.11ax）。5 GHzのAPはスキャン
しても出ません。

## 層構造

| モジュール | 役割 |
| --- | --- |
| `src/sdmmc.rs` | SDHOSTコントローラ本体。カード番号付きのコマンド発行、カードごとのクロック分周器とバス幅 |
| `src/sdio.rs` | C6をSDIOカードとして活性化（CMD5/CMD3/CMD7、CCCR、CIS）し、CMD52とCMD53を提供 |
| `src/wifi/hosted.rs` | ESP-Hostedのフレーム層。12 byteヘッダ、スレーブレジスタ、送受信、初期化ハンドシェイク |
| `src/wifi/proto.rs` | RPCに必要な範囲だけのprotobuf |
| `src/wifi/rpc.rs` | TLVエンベロープと`Rpc`メッセージ、分割と再結合、イベントの保持、`IF_STA`受信フレームのキュー |
| `src/wifi/station.rs` | `esp_wifi_*`に対応する操作（初期化、スキャン、接続、設定取得、状態、切断） |

C6のリンクとIPスタックは`src/app/wifi_manager.rs`の接続管理器が、それぞれ
`Option<wifi::Rpc>`と`Option<net::Stack>`としてまとめて保持します。リンクを張り直すと
C6がリセットされ接続が失われるため、シェルコマンドをまたいで同じsessionを生かします。
管理器は接続元（CLI／メニュー）、IP設定方針（未設定／DHCP／static）、接続状態も持ち、
C6リンク喪失またはSTA切断時は古いIP stackを同時に捨てます。

**受信フレームは読まないと溜まります。** アソシエート後のC6はホストが読むまで
フレームを保持し、総量がステージングバッファを超えるとリンクは復帰できません。
`app.rs`のフレームループが毎フレーム`Stack::poll`を呼ぶのはこのためで、
背圧の扱いは[`NETWORK.md`](NETWORK.md)にあります。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `wifi` | 全画面メニュー。AP選択、password入力、保存方法の選択、DHCPまでを行う |
| `wifi on\|off\|status\|forget` | Wi-Fi全体の永続ON/OFF、管理器状態の表示、保存profileの削除。引数なしの`wifi`はメニューを開く |
| `wifiinfo` | C6をSDIOカードとして活性化し、RCA・I/O関数数・CIS識別子・バス幅・クロックを表示（SDIO層の診断）。終了後は元のON/OFF状態へ戻す |
| `wifiup` | ESP-Hostedのリンクを張り、スレーブが申告するチップID・ファームウェア版・capability・キューサイズを表示。終了後は元のON/OFF状態へ戻す |
| `wifimac` | RPCを1往復させてC6のSTA MACアドレスを取得（RPC層の診断） |
| `wifiscan` | station modeで起動してスキャンし、AP一覧（RSSI・チャンネル・認証方式・SSID）を表示 |
| `wificonnect <ssid> [password]` | 指定APへ接続。結果はイベントで待ち、成功ならSSID・BSSID・チャンネル・認証方式を表示 |
| `wifistatus` | 接続先のSSID・BSSID・チャンネル・RSSI。未接続ならスレーブのステータスコード |
| `wifisaved` | C6が現在読み込んでいるSTA設定のSSID、資格情報の有無、このboot中のprofile書き込み／失敗／forget回数を表示。passwordと長さは表示しない |
| `wififorget` | `wifi forget`の互換名。C6 NVSの保存済みWi-Fi設定をdefaultへ戻す。OFF中でも実行でき、OFF状態は維持する |
| `wifilog` | 接続管理器の直近16件の状態遷移、試行番号、接続世代、reason／RPC status／backoffを表示 |
| `wifidisconnect` | 今回のbootだけ現在のAPから切断する。Wi-Fiと保存profileは有効なままで、次回bootでは自動接続する |

`wifi`メニューは接続成功後にDHCPを自動開始し、最大15秒待って取得したIPv4アドレスを
結果画面へ表示します。一方、コマンドラインの`wificonnect`は従来どおり
アソシエーションだけを行い、IPアドレスは`ipconfig dhcp`で手動取得します。
IP関連コマンド（`ipconfig`・`ping`・`tftpget`・`httpget`・`netdump`）の詳細は
[`NETWORK.md`](NETWORK.md)にあります。

Wi-FiがONなら`wifiscan`以降は必要に応じてリンクを張り、`esp_wifi_init`→station mode→
`esp_wifi_start`→省電力オフまでを済ませてから本題に入ります。
OFF中のscan、接続、IP関連コマンドはC6を暗黙に起動せず、`wifi on`を案内します。
`wifiinfo`と`wifiup`は下層の診断なので一時的にセッションを捨てて張り直しますが、
終了時にOFFならC6を再びpower downし、ONなら保存profileの通常接続を復元します。

メニューは白背景、淡色のheader／footer／panel、黒い本文を基本色とする。AP一覧は
同じSSIDを1行へまとめ、最も強いRSSIのBSSIDと検出BSSID数を
15件ずつ表示します。1行は固定x位置の列で、左から**SSID／信号／CH／SECURITY／
BSSID数**の順です。SSIDが左端なのは読み手が探しているのがそれだからで、信号が
次なのは同じ名前の行を選び分ける材料だからです。列を固定xに置くのは、行を横に
読むだけでなく列を縦に読めるようにするためです。信号は4本の棒とRSSIの数値の
両方を出します——棒は行同士を一目で比べるためのもの、数値は同じAPの昨日と
比べるためのもので、UARTログや`wifiscan`と同じ形です。現在接続中のSSIDの行は
先頭に`✓`が付き、SSIDが太字（1 px右へ二度打ち）になります。`✓`は`Online`なら緑、
associate済みでlease未取得なら橙で、ブラウザの棒と同じ区別です。
上下／Page Up／Page Downで選択し、Enterまたは行のtapで接続、
`R`で再scan、Escapeで終了します。`O`はWi-Fi全体のON/OFF、`F`は確認画面を経たprofile削除です。
hidden SSIDは表示しますが選択できません。OPEN以外は最大64 byteの
パスワードを入力し、画面には同じbyte数の`*`だけを表示します。続けて保存方法を選び、
`Save and auto-connect`は次回起動にも使うprofile、`Connect once`はC6 resetまでの一時設定に
します。パスワードはコマンド履歴やUARTログに出しません。画面側の入力bufferはconnect要求の直後に消去し、接続管理器は初回接続の
自動再試行と接続後の自動再接続に使うため、現在のメニュー接続が有効な間だけ固定長RAM bufferへ
保持します。認証系エラーでの停止、別AP／CLI接続への置換、明示disconnect、低層sessionの
明示破棄、HP core reboot時にvolatile writeで消去し、値や長さを`wifilog`にも出しません。

永続profileはP4側Flashへ複製せず、C6のESP-IDF NVSだけに保存します。通常のメニュー試行は
最初に`WIFI_STORAGE_RAM`を選び、association成功後にだけ同じ設定を
`WIFI_STORAGE_FLASH`で書くため、誤passwordや到達不能APで以前のprofileを置換しません。
`Connect once`、CLI接続、再接続は毎回RAM保存を明示し、保存profileを上書きしません。
起動時はC6 NVSからmodeとprofileを読み、ONかつprofileが存在すれば白い起動画面の
Wi-FiサブアプリがassociationとDHCPを進めます。回復可能な失敗は起動画面に限って
500 ms間隔、初回を含め最大3回で止めます。OnlineならBrowser、profileなし、永続OFF、
認証失敗、association／DHCPの終端失敗なら白いWi-Fiメニューへ進みます。OFFならsessionと
stackを作らずC6をpower downします。起動画面を出ると理由別の通常retry policyへ戻り、
進行中の接続とDHCPは同じ`Manager`が引き継ぎます。
Stage 6の実機確認ではC6 reset、HP core reboot、完全電源断のすべてでprofile保持を確認しました。

起動時自動接続または以前のメニュー接続が有効な状態で、メニューまたはCLIから別の接続を
開始する場合は、古いIP stackと再接続timerを無効化してからC6へdisconnectを要求し、切断イベントを
最大3秒待ちます。切断完了後にだけ新しいRAM設定とconnect要求を送るため、既存associationと
新しい要求を競合させません。保存済みprofileは新しいassociationが成功するまで変更しません。
切断RPCの失敗またはtimeout時は新しい接続を開始せず、メニュー／CLI表示と`wifilog`へ理由を残します。
CLIはこの前処理を共有しますが、接続後のDHCPは従来どおり`ipconfig dhcp`まで開始しません。

`wifisaved`はC6の`esp_wifi_get_config(WIFI_IF_STA)`相当RPCを呼びます。応答には平文の
passwordも含まれるため、固定長bufferへ必要な範囲だけcopyした直後にRPC応答bufferを消去し、
表示はSSIDと資格情報の有無だけに限定します。このコマンドが示すのは「現在C6が読み込んでいる
設定」であり、RAMの`Connect once`設定を表示する場合もあります。保存profileそのものを
確認するには、接続設定を変更する操作を挟まずC6 reset後に再実行します。`wififorget`は
`esp_wifi_restore`相当RPCでC6のWi-Fi永続設定をdefaultへ戻し、P4側の自動再接続資格情報も
消去します。

scanとconnect要求のRPC自体は同期処理ですが、associationイベント待ちとDHCP待ちは
フレーム駆動です。待機中も入力とC6リンクを毎フレーム処理し、Escapeでシェルへ戻っても
開始済みの接続とDHCPは管理器が続行します。associationは20秒、結果画面で待つDHCPは15秒で
timeout表示になりますが、DHCP clientは動作を続け、後からleaseを取得できます。

初回接続でreason 4を受けた場合は500 ms間隔で3回再試行し、その後は1、2、4、8、16、最大30秒の
backoffへ移ります。AP不在、beacon timeout、association失敗、接続timeout、RPC無応答も一般
backoffで再試行します。認証／handshake系reason 15、202、204、210、211とRPCが返したerror
statusでは停止し、APを選び直してpasswordを再入力するよう表示します。

メニュー接続の成功後にSTA切断を検出した場合も古いIP stackを直ちに破棄し、同じreason分類と
backoffで自動再接続します。APが戻ってassociationできると新しいstackでDHCPを取り直します。
C6／ESP-Hostedリンク喪失ではC6リンクから再構築します。10分安定すると失敗回数をresetします。
DHCP leaseだけを失った場合はWi-Fiを切らず、動作中のDHCP clientが再取得を続けます。
これらはメニュー接続だけが対象で、CLIの`wificonnect`には自動再接続も自動DHCPも適用しません。

ON/OFFはC6 NVSのWi-Fi modeで保持します。保存profileがある場合はprofileを残したまま
`WIFI_MODE_NULL`をOFFとして使います。profileがない初期状態との区別には、FAST scanでは
接続動作へ使われないSTA設定の`failure_retry_cnt`へmarkerを置きます。ONへ戻すとmarkerを消し、
station modeを開始します。P4側RAM内の資格情報はreboot／完全電源断をまたがず、永続profileの
平文資格情報はC6側だけにあります。

## 再起動をまたぐC6

`reboot`はHP CPUコアだけをリセットします。**C6は別チップで、電源をゲートする
I2Cエクスパンダごとリセットされないため、アソシエートしたまま生き続けます。**
起動直後に`SDIO: pad levels without pull-up=`がDAT1（bit 2）だけLowを示すのが
その痕跡で、これは**SDIOの割り込み線をC6が能動的に駆動している**——ホストが
読まなかったフレームについて割り込みを上げ続けている——という意味です。
プルアップ有りでも値が変わらないことで、浮いているのではなく駆動されている
と判定できます。

放っておくと、次に`sdio::init`がリセット線を叩いた瞬間にC6が**アソシエート中に
何も告げずに消えます**。APには応答しなくなったステーションのエントリが残り、
その無通信タイムアウトが次のアソシエーションに降ってくるので、
**再起動後の最初の`wificonnect`だけが`reason 4 DISASSOC_DUE_TO_INACTIVITY`で
失敗し、2回目は成功します**。SDIO側にはエラーが1つも出ないので、リンクを
いくら調べても見つかりません。

対処は`shell::reboot`のdeauthと、ブート経路の`sdio::power_down_c6`の2つです。
役割の違いは[`CONSOLE_SHELL.md`](CONSOLE_SHELL.md)に書いてあります。

## 実装上の要点

- **MAC取得RPCの`mode`は実際にはインタフェース番号**: C6側は要求値を
  `wifi_interface_t`として扱います。STAは`WIFI_IF_STA = 0`です。
  `WIFI_MODE_STA = 1`を渡すとSoftAP側のMACを取得してしまいます
- **フレームは1回の読み出しに複数載る**: スレーブの`PACKET_LEN`は累積バイト数で、
  差分は複数フレームの合計になり得ます。`hosted.rs`はステージングバッファの
  未解析部分を持ち、使い切るまでバスに触らずフレームを1つずつ返します
- **1回のCMD53は最大1,536 byte**: SDIOスレーブは1回の転送でバッファ1個ぶんまでしか
  返さないため、それより長い読み出しは分割します。ウィンドウの終端は固定
  （`0x1F800`）なので、各回のアドレスは`0x1F800 - 残量`です
- **入れ子メッセージは省略しない**: protobufの入れ子を省くとスレーブ側では
  ヌルポインタになり、この世代のファームウェアはガードせず参照してリセットします。
  参照ホストが常に送るもの（`wifi_sta_config`の`threshold`と`pmf_cfg`など）は
  値がすべて0でも送ります
- **省電力は明示的に切ります**（`WIFI_PS_NONE`）。眠るコプロセッサはバスに
  応答しないコプロセッサです
- **リンク切れは1回だけ報告します**: レジスタ読みが3回続けて失敗したら
  リンク切れと判定してポーリングを止め、シェルはセッションを捨てて次回張り直します

## microSDとの共存

コントローラは1つなので、次の2点で干渉します。

- **コントローラのリセットは初回だけ**行います。カードの活性化ごとにリセットすると、
  もう一方のカードのホスト側設定（クロックイネーブル、分周器、バス幅）が消えます
- **入力クロックは共有**です。カードごとの分周器は別々に選べますが、
  その手前の`sdhost_cclk_in`は共通なので、C6が活性化されている間は
  microSDのHigh Speed（40 MHz）への切り替えを行いません。C6側の分周器は
  20 MHz入力を前提に選んでおり、入力を倍にするとC6がHigh Speedを有効に
  しないまま40 MHzで駆動されるためです。この間microSDはDefault Speedの
  20 MHzに留まります

カード活性化中は識別用に入力クロックが400 kHzまで下がるので、もう一方の
カードも一時的に遅くなります。シェルは単一スレッドで、その瞬間にもう一方の
転送が走ることはありません。
