# ESP32-C6経由Wi-Fi対応 実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: 全Stage完了（実機確認済み）

## 方針

Tab5のWi-Fi無線はESP32-P4本体ではなくESP32-C6が持つ。C6には工場出荷時に
Espressifの**ESP-Hosted（esp-hosted-mcu）のslaveファームウェア**が書かれており、
P4から見るとC6は「SDIOバスにぶら下がったWi-Fi/BLEコプロセッサ」である。P4側は
SDIO上でRPC（protobufメッセージ）を投げ、C6上の`esp_wifi_*`を遠隔実行させる。
ESP-IDFの世界では`esp_wifi_remote`＋`esp_hosted`コンポーネントがこれを担当するが、
本リポジトリはESP-IDFをリンクしないので、必要な範囲を自前で実装する。

したがって本計画は次の3層に分かれる。層ごとに実機で確認できる単位が異なるため、
`SD_CARD_PLAN.md`と同じく一気に実装せず層ごとに確認しながら進める。

1. **SDIOバス層**: SDMMCコントローラのslot 1をGPIO Matrix経由で起こし、
   C6をSDIOカードとして活性化してCMD52/CMD53でレジスタとデータを読み書きする
2. **ESP-Hostedフレーム層**: 12 byteの`esp_payload_header`、インターフェース種別
   （STA/SERIAL/PRIV）、スレーブ初期化イベントとホスト設定の交換、
   送受信バッファのフロー制御
3. **RPC層**: SERIALインターフェース上のTLV＋protobufで
   `esp_wifi_scan_start`や`esp_wifi_connect`に相当するRPCを往復させる

参照実装は[esp-hosted-mcu](https://github.com/espressif/esp-hosted-mcu)（`host/`以下と
`common/proto/esp_hosted_rpc.proto`）と、ワークスペース同梱のESP-IDF v5.5.3
（`.embuild/espressif/esp-idf/v5.5.3/components/sdmmc/sdmmc_io.c`、
`components/soc/esp32p4/sdmmc_periph.c`）をレジスタ・シーケンスの照合先として使う
（いずれもリンクはしない）。

## 到達目標と非目標

到達目標は次のシェルコマンドである。

- `wifiscan` — スキャンしてAP一覧（SSID、RSSI、チャンネル、認証方式）を表示
- `wificonnect <ssid> [password]` — 指定APへ接続し、結果を表示
- `wifistatus` — 接続状態、接続先SSID/BSSID/チャンネル/RSSIを表示
- `wifidisconnect` — 切断

**非目標（この計画では実装しない）**:

- TCP/IPスタック（lwIP相当）、DHCPクライアント、ping、ソケットAPI
- SoftAP、WPA2/3 Enterprise、DPP、iTWT、省電力（Host Power Save）
- Bluetooth/HCI、OpenThread、C6のOTA、Network Split、GPIOエキスパンダ
- 割り込み駆動（`USB_HOST_PLAN.md`と同じくポーリングで実装する）

このため接続に成功しても「IPアドレスを取得して通信する」ことはできない。到達点は
**リンク層としてAPにアソシエートし、その状態を表示できること**まで。C6から流れてくる
STAデータフレーム（ブロードキャスト等）は受信して破棄する。この制限は完了時に
`DESIGN.md`の「制約」へ明記する。

## モジュール構成（案）

| 追加/変更 | 責務 |
| --- | --- |
| `src/sdmmc.rs`（変更） | slot 1（card 1）対応。CMD発行時の`card_number`、`CLKENA`/`CLKSRC`/`CTYPE`のカード別ビット、GPIO Matrix経由のピン設定 |
| `src/sdio.rs`（新規） | SDIO I/Oカードの活性化（CMD5/CMD3/CMD7/CCCR）とCMD52・CMD53のプリミティブ |
| `src/wifi/hosted.rs`（新規） | ESP-Hostedのペイロードヘッダ、スレーブレジスタ、TX/RX、初期化ハンドシェイク |
| `src/wifi/rpc.rs`（新規） | SERIAL TLVとprotobufの符号化・復号、リクエスト/レスポンス往復 |
| `src/wifi/mod.rs`（新規） | 接続状態の保持と`scan`/`connect`/`status`の公開API、`poll` |
| `src/gpio.rs`（変更） | C6用ピン（GPIO8..13のSDIO、GPIO15のRESET）の定義とGPIO Matrix配線ヘルパ |
| `src/usb/hcd.rs`（利用） | E2.P0（C6の電源／イネーブル）を既存の`set_pi4ioe2_output_bit`で駆動する |
| `src/app/shell.rs`（変更） | 上記シェルコマンドとヘルプ |
| `src/app.rs`（変更） | キー入力ループから`wifi::poll()`を呼び、イベントとデータフレームを吸い出す |

## ハードウェア前提

P4とC6の接続は次のとおり確定している。

| P4側GPIO | C6側ネット | 用途 |
| --- | --- | --- |
| GPIO11 | `SDIO2_D0` | SDIOデータ0 |
| GPIO10 | `SDIO2_D1` | SDIOデータ1（SDIO割り込み線を兼ねる） |
| GPIO9 | `SDIO2_D2` | SDIOデータ2 |
| GPIO8 | `SDIO2_D3` | SDIOデータ3 |
| GPIO13 | `SDIO2_CMD` | SDIOコマンド |
| GPIO12 | `SDIO2_CK` | SDIOクロック |
| GPIO15 | `RESET` | C6のリセット |
| GPIO14 | `IO2` | C6のIO2。SDIOには使わない |

電源／イネーブルは**PI4IOE5V6408（E2、I2Cアドレス`0x44`）のP0**、すなわち
`src/usb/hcd.rs`の`set_pi4ioe2_output_bit`にビット0を渡す線である。USB-A VBUS
（P3）や`power.rs`のシャットダウンパルス（P4）と同じ拡張ICを共有するので、
書き込みは必ず既存のビット単位read-modify-writeを使い、他のピンの設定を壊さない。

SDIOバスはSDMMCコントローラの**slot 1**（ESP-IDFの信号名では`SD_CARD_*_2_*`、
M5Stackの`SDIO2_*`表記と対応する）。slot 1はIOMUX直結の経路を持たず、
**GPIO Matrix経由でしか配線できない**（`components/soc/esp32p4/sdmmc_periph.c`の
slot 1の`iomux_pin_num`はすべて`-1`で、`sdmmc_slot_gpio_sig`にslot 1だけ
信号インデックス0..9が入っている）。microSD（slot 0、GPIO39..44のIOMUX直結）とは
経路が異なる。

### Stage 0で残っている確認項目

- ~~`RESET`（GPIO15）の極性~~ → **平常High、Lowパルスでリセット**で確定（Stage 1の実機試験）
- E2.P0は出力・非ハイインピーダンス・Highに設定できるが、入力ステータスは
  Lowのままを読む。C6は動いているので実害はないが、電源スイッチの制御入力で
  読み戻しが駆動値を返さない配線と推測している。必要になったら再確認する
- `IO2`（GPIO14）の用途。C6のIO2はブート時のストラップピンでもあるため、
  P4側が駆動していると起動モードが変わる可能性がある。**用途が判明するまでは
  入力のまま放置し、出力にしない**。ホスト起床（Host Power Save）や
  スレーブ側GPIOとして使われている可能性もあるが、本計画の範囲では使わない
- ~~出荷ファームウェアのバージョン~~ → INITイベントに**バージョンTLVが無い**ことが
  判明（Stage 2の実機試験）。ESP-Hostedのホストも同じ場合を「0.0.0」として扱う。
  必要なRPCはすべて0.0.6で追加されたものなので実害はない

## 段階分け

### Stage 0: 配線・電源の確認 ✅ 配線と電源線は確定（上表）

残りは上記「Stage 0で残っている確認項目」のとおり、極性・初期状態・`IO2`の用途で、
いずれも実機での挙動確認またはStage 1・2の実装中に判明する。コードは書かない。

### Stage 1: SDMMC slot 1のホスト初期化とSDIOカード活性化 ✅ 完了（実機確認済み）

`src/sdmmc.rs`をカード番号付きに拡張し、`src/sdio.rs`を新設する。

- **給電とリセット解除**: `usb::set_pi4ioe2_output_bit(0, true)`でE2.P0をオンにし、
  GPIO15のRESETを（極性確認のうえ）解除してからC6のブートを待つ。既にオンで
  起動している場合でも、明示的に設定してから始めれば状態が確定する
- **slot 1のピン設定**: GPIO Matrixのアウト（`GPIO_FUNCn_OUT_SEL_CFG`）へ
  CLK=0、CMD=1、D0=2、D1=3、D2=4、D3=5の信号インデックスを、イン
  （`GPIO_FUNCn_IN_SEL_CFG`）へCMD/D0..D3の同インデックスを設定する。
  `SD_CARD_PLAN.md` Stage 1の教訓どおり、IOMUXの`fun_ie`（入力バッファ有効）と
  `fun_wpu`をCMD/D0..D3で明示的に立てる。IOMUXの機能選択はGPIO機能
  （マトリクス経由）にする
- **コントローラのカード別ビット**: `SDHOST_CMD_REG`のカード番号フィールド、
  `SDHOST_CLKENA_REG`のbit1（cclk_enable card1）とbit17、`SDHOST_CLKSRC_REG`の
  card1用ディバイダ選択、`SDHOST_CTYPE_REG`のbit1（card1の4-bit幅）。既存の
  `set_card_clock`は`CLKENA`のbit0/bit16とcard0前提なので、カード番号を引数に取る形へ
  一般化する
- **SDIO活性化**: CMD52によるI/Oリセット（CCCR `0x06`へ`0x08`）→
  CMD5（IO_SEND_OP_COND、OCR=0で問い合わせ→電圧ウィンドウ指定でreadyまでポーリング）
  → CMD3（RCA取得）→ CMD7（選択）→ CCCR `0x07`で4-bit幅 →
  FN0/FN1のブロックサイズを512（FBRは`0x100 * fn + 0x10/0x11`）→
  CCCR `0x02`でFunction 1を有効化し`0x03`でreadyを待つ →
  CCCR `0x04`で割り込み許可（マスタ有効＋FN1。ホスト側は割り込みを使わないが、
  ESP-Hostedのホストが設定しているので同じにしておく）
- クロックは列挙時400 kHz、その後は既存の分周設定を流用して20 MHzから始める
  （ESP-HostedのホストはPCB上の上限を50 MHzとしている。40 MHz化はStage 5以降の
  余力があるときの課題とする）
- 診断コマンド`wifiinfo`（暫定名）を追加し、RCA、CCCRの主要バイト、CIS由来の
  Vendor ID/Device IDをコンソールへ出す

**実機確認（済み）**: CMD5が有効なOCRを返し、CISの`CISTPL_MANFID`が読めること。
実機の値はmanufacturer=`0x0092`／product=`0x6666`だった（計画時に想定していた
`0x6666`／`0x2222`ではない。詳細は「実機での判断記録」を参照）。
CMD5のOCRは`0x20FFFF00`で、I/O関数2個・メモリ部なしというSDIO専用カードの
応答だった。

**想定される罠**:

- slot 0はIOMUXなので`fun_ie`だけで済んだが、slot 1はGPIO Matrixのイン側設定を
  忘れると同じ「送信は通るのに応答が常に0」という壊れ方をする
- microSDとコントローラを共有する。Stage 1時点では同時使用しないが、
  `sdmmc::init()`がコントローラ全体をリセットするため、SDカード操作の後に
  C6が切れる（またはその逆）ことがあり得る。共存はStage 6の課題として切り出し、
  それまでは「先に使った方が勝つ」で構わない
- C6が起動途中だとCMD52が失敗する。ESP-Hostedのホストは100 ms間隔で最大1.5 s
  リトライし、それでも駄目ならRESETをかけて再試行する。同じ構造にする
- GPIO14（C6の`IO2`）を出力にしない。C6側のIO2はブートストラップピンでもあるため、
  リセット解除の瞬間にP4が駆動していると起動モードが変わり、SDIOスレーブとして
  立ち上がらないという分かりにくい失敗になり得る

### Stage 2: ESP-Hostedトランスポート層 ✅ 完了（実機確認済み）

`src/wifi/hosted.rs`を新設し、フレームの送受信と初期化ハンドシェイクまでを通す。
パケット本体はCMD53のブロックモード、32 bitのスレーブレジスタはCMD53の
バイトモードで転送する（どちらも既存のIDMAC経路）。CCCRなど1 byte単位の
アクセスだけStage 1のCMD52を使う。

> 当初は「4 byteレジスタもCMD52×4回」で始める計画だったが、実装時に変更した。
> 理由は「実機での判断記録」のStage 2の項を参照。

- スレーブレジスタ（Function 1、アドレスは下位10 bitのみ有効）:

  | 名前 | アドレス | 用途 |
  | --- | --- | --- |
  | `INT_RAW` | `0x050` | 割り込み要因。bit23=新規パケット、bit7/bit6=送信スロットル開始/停止 |
  | `INT_CLR` | `0x0D4` | 上記のクリア |
  | `PACKET_LEN` | `0x060` | スレーブが送信済みの累積バイト数（下位20 bit） |
  | `TOKEN_RDATA` | `0x044` | bit16..27がスレーブの受信バッファ累積数 |
  | `SCRATCH_REG_7` | `0x08C` | ホスト→スレーブ割り込み。bit0=データパスopen |

- **受信長の算出**: `PACKET_LEN`の値から自ホストの累積読み出しバイト数を引き、
  `0x100000`でロールオーバーさせる。得られた長さぶんを
  `0x1F800 - 残りバイト数`のアドレスに対するCMD53ブロック読み出しで吸い出す
  （末尾アドレス固定・512の倍数へ切り上げ、余りはスレーブがゼロ埋めする）
- **送信**: 必要バッファ数 = `ceil((12 + payload_len) / 1536)`。`TOKEN_RDATA`から
  求めた空きバッファ数がそれ以上になるまで待ってから、
  `0x1F800 - 全長`へCMD53ブロック書き込みする
- **ペイロードヘッダ**（12 byte、リトルエンディアン）:

  | オフセット | 内容 |
  | --- | --- |
  | 0 | 下位4 bit=`if_type`、上位4 bit=`if_num` |
  | 1 | `flags`（bit0=MORE_FRAGMENT） |
  | 2..3 | `len`（ペイロード長） |
  | 4..5 | `offset`（ヘッダ長=12） |
  | 6..7 | `checksum`（ヘッダ＋ペイロードの単純バイト加算。計算時はこの欄を0にする） |
  | 8..9 | `seq_num` |
  | 10 | 下位2 bit=`throttle_cmd` |
  | 11 | `priv_pkt_type`／`hci_pkt_type`との共用 |

  `if_type`は STA=1、AP=2、SERIAL=3、HCI=4、PRIV=5、TEST=6、ETH=7。

- **初期化ハンドシェイク**:
  1. データパスopenとして`SCRATCH_REG_7`に`0x01`を書く
  2. スレーブから`if_type=PRIV`のパケットが来る。中身は
     `event_type=0x22（INIT）`、`event_len`、続いてTLV列。
     タグ`0x11`=capability（1 byte）、`0x16`=拡張capability（4 byte）、
     `0x12`=チップID（ESP32-C6は`0x0D`）、`0x14`/`0x15`=スレーブのキューサイズ、
     `0x17`=ファームウェアバージョン（4 byte LE）、`0x18`=SDIOモード
     （1ならstreaming。ホストはpacketモードなので、1が来たらエラーとして扱う）
  3. ホストからも`if_type=PRIV`で同形式のINITイベントを返す。TLVは
     `0x44`=ホストcapability（0でよい）、`0x45`=受け取ったチップID、
     `0x46`=raw throughputテスト（0）、`0x47`/`0x48`=スロットル閾値
  4. capabilityの`ESP_CHECKSUM_ENABLED`（bit7）が立っていればチェックサム必須。
     常に計算して入れておけば分岐が要らない
- 受信したフレームは`if_type`で振り分け、SERIALはRPC層へ、PRIVはハンドシェイクへ、
  STA/APは**破棄**する。`MORE_FRAGMENT`が立っていれば次のフレームと連結する
- スレーブが送ってくるパケットを読み出さないとスレーブ側のバッファが尽きるため、
  接続後は`src/app.rs`のループから`wifi::poll()`を呼んで捨て続ける

**実機確認**: INITイベントのチップIDが`0x0D`（ESP32-C6）、ファームウェアバージョンが
妥当な値としてUARTログに出ること。ホスト設定を返した後、スレーブがエラーを返さず
（＝以降のSERIALパケットが流れ始める状態になり）RPCの土台が整うこと。

**想定される罠**:

- IDMACのバッファは内部SRAM。`sdmmc.rs`と同じく、DMAへ渡す前のwriteback、
  CPUが読む前のinvalidateが必要（`docs/KNOWN_ISSUES.md`のDW-GDMA/SDHOSTの制約も参照）
- `PACKET_LEN`が`0xFFFFFFFF`を返したらSDIOバス自体の異常。ESP-Hostedのホストも
  この値を専用にバスフォールトとして扱っている
- 累積カウンタ方式なので、一度読み落とすと以後ずっとズレる。読み出したバイト数を
  必ず加算し、失敗時の扱いを決めておく

### Stage 3: RPC層（TLV＋protobuf） ✅ 完了（実機確認済み）

`src/wifi/rpc.rs`を新設する。SERIALインターフェースのペイロードは
「TLV → protobufでシリアライズした`Rpc`メッセージ」という二重の入れ子になっている。

- **TLV**（`compose_tlv`／`parse_tlv`と同じ）:
  `0x01` + エンドポイント名長（2 byte LE）+ 名前 + `0x02` + データ長（2 byte LE）+ データ。
  ホストが送るときの名前は`"RPCRsp"`（6文字）。スレーブからは`"RPCRsp"`（応答）または
  `"RPCEvt"`（イベント）が来る。どちらも6文字なので固定長で読める
- **protobuf**: 依存クレートを増やさず手書きする。必要なのはvarint、
  length-delimited（wire type 2）、入れ子メッセージだけで、`Rpc`の共通部分は
  `msg_type`(1)=Req/Resp/Event、`msg_id`(2)、`uid`(3)、そして`msg_id`と同じ番号の
  フィールドにペイロードが入る、という規則になっている
- 送信は「protobufを組む → TLVで包む → 1524 byteごとに分割して`MORE_FRAGMENT`付きで
  ESP-Hostedへ渡す」、受信はその逆。応答は`uid`で対応付ける（単純に
  1リクエストずつ直列に投げ、タイムアウト付きでポーリングする）
- **符号化の注意**: `.proto`で`int32`のフィールドに負の値が入ると、varintは
  64 bitへ符号拡張されて**10 byte**になる。RSSIは負なので復号側で必ず踏む

必要なメッセージとID（`common/proto/esp_hosted_rpc.proto`より）:

| 用途 | msg_id | リクエストのフィールド | 応答のフィールド |
| --- | --- | --- | --- |
| `WifiInit` | 278 | `cfg`(1)=`wifi_init_config` | `resp`(1) |
| `SetWifiMode` | 260 | `mode`(1)（STA=1） | `resp`(1) |
| `WifiStart` | 280 | なし | `resp`(1) |
| `WifiSetConfig` | 284 | `iface`(1)（STA=0）、`cfg`(2)=`wifi_config` | `resp`(1) |
| `WifiConnect` | 282 | なし | `resp`(1) |
| `WifiDisconnect` | 283 | なし | `resp`(1) |
| `WifiScanStart` | 286 | `config`(1)、`block`(2)、`config_set`(3) | `resp`(1) |
| `WifiScanGetApNum` | 288 | なし | `resp`(1)、`number`(2) |
| `WifiScanGetApRecords` | 289 | `number`(1) | `resp`(1)、`number`(2)、`ap_records`(3)を繰り返し |
| `WifiStaGetApInfo` | 294 | なし | `resp`(1)、AP情報 |
| `GetMACAddress` | 257 | `mode`(1) | `resp`、MAC |

応答の`msg_id`はリクエスト＋256（例: `WifiScanStart`のRequestは286、Responseは542）。
イベントは768番台で、スキャン完了=774、STA接続=775、STA切断=776。

`wifi_ap_record`のフィールドは`bssid`(1, bytes)、`ssid`(2, bytes)、`primary`(3, uint32)、
`second`(4)、`rssi`(5, int32)、`authmode`(6, int32)、`pairwise_cipher`(7)、
`group_cipher`(8)、`ant`(9)、`bitmask`(10)、`country`(11)、`he_ap`(12)、
`bandwidth`(13)以降。表示に使うのは`ssid`/`rssi`/`primary`/`authmode`だけなので、
**知らないフィールド番号は wire type を見て読み飛ばす**復号器にする。

`wifi_sta_config`は`ssid`(1, bytes)、`password`(2, bytes)、`scan_method`(3)、
`bssid_set`(4)、`bssid`(5)、`channel`(6)、`listen_interval`(7)、`sort_method`(8)、
`threshold`(9)、`pmf_cfg`(10)、`bitmask`(11)…で、`wifi_config`は
`oneof`の`sta`(2)に`wifi_sta_config`を入れる。

このStageの確認は`wifimac`（暫定名、`GetMACAddress`を1往復）で行う。実装量が
最も多いのがこのStageなので、往復1本が通ってから次へ進む。

**実機確認**: C6のSTA MACアドレスが妥当な値（M5Stack製品のOUIまたはEspressif OUI）
としてコンソールに出ること。

**想定される罠**:

- protobufのフィールド順は昇順でなくてもよいが、スレーブ側は`protobuf-c`生成コードで
  復号するため、未知フィールドや不正なwire typeを送ると黙って失敗する
- `WifiInit`の`wifi_init_config`は空でも通るとは限らない。ESP-IDFの
  `WIFI_INIT_CONFIG_DEFAULT()`相当の値（特に`magic`）を埋める必要があるかを、
  最初の実機テストで切り分ける。空で通らない場合はESP-IDFの
  `components/esp_wifi/include/esp_wifi.h`の既定値を移植する

### Stage 4: スキャン（`wifiscan`） ✅ 完了（実機確認済み）

シーケンスは `WifiInit` → `SetWifiMode(STA)` → `WifiStart` →
`WifiScanStart(block=true)` → `WifiScanGetApNum` → `WifiScanGetApRecords(n)`。

- `block=true`にすると応答がスキャン完了まで返らないので、シングルスレッドの
  シェルと相性がよい（イベント待ちの状態機械を作らずに済む）。応答待ちの
  タイムアウトは10秒程度を見込む。うまくいかない場合は`block=false`にして
  イベント774（スキャン完了）を待つ形へ切り替える
- 初期化済みかどうかを`src/wifi/mod.rs`で保持し、2回目以降は`WifiInit`から
  やり直さない
- 表示は最大20件程度に制限し、SSID（32 byteまで、非ASCIIは`.`に置換）、
  RSSI、チャンネル、認証方式（`authmode`の数値を`OPEN`/`WPA2_PSK`/`WPA3_PSK`等の
  文字列に変換）を1行1APで出す。全件はUARTログへ

**実機確認**: 周囲の既知のSSIDが妥当なRSSI（-30〜-90 dBm程度）とチャンネルで
並ぶこと。スマートフォンのテザリングのSSIDを一時的に立て、それが出るかどうかで
「実データを読めている」ことを確認する。

**想定される罠**:

- AP件数が多いと`WifiScanGetApRecords`の応答が1536 byteを大きく超え、必ず
  フラグメント再結合を通る。Stage 2の再結合が正しくないとここで初めて露見する
- 応答バッファはヒープ（PSRAM）に取る。数十件×`wifi_ap_record`で数KB規模になる
- `authmode`の数値はESP-IDFの`wifi_auth_mode_t`。バージョンによって値が増えるので、
  未知の値は数値のまま出す

### Stage 5: 接続（`wificonnect` / `wifistatus` / `wifidisconnect`） ✅ 完了（実機確認済み）

- `wificonnect <ssid> [password]`: `WifiSetConfig(iface=STA, sta{ssid, password})` →
  `WifiConnect` → イベント775（接続）または776（切断＝失敗）を待つ。
  776の`reason`コードを数値で表示する（`NO_AP_FOUND`、`AUTH_FAIL`などの主要な
  値だけ文字列にする）
- 待ち時間は15秒程度。この間`wifi::poll()`相当のループでSDIOを読み続ける
- `wifistatus`: `WifiStaGetApInfo`で接続先のSSID/BSSID/チャンネル/RSSIを表示。
  未接続なら未接続と表示する
- `wifidisconnect`: `WifiDisconnect`を送り、イベント776の受信を確認する
- 接続後はSTAデータフレームが流れ始めるので、`src/app.rs`のループから
  `wifi::poll()`を呼んで受信・破棄する。破棄した数を統計として持ち、
  `wifistatus`で出すと動作確認に使える

**実機確認**: 手元のAPへ接続してイベント775が来ること。パスワードを1文字変えると
776が`AUTH_FAIL`相当のreasonで返ること（成功と失敗の両方が正しく判別できることの確認）。
接続したまま数分放置してもSDIOが詰まらず（`wifistatus`が応答し続ける）、
破棄フレーム数が増え続けること。

**想定される罠**:

- パスワードはシェルの引数に平文で並ぶ。UARTログに出さないこと
- 接続後の受信を怠るとスレーブのバッファが尽き、送信側（`TOKEN_RDATA`の空き）も
  止まってRPCが返らなくなる。「応答が返らない」の原因がここに化ける
- キー入力ループの1周ごとにSDIOレジスタをCMD52で叩くと、入力の体感遅延に効く
  可能性がある（`CONSOLE_SHELL.md`のUSB Serial/JTAG書き込みと同種の問題）。
  ポーリング間隔を測り、必要なら数十ms間隔に落とす

### Stage 6: microSDとの共存と文書化 ✅ 完了（実機確認済み）

- SDMMCコントローラをmicroSD（slot 0）とC6（slot 1）で共有するため、
  「どちらかの初期化がもう一方を壊さない」ことを実機で確認する。具体的には
  `wifiscan`成功後に`sdinfo`、その後もう一度`wifistatus`が通るか
- クロックはカードごとにディバイダを選べる（`SDHOST_CLKSRC_REG`）ので、
  slot 0とslot 1で別速度を選べるかを確認し、駄目なら共通の低い方に合わせる
- 現状文書`docs/WIFI.md`を新設し、`DESIGN.md`の「ドキュメント構成」の表と
  `docs/FILE_LAYOUT.md`のモジュール一覧、`docs/DIAGNOSTICS.md`の起動・診断ログに
  それぞれ追記する。`DESIGN.md`の「制約」にTCP/IP未実装を明記する
- `README.md`は人間が管理するファイルなので変更しない。記述が実態と合わなくなる
  場合は報告のみ行う

## プロトコル定数の参照先

実装時に値を確認する場所を残しておく。

| 内容 | 参照 |
| --- | --- |
| ペイロードヘッダ、フラグ | esp-hosted-mcu `common/esp_hosted_header.h` |
| インターフェース種別 | `common/esp_hosted_interface.h` |
| チェックサム、PRIVイベント、バッファサイズ | `common/transport/esp_hosted_transport.h` |
| INITイベントのTLVタグ、capabilityビット | `common/transport/esp_hosted_transport_init.h` |
| SDIOスレーブレジスタ、Vendor/Device ID | `host/drivers/transport/sdio/sdio_reg.h` |
| 受信長・送信バッファ数の算出、転送アドレス | `host/drivers/transport/sdio/sdio_drv.c` |
| SDIOカード活性化、CCCR操作 | `host/port/esp/freertos/src/port_esp_hosted_host_sdio.c` |
| ハンドシェイク（INIT受信→ホスト設定送信） | `host/drivers/transport/transport_drv.c` |
| SERIALのTLV | `host/drivers/virtual_serial_if/serial_if.c` |
| RPCのメッセージ定義とID | `common/proto/esp_hosted_rpc.proto` |
| 実装済みRPC一覧とバージョン | `docs/implemented_rpcs.md` |
| SDIOカード規格側のシーケンス | ESP-IDF v5.5.3 `components/sdmmc/sdmmc_io.c` |
| slot 1の信号インデックス | ESP-IDF v5.5.3 `components/soc/esp32p4/sdmmc_periph.c` |

## 実機での判断記録

（実機で分かったこと・踏んだ罠をここへ追記する）

### Stage 2実装時の判断（実機確認前）

- **32 bitレジスタはCMD53のバイトモードで読む**: 計画ではCMD52を4回に分ける
  つもりだったが、`PACKET_LEN`はスレーブがデータを積むたびに増えるカウンタで、
  4回に分けると下位バイトが古く上位バイトが新しい値を混ぜて読む可能性がある。
  この値はホスト側の読み出し位置の基準なので、一度ずれると以降の転送が全部
  ずれる。ESP-Hostedのホストも`sdmmc_io_read_bytes`（CMD53バイトモード）で
  4 byteをまとめて読んでいるので、同じにした
- **1回の読み出しに複数フレームが載る**: `PACKET_LEN`の差分はスレーブが積んだ
  バイト数の合計であって1フレーム分とは限らない。esp-hosted-mcuの
  `sdio_push_data_to_queue`も読み出したバッファを`offset + len`で切り分けながら
  複数パケットを取り出している。`Transport`はステージングバッファの未解析部分を
  持ち、使い切るまでバスに触らずに`receive`がフレームを1つずつ返す。
  復号に失敗したフレームは残り全体を道連れに捨てる（壊れた長さを信じない限り
  次のフレームの先頭が分からないため）
- **DMAバッファは64 byte境界に置く**: IDMACへ渡すバッファのキャッシュ操作は
  ラインまるごとに効くので、`#[repr(C, align(64))]`のラッパー型にした。
  4 byteのレジスタ用バッファも同じ理由でライン1本を専有させている
- **受信側のチェックサムは値が0なら検証しない**: ESP-Hostedのホストは
  チェックサムの有無をコンパイル時に決めるが、こちらはスレーブの
  capabilityビットを見るまで分からない。最初に受け取るINITイベント自体が
  その判定材料なので、「0なら未計算」として扱えばどちらのビルドにも追従できる
  （ヘッダだけで非0のバイトを含むため、正しいフレームの合計が0になることはない）
- **送信は常にチェックサムを入れる**: 検証しないスレーブは無視するだけなので、
  capabilityが分かる前でも安全側に倒せる

### 第4回実機試験: CMD53バイトモードがtimeout

`wifiup`の初回実行は、最初のスレーブレジスタ読み出しで
`SDIO: CMD53 byte mode timed out, RINTSTS=0x00020000`を繰り返した。

`RINTSTS`のbit17はカード1のSDIO割り込み（C6が「データがある」と言っている）で、
データ転送完了（DTO、bit3）もデータ系のエラービットも立っていない。つまり
コマンド自体は応答が返っている（応答エラーなら`send_command_on`が別に記録する）が、
**データ位相が始まっていない**。ハードウェアのデータタイムアウトは`TMOUT`の
上位24 bit＝0xFFFFFF card clock（20 MHzで約0.8秒）で、こちらのソフトウェア
タイムアウト（数十ms）の方が先に切れるため、DRTOも立たないまま抜けている。

2点変更した。

1. **`RINTSTS`の所有権を位相ごとに分けた**: `send_command_on`はコマンド完了時に
   `RINT_ALL`（0xFFFF）を書いて全ビットをクリアしていた。データ位相が短いと
   DTOがこのクリアで消え、待っている側が永久に取り逃す。コマンド位相のビット
   （command done／response error／CRC／timeout／hardware locked）だけを
   クリアするようにし、データ位相のビットは転送を仕掛ける直前に
   `data_transfer_on`がクリアする。今回の症状の主因である可能性は低い
   （クリアはcommand doneの直後、DTOはその1〜3 µs後に来る）が、
   クロックを上げるほど危なくなる構造なので直しておく
2. **32 bitレジスタの読み出しをCMD52×4へ戻した**: バイトモードが通らない以上、
   Stage 2をここで止める理由がない。torn readはStage 2の計画時に挙げた懸念
   なので、「2回続けて同じ値が読めるまで繰り返す」ガードを付けた。
   `PACKET_LEN`はスレーブがパケットを積んだときだけ変わるので、
   4コマンド（数十µs）の間に2回続けて変化することは実質ない

`data_transfer_on`のタイムアウト時に`STATUS`と`IDSTS`も出すようにした。
`STATUS`のFIFO_COUNT（bit29:17）が0なら「バスにデータが来ていない」、
非0なら「FIFOには来たがIDMACが吸い出していない」で切り分けられる。

**バイトモードが通らない理由は未解明のまま**である。パケット本体の転送は
ブロックモード（512 byte単位）で、これはmicroSDで動作実績のある経路と
同じ形なので、Stage 2の確認はブロックモードで進める。もしブロックモードの
受信も同じように止まる場合、次に疑うのはCCCR `0x04`で有効にしたカード割り込みで、
C6がDAT1を専有していると4 bit転送が読めなくなる可能性がある（こちらは
ポーリングなので、割り込みを無効にしても失うものはない）。

### 第5回実機試験: Stage 2完了

CMD52へ戻したレジスタ読み出しで初期化ハンドシェイクが完走した。

```
HOSTED: opening the data path
HOSTED: link is up
chip id: 0x0D (ESP32-C6)  firmware: 0.0.0
capabilities: 0x0D  extended: 0x00000000
slave queues: rx 20, tx 0  mode: packet
```

スレーブのINITイベントを受信し、ホスト設定を返送するところまで**双方向に**
通ったことになる。読み取れた事実:

- **chip id `0x0D`**でESP32-C6。CISはチップ由来なので分からなかった
  「ESP-Hostedのスレーブファームが動いている」ことがこれで確定した
- **capability `0x0D`** = bit0 WLAN over SDIO、bit2 BT over SDIO、bit3 BLE only。
  **チェックサムのbit7は立っていない**ので、このスレーブはヘッダの
  チェックサムを検証しない。ホスト設定フレームもチェックサム0で受理されており、
  「0なら未計算として扱う」という受信側の規則も含めて実機で裏が取れた
- **ファームウェアバージョンのTLVが無い**（`0.0.0`表示）。出荷ファームは
  `ESP_PRIV_FIRMWARE_VERSION`（タグ`0x17`）を送らない世代である。
  Stage 4・5で使うRPCはすべて0.0.6で追加されたものなので影響はない
- 拡張capabilityのTLVも無い。SPI HD／UART向けの情報なのでSDIOでは不要
- スレーブの受信キューは20バッファ。送信キューのTLVは0

なお`0x13`（raw throughputテスト）のTLVは値0で送られてくる。これは正常なので
「未対応タグ」として記録しないよう直した。

ブロックモードのCMD53は**まだ通していない**（INITイベントもホスト設定も
1フレームが512 byte未満で、ブロック1個ぶんの転送で済んでいる）。実際には
この経路が既に動いていることになるので、心配していたカード割り込みによる
DAT1の専有は起きていない。

### Stage 3実装時の判断（実機確認前）

- **`uid`が0の応答も受け入れる**: ホストは応答を`uid`で突き合わせるが、
  出荷ファームはバージョンTLVすら送らない世代なので、`uid`を返さない
  可能性がある。0が返ってきた場合はログを1行出したうえで`msg_id`だけで
  判定する。同時に1つのRPCしか投げないので、これで取り違えは起きない
- **応答は`Vec`で受ける**: スキャン結果は数KBになり得るのでフレーム長を超える。
  リクエスト側は1 KiBの固定バッファ（一番大きい`WifiSetConfig`でも数百byte）、
  応答側だけヒープを使う
- **応答待ちの間に来たイベントは捨てずに貯める**: RPCの応答を待っている間にも
  スレーブはイベントを送ってくる（ハンドシェイク直後の`Event_ESPInit`など）。
  ここで捨てるとStage 5の接続完了イベントを取りこぼすので、`Rpc`が
  イベント列として保持する。STAのデータフレームは数えて捨てる
- **protobufは手書き**: 使うのはvarint、length-delimited、入れ子だけで、
  floatもmapもpacked repeatedも登場しない。依存クレートを増やさずに済む。
  `int32`の負値が10 byteのvarintになる点だけ符号化側で明示的に扱う

### 第6回実機試験: Stage 3完了

`wifimac`が`RPC round trip completed`に到達した。TLVエンベロープ、
`Rpc`メッセージの組み立てと解析、SERIALインターフェース上の往復が
実機で通ったことになる。

`Rpc_Req_GetMacAddress`は`esp_wifi_get_mac`を呼ぶので、`esp_wifi_init`前は
スレーブがエラーを返す。Stage 3の合格条件は往復そのものなので、
`slave status`が非0でも問題ない（Wi-Fi初期化はStage 4の先頭で行う）。

実機の結果:

- `slave status = 12289` = `0x3001` = `ESP_ERR_WIFI_NOT_INIT`。想定どおりで、
  ステータスフィールドの復号も正しいことが確認できた
- `event 769 (0 bytes)` = `Event_ESPInit`。**イベント経路も動作**している
- `RPC: slave did not echo the uid`は出なかった。つまりこのスレーブは
  `uid`を返す世代で、応答の突き合わせは本来の形で機能している

### Stage 4実装時の判断（実機確認前）

- **`WifiInit`はESP-IDFの既定値一式を送る**: スレーブの`req_wifi_init`は
  自分の`WIFI_INIT_CONFIG_DEFAULT()`から始めるが、`get_merged_init_config`が
  **ほぼ全フィールドをホストの値で上書き**する（`magic`も含む）。
  ゼロで埋めると`esp_wifi_init`が弾くので、`static_rx_buf_num=10`、
  `dynamic_rx_buf_num=32`、`tx_buf_type=1`、`rx_ba_win=6`、
  `magic=0x1F2F3F4F`といった既定値を明示的に送る。
  `feature_caps`だけは0のままにした。スレーブ側のビルドと値が違う場合、
  スレーブは自分の値を使う（安全側）ため
- **スキャンは`block=true`、`config_set=0`**: スレーブは`config_set`が0なら
  `esp_wifi_scan_start(NULL, block)`を呼ぶので、スキャン設定を組み立てる
  必要がない。ブロッキングにすると応答がスキャン完了まで返らないので、
  スキャン完了イベントを待つ状態機械を作らずに済む
- **1回の読み出しは1フレームとは限らない（重要）**: フラグメントされた応答は
  複数フレームが連続して積まれ、`PACKET_LEN`の差分はその合計になる。
  Stage 2ではステージングバッファが1フレーム分（1536 byte）しかなく、
  超えた時点で「読めない＝カウンタを進められない＝以後永久に停止」だった。
  ステージングバッファをESP-Hostedの最大フラグメント長に合わせて
  8,704 byteへ拡大し、1フレームの上限（1536 byte、送信側の上限でもある）と
  1回の読み出しの上限を別の定数に分離した。スキャン結果は数KBになるので、
  これが無いとStage 4は最初の1回で確実に止まる

### 第7回実機試験: Stage 4完了

`wifiscan`が**27件**のAPを表示した。既知のAPが妥当なRSSI・チャンネルで並び、
27件ぶんの応答は1フレーム（1536 byte）に収まらないので、
**フラグメント再結合と複数フレームの一括読み出しも実地で通った**ことになる。

**5 GHz帯のAPは出ない。これはESP32-C6が2.4 GHz専用だからで、実装の問題ではない**
（Wi-Fi 6は2.4 GHzの802.11ax。5 GHzが要るならデュアルバンドのESP32-C5）。
`WifiSetBand`/`GetBand`のRPCはC5のような機種向けで、C6では意味がない。

### Stage 5実装時の判断（実機確認前）

- **C6のリンクをコマンドをまたいで保持する**: `wificonnect`の直後に
  `wifistatus`を実行する以上、コマンドごとにリンクを張り直すわけにいかない
  （張り直し＝C6のリセット＝接続の喪失）。`app::run`が
  `Option<wifi::Rpc>`を持ち、`shell::execute`へ渡す。`usb_host`と同じ形
- **`sd`で始まるコマンドはリンクを捨てる**: microSDとC6は1つのSDMMC
  コントローラを共有し、カードの活性化はコントローラごとリセットする。
  セッションを持ったままだと、`sdinfo`の後の`wifistatus`が理由の分からない
  失敗になる。ディスパッチの手前でセッションを捨て、その旨を1行表示する。
  本来の共存はStage 6の課題
- **`iface`は`WIFI_IF_STA`＝0**: スレーブの`req_wifi_set_config`は
  `iface`を`WIFI_IF_STA`/`WIFI_IF_AP`と比較する。`wifi_mode_t`（STA=1）とは
  別の列挙なので、`GetMacAddress`の`mode`（=1）と値が違う
- **`pmf_cfg.capable`は立てて送る**: 最近のIDFでは非推奨（APが対応していれば
  常にPMFを使う）だが、古いスレーブが読む可能性があり、PMFを断ると
  WPA3のAPに繋がらなくなる
- **接続完了はイベントで待つ**: `WifiConnect`の応答は「要求を受け付けた」まで。
  実際の結果は`Event_StaConnected`(775)か`Event_StaDisconnected`(776)で来るので、
  RPC層に「リクエストを送らずにイベントだけ待つ」`wait_for_event`を足した。
  待っている間に来た別のイベントは捨てずに貯める

### 第8回実機試験: 接続要求のあとC6がバスから消える

`wificonnect`が`connecting...`のあと、次のログを延々と繰り返した。

```
SDMMC: command failed, RINTSTS=0x00020104
SDMMC: command failed, RESP0=0x0000100E
SDMMC: command failed, STATUS=0x0001A106
SDMMC: command failed, response index=0x00000034
```

`RINTSTS`のbit8はresponse timeout、bit2はcommand done、bit17はカード1の
SDIO割り込み。`STATUS`のresponse index（bit16:11）は`0x34`＝**52**なので、
落ちているのは**CMD52（レジスタアクセス）**である。`STATUS`のdata_busyは0、
DAT3はHighのままなので、バスは待機状態で、C6だけが応答しなくなっている。

つまり**接続要求のどこかでC6がSDIOバスから消える**。スキャン（プローブ要求の
送信を含む）は成功しているので、単純な送信動作の問題ではない。

原因はまだ確定していない。この回では3つ入れた。

1. **リンク切れを事実として1回だけ報告する**: レジスタ読みが3回連続で失敗したら
   リンク切れと判定し、以後のポーリングを止める。ログが数千行に膨れて原因が
   埋もれるのを防ぐ。シェルは死んだセッションを捨て、次のコマンドで張り直す
2. **CMD5で切り分ける**: リンク切れ判定の直後にCMD5（IO_SEND_OP_COND）を1回送る。
   **CMD5に応答があればC6はリセットされてidle状態に戻っている**（＝クラッシュか
   ブラウンアウト）。CMD5にも応答が無ければ、電源が落ちているか
   スリープに入っている。`SDIO: C6 still answers CMD5`か
   `SDIO: C6 does not answer CMD5 either`のどちらが出るかで次の一手が決まる
3. **省電力を明示的に切る**: STAは既定で`WIFI_PS_MIN_MODEM`になり、
   アソシエート後はビーコン間で無線を止める。**眠るコプロセッサは
   バスに応答しないコプロセッサ**なので、`WifiStart`の後に
   `Req_SetPs`（270）で`WIFI_PS_NONE`を要求する。この firmware は電力管理を
   しないので、仮説が外れていても設定として正しい方向

どのRPCで落ちるかを特定するため、`set_config`と`connect`の送信直前に
UARTログを1行ずつ出すようにした。

未確認の可能性として、GPIO14（C6の`IO2`）がESP-Hostedの省電力機能で使う
ホスト⇔スレーブの起床線である線も残っている。IO2の用途はStage 0から
未解明のままで、もしスレーブが自発的にライトスリープへ入る構成なら、
この線で起こす必要がある。

### 第9回実機試験: 原因は「省略した入れ子メッセージ」

診断を入れた次の実行で切り分けが完了した。

```
WIFI: sending the station configuration
SDIO: C6 still answers CMD5, OCR=0x20FFFF00
connect: RPC failed, see UART log
the C6 link was lost; it will be rebuilt next time
```

- `WIFI: sending connect`が出ていないので、落ちるのは**`WifiSetConfig`**である
- **CMD5には応答する**ので、C6は電源が落ちたのでもスリープしたのでもなく、
  **リセットされてidle状態に戻っている**。つまりスレーブ側のクラッシュ
- 省電力（`WIFI_PS_NONE`）は無関係だった。GPIO14の起床線説も外れ

**教訓: 参照ホストが常に送る入れ子メッセージは、全フィールドが0でも送る。**

protobufの入れ子メッセージは、省略するとスレーブ側でヌルポインタになる。
最新のスレーブは`if (p_c_sta->threshold)`のようにガードしているが、
Tab5に載っている世代はガードせず`threshold->rssi`を読むため、
リクエストの処理中にコプロセッサごと落ちる。esp-hosted-mcuのホストは
`rpc_req.c`で`RPC_ALLOC_ELEMENT`により`threshold`と`pmf_cfg`を
**無条件に**確保しており、ホストが常に送るものは実質的に必須と考えるべきだった。

`wifi_sta_config`に`threshold`（フィールド9、値はすべて0）を追加した。
`pmf_cfg`（フィールド10）は最初から送っていた。

同じ理由で`WifiScanStart`が無事だったのも説明がつく。スレーブは
`config_set`が0なら`config`以下を一切参照しないので、入れ子を省略しても
ヌル参照に至らなかった。

### 第10回実機試験: 接続成功

`threshold`を足した次の実行で`wificonnect`がAPへのアソシエートまで完走し、
`connected to ...`／BSSID／チャンネルと認証方式、
`no IP address: there is no TCP/IP stack`まで表示された。

つまり`WifiSetConfig`→`WifiConnect`→`Event_StaConnected`(775)の待ち受けが
一通り動いている。Stage 3で確認したイベント経路がここで実際に使われた。

**結果**: `wifidisconnect`は正常、誤ったパスワードでは
`disconnected, reason 4`が出た（成功と失敗を判別できている）。
`wifistatus`だけは別の問題を踏んだ（次項）。

### 第11回実機試験: 接続後の`wifistatus`でデータ経路が固まる

```
SDIO: CMD53 timed out, RINTSTS=0x00020020
 timed out, STATUS=0x03A9AD00
 timed out, IDSTS=0x0000A000
SDMMC: command hard timeout, RINTSTS=0x00020400 （以後）
HOSTED: the C6 stopped answering, link lost
SDIO: C6 does not answer CMD5 either
```

今度は**ホスト側のデータ経路の停止**で、スレーブのクラッシュではない。

- `RINTSTS=0x00020020`はbit5（RXDR、受信FIFOにデータあり）だけでDTOが無い
- `STATUS=0x03A9AD00`はresponse index=`0x35`＝**CMD53**、
  fifo_count（bit29:17）=**468 word（1,872 byte）**、data_state_mc_busy=1。
  **FIFOにデータが溜まったままIDMACが吸い出していない**
- 以後のコマンドの`RINTSTS=0x00020400`はbit10＝**HTO（host starvation timeout）**。
  データ経路が止まったせいでCIUが次のコマンドを受け付けなくなっている
- この状態ではCMD5も通らないが、それはバスが固まっているためで、
  C6が消えたわけではない

原因はStage 4で入れた「報告された長さを1回のCMD53でまとめて読む」実装。
**ESP32のSDIOスレーブは1回の転送でスレーブ側バッファ1個ぶん（1,536 byte）までしか
返さない**ので、それより長く要求すると、カードが送り終えた後もコントローラが
残りを待ち続けてFIFOが詰まる。esp-hosted-mcuのホストがパケットモードで
`len > ESP_RX_BUFFER_SIZE`をエラーにしているのはこの制約のためで、
読み出しループが`ESP_SLAVE_CMD53_END_ADDR - data_left`という
「残量から逆算するアドレス」になっているのは分割読み出しのための形だった。

修正:

- **1回のCMD53は最大1,536 byte**にし、報告された長さをその単位で分割して読む。
  ウィンドウの終端は固定なので、各回のアドレスは`0x1F800 - 残量`。
  1,536は512の倍数なので、途中の分割はブロック境界を保ち、
  端数のパディングが要るのは最後の1回だけ
- **コマンドの最後にリンクを空読みする**（`Rpc::drain`）。アソシエート後は
  スレーブが受信フレームを送り続け、こちらにはIP層が無いので捨てるしかない。
  放置すると次のコマンドが巨大な滞留を一度に読むことになる

`wifi_err_reason_t`の1〜7も名前表に足した（reason 4は
`DISASSOC_DUE_TO_INACTIVITY`）。

### 第12回実機試験: Stage 5完了

分割読み出しと空読みを入れた版で`wifistatus`が接続中のSSID・BSSID・
チャンネル・RSSI（`channel N, -NN dBm`の形）を表示し、
`station frames received and dropped:`も出た。`wifidisconnect`と
誤ったパスワードでの失敗表示も確認済みで、Stage 5の到達条件をすべて満たした。

### Stage 6実装時の判断（共存は実機未確認）

- **コントローラのリセットは初回だけ**にした。従来は`init_host`が毎回
  ペリフェラルリセットとコントローラリセットを行っており、片方のカードを
  活性化するともう片方のホスト側設定（クロックイネーブル、分周器、バス幅）が
  消えていた。カード側の状態（RCA、選択、4bit、Function有効）はカードが
  持っているので、コントローラを壊さなければ再活性化は不要
- **共有する入力クロックを守る**: カードごとの分周器は別々に選べるが、
  その手前の`sdhost_cclk_in`は共通。C6が活性化済みの間はmicroSDの
  High Speed（40 MHz）切り替えを行わない。C6側の分周器は20 MHz入力を
  前提にしており、入力を倍にするとC6がHigh Speedを有効にしないまま
  40 MHzで駆動されるため。**それぞれの分周器を入力に追従させれば両立できる**が、
  そこまでの clock manager は今回作っていない
- シェルにあった「`sd`で始まるコマンドはC6のセッションを捨てる」という
  暫定の回避は削除した。上の2点で干渉しなくなっているはずなので、
  実機で確認する

**実機確認（済み）**: `wifiscan`→`sdinfo`→`wifistatus`の順に実行し、
`sdinfo`の後もC6のリンクが生きていることを確認した。**microSDとC6は
同時に使える**。

これで計画書の全Stageが完了。現状は[`WIFI.md`](WIFI.md)を参照。

### Stage 1実装時の判断（実機確認前）

- **カード番号の持たせ方**: `sdmmc.rs`の全関数にカード番号を通すのではなく、
  カード0前提の既存パス（`init`／`read_block`など）はそのまま残し、
  `start_command`／`set_card_clock`／`update_clock_registers`だけをカード番号付きに
  変更した。microSD側の挙動を変えずにカード1を足せるため。`sdio.rs`向けには
  `init_host`／`send_command_on`／`set_clock`／`set_host_bus_width_4bit`／
  `log_diagnostics`を公開している
- **クロック分周器の割り当て**: `SDHOST_CLKDIV_REG`は4本の分周器を持ち、
  `SDHOST_CLKSRC_REG`がカードごとにどれを使うかを選ぶ。ESP-IDFの
  `sdmmc_ll_set_card_clock_div`に合わせ、カード0は分周器0、カード1は分周器1を使う。
  `CLKENA`のcclk_enable／low-powerビットも対象カードのぶんだけ触るようにした。
  ただし`set_low_speed_clock_source`（`sdhost_cclk_in`）は共有なので、
  microSDとC6を同時に動かすときは共通値が必要になる（Stage 6）
- **リセット極性**: 回路図で未確認のため、ESP-Hostedの既定（`Kconfig`の
  "RESET: Active High" = 平常High、Lowパルスでリセット）をまず試し、
  失敗したら反転極性でもう一度活性化を試す実装にした。成功した側をUARTログに出す
  （`SDIO: C6 activated (reset line idles high/low)`）。実機で確定したら
  フォールバックは削除してよい
- **レジスタアクセスはCMD52のみ**: Stage 1ではCMD53のバイトモードDMAを使わず、
  CCCRもCISも1バイトずつCMD52で読む。データ経路を通さないぶん切り分けが簡単で、
  CIS読み出し（数十バイト）程度なら速度も問題にならない
- **High Speedは有効化しない**: CCCR `0x13`のサポートビットは読んで表示するが、
  クロックはmicroSDと同じ20 MHz（160 MHz / 8）に留めた

### 第1回実機試験: CMD5がresponse timeout

`wifiinfo`の初回実行はCMD52（I/Oリセット）とCMD5の両方が
`RINTSTS=0x00000104`（Command Done + Response Timeout）で、リセット極性を反転した
再試行も同じだった。`RESP0`は0、`STATUS=0x00000106`。

コントローラ側の設定は意図どおりだった。`CLKENA=0x00020002`はカード1の
cclk_enableとlow-powerのみ、`CLKSRC=0x00000004`はカード1が分周器1を選択、
`CLKDIV=0x00001400`は分周器1が20（160 MHz / 10 / 40 = 400 kHz）で、
`CTRL=0x00000030`はリセット解除済み。つまり「CIUはコマンドを送ったが、
CMD線に応答の開始ビットが来なかった」という状態で、切り分けるべきは
(a) C6が起動していない、(b) パッドに信号が出ていない、の2つ。

この結果を受けて3点変更した。

1. **パッドの出力イネーブル**: ESP-IDFはペリフェラル出力を
   ROMの`esp_rom_gpio_connect_out_signal`経由で配線しており、この関数は
   `OEN_SEL`を0にするだけでなく`GPIO_ENABLE`もセットする。レジスタの説明上は
   `OEN_SEL`=0でペリフェラル側のOEが使われるはずだが、初回実装は`GPIO_ENABLE`を
   立てておらず、これがCLKを含む全パッドが駆動されない原因になり得る。
   ESP-IDFと同じく両方を設定するようにした（現時点では最有力の仮説であって、
   確認はこれから）
2. **起動待ちの延長**: リセット解除後の待ちを500 msから1,500 ms
   （ESP-Hostedの`H_HOST_SDIO_RESET_DELAY_MS`既定値）にし、さらにCMD5を
   100 ms間隔で15回まで再試行するようにした。初回実装は1回で諦めていた
3. **診断の追加**: 活性化の前に、E2のdirection／hi-z／output／inputレジスタと、
   6本のパッドを素のGPIO入力にしたときのレベル（内部プルアップ無し／有りの2通り）を
   UARTへ出す。C6側基板が給電されていれば外部プルアップでCMD／D0..D3がHighに
   なるはずなので、「プルアップ無しでLow」なら電源かパッドのルーティングを、
   「プルアップ有りでのみHigh」なら相手側が無反応であることを示す

### 第2回実機試験: パッドは生きている／D3のSDモードストラップ

診断の結果は`pad levels without pull-up=0x3F`（CMD・D0..D3・CLKの6本すべてが
内部プルアップ無しでHigh）で、外部プルアップが効いていた。つまりGPIO8..13は
想定どおりの場所に出ており、パッド自体もHP GPIO側から読める。一方
`E2 input=0x20`はP0がLowのままで、E2への書き込みはACKされているのに
ピンがHighになっていない（要追加調査。ただしバスがプルアップされている以上、
C6側のIO電源そのものは来ている可能性が高い）。

ここでESP-IDFの`sdmmc_host_init_slot`を読み直したところ、**D3を初期化中は
ペリフェラルへ繋がず、素のGPIO出力としてHighに固定している**ことが分かった
（コメントは"Force D3 high to make slave enter SD mode"）。D3はカードが
識別中に見るSD／SPIモード選択線を兼ねており、Lowだとカードはネイティブの
SDモードではなくSPIモードへ入って一切応答しなくなる。ペリフェラルへ渡すのは
`sdmmc_host_set_bus_width(slot, 4)`の中、つまりバス幅を4bitにした後である。

初回実装はD3を最初からスロット1のD3信号へ配線しており、しかも上記の
`GPIO_ENABLE`変更で「ペリフェラルのOEが出ていなければ`GPIO_OUT`の0が出る」形に
なっていたため、D3がLowに落ちていた可能性がある。ESP-IDFと同じ順序に修正した。

- `gpio::configure_c6_sdio_pins`はD3を`configure_push_pull_output`＋High固定にし、
  CLK／CMD／D0..D2だけをペリフェラルへ配線する
- 4bit化（CCCR書き込み＋`CTYPE`）の直後に`gpio::connect_c6_sdio_data3`でD3を
  ペリフェラルへ渡す
- 併せて、パッドに繋がっていないcard detect／card interrupt／write protectの
  各入力を、ESP-IDFと同じ定数（detect=Low＝カード有り、interrupt=High、
  write protect=Low）へ結線した

### 第3回実機試験: 活性化成功

D3の修正でC6が応答するようになり、活性化が完走した。

```
SDIO: CMD5 probe OCR=0x20FFFF00
SDIO: switched to 4-bit bus width
SDIO: C6 activated (reset line idles high)
SDIO: RCA=0x00000001
RCA: 0x0001  I/O functions: 2  memory: no
bus width: 4-bit
clock: 20000 kHz  High Speed: supported (not enabled)
```

- `OCR=0x20FFFF00`はbit30:28が2（I/O関数が2個）、bit27が0（メモリ部なし）、
  電圧ウィンドウ`0x00FFFF00`。SDIO専用カードとして正しい応答である
- **リセット極性は「平常High、Lowパルスでリセット」**（ESP-Hostedの既定と同じ）で
  確定した。反転側のフォールバックは削除してよい
- 冒頭に1回だけ残る`RINTSTS=0x00000104`はCCCRのI/Oリセット（CMD52）に対する
  もので、カードが自らリセットするため応答が返らないのは正常。ESP-IDFの
  `sdmmc_io_reset`も同じ扱いをしている
- E2は`direction=0x09`・`hi-z=0xF6`・`output=0x09`で、P0は出力・非ハイ
  インピーダンス・Highに設定できている。それでも`input=0x20`はP0をLowと読む。
  バスは給電されておりC6も動いているので、P0はC6の電源スイッチの制御入力
  （読み戻しが自身の駆動値を返さない配線）と考えるのが自然。実害はないため
  Stage 2以降で必要になったときに再確認する

**CISの識別子だけ想定と違った**: 読めた`CISTPL_MANFID`は
manufacturer=`0x0092`、product=`0x6666`だった。生ダンプで確認したところ
パース側が正しく、期待値の方が誤りだった。

```
01000: 01 03 D9 01 FF 20 04 92 00 66 66 21 02 0C 00 22
01010: 04 00 00 02 32 1A 05 01 01 00 02 07 1B 08 C1 41
01020: 30 30 FF FF ...
```

- `0x01000`: `01 03` = CISTPL_DEVICE、データ3 byte
- `0x01005`: `20 04` = **CISTPL_MANFID**、データ`92 00 66 66`
  → manufacturer=`0x0092`、product=`0x6666`
- `0x0100B`: `21 02` = CISTPL_FUNCID、データ`0C 00`（`0x0C` = SDIO）
- `0x0100F`: `22 04` = CISTPL_FUNCE（function 0）、データ`00 00 02 32`
  → 最大ブロックサイズ`0x0200` = 512 byte、最大転送レート`0x32` = 25 MHz
- `0x01015`: `1A 05` = CISTPL_CONFIG、`0x0101C`: `1B 08` = CISTPL_CFTABLE_ENTRY、
  `0x01026`: `FF` = CISTPL_END

タプル鎖は完全に整合しており、`0x0092`／`0x6666`がこのC6の実際の値である。
ESP-Hostedの`sdio_reg.h`にある`ESP_VENDOR_ID=0x6666`／
`ESP_DEVICE_ID_1=0x2222`は、esp-hosted-mcuのホスト側コードからは参照されて
おらず、Linux版ドライバの見え方を書いたものと思われる。`sdio.rs`は
`ESP_SDIO_IDENTITIES`として`(0x0092, 0x6666)`と従来の2組を併記し、
どれかに一致すればESPスレーブと判定する。一致しない場合だけCISの生ダンプを
UARTへ出す。

CISはチップ側のものなので、この判定が言えるのは「C6が応答している」までで、
ESP-Hostedファームが載っているかどうかはStage 2のINITイベントで確認する。
