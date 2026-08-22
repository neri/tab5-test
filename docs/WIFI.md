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
| `src/wifi/station.rs` | `esp_wifi_*`に対応する操作（初期化、スキャン、接続、状態、切断） |

C6のリンクは`src/app.rs`が`Option<wifi::Rpc>`として保持し、シェルコマンドを
またいで生かします。リンクを張り直すとC6がリセットされ接続が失われるため、
`wificonnect`の結果を`wifistatus`で見るには同じセッションが必要です。
IPスタック（`Option<net::Stack>`）はその隣に並べて持ち、リンクが切れたときは
一緒に捨てます。

**受信フレームは読まないと溜まります。** アソシエート後のC6はホストが読むまで
フレームを保持し、総量がステージングバッファを超えるとリンクは復帰できません。
`app.rs`のフレームループが毎フレーム`Stack::poll`を呼ぶのはこのためで、
背圧の扱いは[`NETWORK.md`](NETWORK.md)にあります。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `wifiinfo` | C6をSDIOカードとして活性化し、RCA・I/O関数数・CIS識別子・バス幅・クロックを表示（SDIO層の診断） |
| `wifiup` | ESP-Hostedのリンクを張り、スレーブが申告するチップID・ファームウェア版・capability・キューサイズを表示 |
| `wifimac` | RPCを1往復させてC6のSTA MACアドレスを取得（RPC層の診断） |
| `wifiscan` | station modeで起動してスキャンし、AP一覧（RSSI・チャンネル・認証方式・SSID）を表示 |
| `wificonnect <ssid> [password]` | 指定APへ接続。結果はイベントで待ち、成功ならSSID・BSSID・チャンネル・認証方式を表示 |
| `wifistatus` | 接続先のSSID・BSSID・チャンネル・RSSI。未接続ならスレーブのステータスコード |
| `wifidisconnect` | 切断 |

アソシエートした先でIPアドレスを取得して通信するコマンド（`ipconfig`・`ping`・
`tftpget`・`httpget`・`netdump`）は[`NETWORK.md`](NETWORK.md)にあります。

`wifiscan`以降は必要に応じてリンクを張り、`esp_wifi_init`→station mode→
`esp_wifi_start`→省電力オフまでを済ませてから本題に入ります。
`wifiinfo`と`wifiup`は下層の診断なので、実行するとセッションを捨てて
張り直します。

パスワードはUARTログに出しません。

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
