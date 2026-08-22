# TCP/IPスタック（smoltcp）実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: **完了**（全Stage実機確認済み）

コードと文書はStage 0〜7まで入っており、ビルドと`tools/check_elf_layout.py`は
通ります。現状文書は[`NETWORK.md`](NETWORK.md)です。

以前の実機試験では送信したフレームへの応答がありませんでした。調査の結果、
`GetMacAddress`要求へWi-Fiモード値`WIFI_MODE_STA = 1`を渡し、SoftAP側のMACを
IPインタフェースへ設定していたことが判明しました。STAインタフェース値
`WIFI_IF_STA = 0`へ修正後、実機でDHCPリース取得とP4・PC間の双方向pingに
成功しました。詳細は下の「実機での判断記録」を参照してください。

| Stage | 状態 |
| --- | --- |
| 0 `IF_STA`の中身確認 | **実機確認済み**（802.3で確定） |
| 1 SYSTIMERのTick | **実機確認済み**。`uptime`とストップウォッチの照合済み。加えて`pump_until`がタイムアウトして戻ること自体がティックの前進を示す（止まっていれば`now_ms()`が0のまま無限ループする） |
| 2 依存の追加とビルド | 確認済み |
| 3 `Device`と固定IPでのICMP | **実機確認済み**。PCからP4、P4からPCの双方向pingに成功 |
| 4 DHCP | **実機確認済み**。`ipconfig dhcp`でリース取得に成功 |
| 5 TFTP | **実機確認済み**。3ファイルでサイズとCRC-32が一致し、存在しないファイルはエラーコード1 |
| 6 TCP | **実機確認済み**。`httpget`でヘッダ表示・512 KiBの本文受信・404のいずれも成功 |
| 7 文書化 | 完了 |

**この計画は完了です。** 到達目標（DHCP・ping・TFTP・HTTP）はすべて実機で確認しました。非目標のまま残しているのはDNS解決・IPv6・TLS・サーバ機能・受信データの保存先で、TFTPのblksize拡張（RFC 2348）も入れていません。以降の作業は新しい計画書を立てて進めます。

## 方針

[`WIFI.md`](WIFI.md)のとおり、ESP32-C6経由でAPへアソシエートするところまでは
動いています。この計画はその上にIP層を載せ、**DHCPでアドレスを取得し、
TFTPでファイルを転送できる**ところまでを作ります。

**プロトコルスタックは自前実装せず、[smoltcp](https://docs.rs/smoltcp)を使います。**
このリポジトリはハードウェアを触る層を自前で実装する方針ですが、その理由は
「ESP-IDFやベンダHALの中で何が起きているか分からなくなるのを避ける」ことにあり、
RFCで完全に規定され、外から観測できるプロトコル層には当てはまりません。
自前実装との比較は次のとおりです。

| | 自前実装 | smoltcp |
| --- | --- | --- |
| ARP/IPv4/ICMP/UDP/DHCP | 約1,400行 | 0行 |
| TCP | さらに約1,200行 | 0行 |
| こちらが書く量 | 上記＋TFTP約300行 | `Device`実装とポンプで約250行＋TFTP約300行 |
| 実機での試行回数 | UDP系で6〜10回、TCPを含めると20回規模 | 各段階1〜2回 |
| 主なリスク | TCPの再送・ウィンドウ・順序外の相互運用 | 依存が1つ増えること |

TCPの相互運用バグは「たまに200 ms止まる」「相手のfast retransmitと噛み合わない」
という形で出るため、1回の試行がフラッシュと手動操作を伴うこの環境では
デバッグ費用が支配的になります。smoltcpは`no_std`前提で枯れており、
TCPが**副産物として付いてくる**ので、TFTPまでで止めるつもりでも選択肢として有利です。

依存の追加は`linked_list_allocator`・`riscv-rt`に次ぐ3つ目です。`std`を引き込まない
よう`default-features = false`で使います。

## 到達目標と非目標

到達目標:

- `ipconfig` — DHCPでアドレス・ゲートウェイ・DNSサーバを取得して表示
- `ping <ip>` — ICMP echoの送信と往復時間の表示。こちら宛のechoにも応答する
- `tftpget <server> <file>` — TFTPで読み出し、サイズとCRCを表示
- `httpget <ip> <path>` — TCPの動作確認用の最小HTTP GET（smoltcpに付いてくるので）

非目標:

- IPv6、DNS解決（`socket-dns`を有効にすれば足せるが今回は入れない）
- TLS、サーバ機能（TFTPサーバ、HTTPサーバ）
- ファイルシステム。TFTPで受けたデータの保存先は当面メモリ上だけで、
  SDカードへ書くのは[`STORAGE.md`](STORAGE.md)側の課題

## 前提（Stage 0で確認する）

- **`IF_STA`のペイロードは802.3イーサネットフレーム**であること。ESP-Hostedの
  ホスト側TXは`esp_netif`のtransmitコールバックで、Wi-Fiステーション用netifが
  渡すのは14 byteヘッダ付きのイーサネットフレームである（802.11との変換は
  C6側のWi-Fiドライバが行う）。**この前提が崩れると計画全体が変わる**ので、
  受信フレームの先頭16 byteをダンプして自分のMACとethertypeを確認してから進む
- MTUは1500。ESP-Hostedのフレーム最大ペイロードは1,524 byteなので、
  イーサネットフレーム1,514 byteはそのまま載る

## モジュール構成（案）

| 追加/変更 | 責務 |
| --- | --- |
| `src/net.rs`・`src/net/`（新規） | `usb.rs`・`wifi.rs`と同じく親は宣言と再エクスポートだけ |
| `src/net/device.rs`（新規） | smoltcpの`phy::Device`実装。`IF_STA`フレームとトークンの橋渡し |
| `src/net/stack.rs`（新規） | `Interface`・`SocketSet`・DHCPソケットの保持、ポンプ、アドレス確定時のルート設定 |
| `src/net/tftp.rs`（新規） | UDPソケット上のTFTPクライアント |
| `src/net/ping.rs`（新規、案からの変更） | ICMP echoの送信と往復時間 |
| `src/net/http.rs`（新規、案からの変更） | 最小のHTTP/1.0 GET |
| `src/wifi/rpc.rs`（変更） | STAフレームを数えて捨てるのをやめ、上限付きのキューへ積む |
| `src/tick.rs`（新規） | SYSTIMERの周期割り込みによる1 kHzのTickと、そこから作る単調な`now_ms()` |
| `src/interrupts.rs`（変更） | Tick用のCLIC線とISVの追加 |
| `src/app.rs`（変更） | フレームループからネットワークをポンプする |
| `src/app/shell.rs`（変更） | 上記コマンド |

## smoltcpの設定

```toml
smoltcp = { version = "0.14", default-features = false, features = [
    "alloc",
    "medium-ethernet",
    "proto-ipv4",
    "proto-dhcpv4",
    "socket-dhcpv4",
    "socket-udp",
    "socket-icmp",
    "socket-tcp",
    "auto-icmp-echo-reply",
] }
```

- `default-features = false`は必須。既定には`std`・`phy-raw_socket`・
  `phy-tuntap_interface`などホスト向けの機能が入っている
- `alloc`はグローバルアロケータがあるので有効にしてよい。ソケットバッファを
  PSRAMヒープから取れる
- `log`・`defmt`は無効のまま。ログは`uart.rs`の既存の接頭辞方式に合わせる
- `auto-icmp-echo-reply`があるとStage 3の確認が`ping`一発で済む
- コードサイズはFLASH XIP領域（`ROM_TEXT`は約3.9 MiB）に対して十分小さく、
  ソケットバッファもPSRAMヒープ（約30 MiB）なので、どちらも制約にならない

## 段階分け

### Stage 0: `IF_STA`フレームの中身確認（実機確認済み）

一時的な診断ではなく`netdump`コマンドとして入れた。上の層が答えないときに
「線には何が来ているのか」を見る場所は残しておく価値があるため。
APに接続した状態で`netdump`を実行し、数フレーム観測する。

**実機確認**: 先頭6 byteが自分のMACまたはブロードキャスト（`FF:FF:FF:FF:FF:FF`）、
次の6 byteが送信元MAC、13〜14 byte目が`0806`（ARP）か`0800`（IPv4）であること。
ARPの要求がブロードキャストで飛んでくるので、接続直後に必ず観測できる。

### Stage 1: タイマー割り込みによるTick（**実機確認済み**）

秒単位の時間はタイマー割り込みのTickで測る。`src/tick.rs`を新設して
`now_ms()`を提供する。**ネットワーク専用の仕組みではなく汎用の時刻源**であり、
最初の利用者は`uptime`コマンドである。

**なぜサイクルカウンタを使わないか**: `delay.rs`の`cycle_count`は`rdcycle`の
下位32 bitだけを読んでおり、360 MHzでは**約11.9秒で一周**します。短いビジー
ウェイトには十分だが、秒単位の測定やsmoltcpの再送タイマの基準には使えない。
`delay_ms`／`delay_us`は現状のまま残し、Tickは別の役割として足す。

実装:

- **SYSTIMER**（ESP32-P4は52 bitカウンタ2本、コンパレータ3本、クロックは
  XTAL 40 MHzを固定分周2.5で**16 MHz**、レベル割り込み）を使う。
  unit 0を自走させ、comparator 0を周期モード（`SYSTIMER_TARGET0_PERIOD_MODE`）で
  16,000ティック＝**1 kHz**に設定する。`TARGET0_PERIOD`は26 bit幅なので余裕がある
- 割り込みソースは`ETS_SYSTIMER_TARGET0_INTR_SOURCE`＝**53**（計画時に54と
  書いていたのは誤り。下の判断記録を参照）。CLIC線は
  表示（線1）とUSB（線2）の下に置く。ISRは「カウンタを進めて
  `SYSTIMER_INT_CLR_REG`を書く」だけなので、表示のアンダーランに影響しない
- 時刻はミリ秒の64 bit値として持つ。RV32には64 bitのアトミックがないので、
  上位・下位を`AtomicU32`2本に分け、**書き手はISRだけ**にする。読み手は
  上位→下位→上位の順に読み、上位が変わっていたら読み直す
- 1 kHzなら下位32 bitだけでも49.7日もつので、上位は保険

**実機確認**: `uptime`をフレーム数の概算から`now_ms()`に切り替え、
1分間ストップウォッチと突き合わせて誤差が1秒未満であること。
`SYSTIMER_INT_RAW_REG`を見る診断も足し、割り込みが取りこぼされていないこと
（Tickの進みが実時間と一致すること）を確認する。

**想定される罠**:

- SYSTIMERはバスクロックのゲートとクロック源選択が別レジスタ
  （`HP_SYS_CLKRST`側）にある。`psram.rs`や`sdmmc.rs`と同じく、
  ペリフェラルのクロック投入を忘れるとレジスタが書けても動かない
- レベル割り込みなので、ISRで`INT_CLR`を書かないと割り込みが張り付く
- 周期モードの設定後は`SYSTIMER_COMP0_LOAD_REG`へ書いて反映させる必要がある

### Stage 2: 依存の追加とビルド確認（確認済み）

`Cargo.toml`へsmoltcpを追加し、上記のfeatureでビルドが通ることだけを確認する。
`cargo build --release`のバイナリサイズと、`tools/check_elf_layout.py`の
検査が通ることを見る。コードはまだ書かない。

**実機確認**: 既存の機能が今までどおり動くこと（回帰確認）。

### Stage 3: `Device`実装と固定IPでのICMP応答（**実機確認済み**）

- `wifi/rpc.rs`のSTAフレームを上限8フレーム程度のキューへ積むよう変更する
  （溢れたら古い方を捨てて数える。今の「数えて捨てる」の置き換え）
- `net/device.rs`に`phy::Device`を実装する。`receive`はキューから1フレーム取り、
  `transmit`は`Transport::send(IF_STA, ...)`へ流すトークンを返す。
  `capabilities`はMTU 1500・`Medium::Ethernet`
- 借用の形: `Rpc`が`Transport`を持ったままにし、`StationDevice<'a>`が
  `&'a mut Rpc`を借りる。`iface.poll`のたびにデバイスを作って捨てれば、
  RPC呼び出しとデバイスの可変借用が衝突しない
- `net/stack.rs`で`Interface::new`する。`Config`のハードウェアアドレスは
  既に実装済みの`GetMacAddress`から取る。乱数シードはサイクルカウンタでよい
- **アドレスは当面固定**（`ipconfig`のDHCPはStage 4）。APのサブネットに合わせた
  値をコマンド引数で受ける

**実機確認**: PCから`ping`が返ること。`auto-icmp-echo-reply`があるので、
ARP応答とIPv4受信とICMP応答が一度に検証できる。**このStageが山場**で、
ここが通れば残りは積み上げになる。

**想定される罠**:

- 時刻はStage 1の`tick::now_ms()`を使う。smoltcpの`Instant`は単調で
  あることが前提で、巻き戻ると再送タイマが壊れる
- ポーリングを怠るとC6側にフレームが溜まり、1回の読み出しがステージング
  バッファを超える（`WIFI_C6_PLAN.md`の第11回試験と同じ壊れ方）。
  `app.rs`のループは表示待ちで約17 ms周期なのでDHCPやTFTPには足りるが、
  シェルコマンドの実行中は止まる。長い処理は自前のポンプループを回す

### Stage 4: DHCPクライアント（**実機確認済み**）

`socket-dhcpv4`のソケットを`SocketSet`へ追加し、`poll`が返す
`Event::Configured`でアドレス・ルータ・DNSを取り出して
`iface.update_ip_addrs`と`routes_mut`へ反映する。`Event::Deconfigured`では
アドレスを外す。`ipconfig`コマンドで現在の設定とリース状態を表示する。

**実機確認**: `wificonnect`の後に`ipconfig`でアドレスが取得でき、
そのアドレス宛のpingがPCから通ること。

### Stage 5: TFTPクライアント（**実機確認済み**）

`net/tftp.rs`をUDPソケットの上に実装する。RRQ→DATA/ACKの512 byte
ロックステップ、タイムアウトと再送、ERRORパケットの解釈まで。
オプション拡張（RFC 2348のblksize等）は入れない。

**実機確認**: PC側でTFTPサーバを立て、数百KiBのファイルを取得して
サイズとCRCが一致すること。

**想定される罠**:

- **サーバは最初の応答を別のエフェメラルポートから返す**。RRQの宛先は69番だが、
  以降のやり取りはサーバが選んだポートに対して行う。最初のDATAの送信元ポートを
  覚えて、以後そこへACKを送る
- 最終ブロックは512 byte未満のDATA。ちょうど512の倍数で終わるファイルは
  長さ0のDATAで終端する
- ロックステップなので往復遅延がそのまま速度になる。SDIOのポーリングと
  合わせると数十〜数百KiB/s程度を見込む

### Stage 6: TCPの動作確認（**実機確認済み**）

smoltcpに付いてくるので、`httpget <ip> <path>`で最小のHTTP/1.0 GETを投げ、
応答の先頭を表示する。TCPソケットのバッファは8 KiBずつ程度。

**実機確認**: LAN内のHTTPサーバから応答が取れること。

### Stage 7: 文書化（完了）

- 現状文書`docs/NETWORK.md`を新設し、`DESIGN.md`の「ドキュメント構成」表と
  「制約」（TCP/IP未実装の記述を実態に合わせる）、`docs/FILE_LAYOUT.md`、
  `docs/DIAGNOSTICS.md`（`NET:`接頭辞）を更新する
- `tick.rs`はネットワーク専用ではないので、`FILE_LAYOUT.md`には独立した
  時刻源として、割り込みの割り当ては`DIAGNOSTICS.md`か`BOOT.md`側に書く
- `README.md`は人間が管理するファイルなので変更しない。古くなる箇所
  （「未対応」のTCP/IPスタックの行、シェルコマンド表）は報告にとどめる

## 参照

| 内容 | 参照 |
| --- | --- |
| `Device`／`RxToken`／`TxToken` | <https://docs.rs/smoltcp/0.14.0/smoltcp/phy/trait.Device.html> |
| `Interface`（`new`／`poll`／`poll_at`／`update_ip_addrs`／`routes_mut`） | <https://docs.rs/smoltcp/0.14.0/smoltcp/iface/struct.Interface.html> |
| DHCPソケット（`Event`／`Config`） | <https://docs.rs/smoltcp/0.14.0/smoltcp/socket/dhcpv4/index.html> |
| UDPソケット（`PacketBuffer`／`bind`／`send_slice`／`recv_slice`） | <https://docs.rs/smoltcp/0.14.0/smoltcp/socket/udp/struct.Socket.html> |
| TFTP | RFC 1350（本体）、RFC 2347/2348（オプション、今回は不使用） |
| SYSTIMER | ESP-IDF v5.5.3 `components/soc/esp32p4/register/hw_ver1/soc/systimer_reg.h`、`components/hal/esp32p4/include/hal/systimer_ll.h`、割り込み番号は`components/soc/esp32p4/include/soc/interrupts.h` |
| 下層の現状 | [`WIFI.md`](WIFI.md)、[`WIFI_C6_PLAN.md`](WIFI_C6_PLAN.md) |

## 実装時の判断記録

実機での確認はまだ行っていないので、ここにあるのは**コードを書く過程で
分かったこと**だけである。実機で踏んだ罠は確認後に追記する。

### 割り込みソース番号は54ではなく53

計画には`ETS_SYSTIMER_TARGET0_INTR_SOURCE`＝54と書いていたが、ESP-IDF
v5.5.3の`components/soc/esp32p4/include/soc/interrupts.h`の列挙を
`ETS_LP_UART_INTR_SOURCE = 16`から数え直すと**53**である。既に実装済みの
`ETS_DW_GDMA_INTR_SOURCE`＝24と`ETS_USB_OTG_INTR_SOURCE`＝93が同じ数え方で
一致するので、数え方のほうが正しい。1つずれた線に配線すると、割り込みは
永久に来ないか、LEDCの線（52）を奪うことになる。

### CLICの`clicintctl`は「上位3 bitがレベル」

計画では「表示（線1）とUSB（線2）の下に置く」と書いたが、ESP32-P4のCLICは
`cliccfg.nlbits`＝3で設定されており、`clicintctl`の**上位3 bitがレベル**、
残り5 bitがレベル内の優先度である。しきい値は`0x1F`（レベル0）なので、
表示の`0x3F`もUSBの`0x20`もレベル1、しきい値より上で通る。最初ティックに
`0x10`を書いたが、これはレベル0になり**しきい値に潰されて割り込みが一度も
来ない**。USBと同じ`0x20`（レベル1の最下位優先度）にした。レベル内では
互いにプリエンプトしないので、「下に置く」は実現できない。ティックのISRが
2ロードとストアとレジスタ書き込みだけであることのほうが重要である。

### SYSTIMERはリセットしない

ESP-IDFの`systimer_hal_init`はレジスタファイルをリセットするが、ここでは
行わない。ROMやブートローダがSYSTIMERを使っている可能性を監査していないため、
リセットするより「依存するレジスタを全部明示的に書く」ほうが影響が閉じる。
書いているのは`CONF`・`TARGET0_CONF`・`COMP0_LOAD`・`INT_ENA`・`INT_CLR`と、
`HP_SYS_CLKRST`側のバスクロック・クロック源。既定値のままでよいものも含めて
書くのは、ブートローダが残した状態への依存を断つため。

### 64 bitミリ秒は「上位を先に繰り上げる」

計画どおり`AtomicU32`2本に分けたが、**繰り上げの順序が結果を変える**。
下位を0に戻してから上位を繰り上げると、その間に読んだ側は上位が変わらないまま
下位0を見て、2^32 ms（約49.7日）過去の時刻を返す。上位を先に繰り上げれば、
読み手の「上位→下位→上位」検査が必ず不一致を検出して読み直す。

### `max_burst_size`は申告しない

`DeviceCapabilities::max_burst_size`はデバイスのバースト能力の申告に見えるが、
smoltcpは`iface/packet.rs`でこれを**TCPウィンドウの上限**
（`max_burst_size * max_segment_size`）として使う。SDIOが1フレームずつしか
運ばないことを正直に`Some(1)`と書くと、全TCP接続が1 MSSインフライトに
制限される。未設定のままにする。

### 受信キューの背圧は「読まない」で作る

キューの深さは計画の「8フレーム程度」ではなく**32フレーム**にした。
1フレーム約1.5 KiBで48 KiB、約30 MiBのヒープに対して無視できる一方、
バーストで溢れる確率は目に見えて下がる。

一度トランスポートから読んだフレームは戻せないので、キューが満杯のときに
できる非破壊な唯一の対処は**バスを読まないこと**である。読まなければ
ステージングバッファに残る。ただしRPC応答を待つ経路がこれをやると、誰も
キューを捌いていない状況（スタックを作る前の`wificonnect`など）で永久に
待つことになるので、そちらは最も古いフレームを捨てる。`Rpc::next_message`が
`hold_when_full`を取るのはこの2つを分けるため。

### `Device::receive`は受信と送信のトークンを同時に返す

smoltcpの`phy::Device`は`receive`で`(RxToken, TxToken)`を返す。どちらも
`&mut self`から作るので、片方がデバイスを借りたままだともう片方が作れない。
受信トークンにフレームを**所有**させる（`Vec<u8>`をキューから取り出す）と
借用が1つで済み、`&'a mut Rpc`の再借用を送信トークンへ渡せる。

### `Stack`はC6のリンクを所有しない

計画の借用の形をそのまま採用した。`Interface`と`SocketSet`は`Stack`が持ち、
`Rpc`はトランスポートを持ったままで、パケットを触る呼び出しがその都度
`&mut Rpc`を借りる。`Interface::connect`だけは例外で、インタフェースの
コンテキストとソケットの両方を同時に必要とするため`Stack::connect_tcp`として
内側に置いた（外から`&mut Stack`の2つのフィールドを分けて借りられない）。

### DHCPイベントはソケットセットを借りたままでは処理できない

`dhcpv4::Socket::poll`が返す`Event`はソケットセットを借りている。そのまま
`update_ip_addrs`や`self.config`を触ろうとすると、`&mut self`全体を要求する
呼び出しと衝突する。必要な値（アドレス・ルータ・DNS・サーバ）を先に
コピーしてイベントを落とし、それから適用する。

### コードサイズ

smoltcp導入前後で、IROMが260,142→334,900 byte（約+73 KiB）、DROMは
130,776 byteで変化なし、IRAMは9,296→9,352 byte。`ROM_TEXT`約3.9 MiBに対して
十分小さい。

## 実機での判断記録

### Stage 0: `IF_STA`は802.3で確定

`netdump`で観測できたのは mDNS のマルチキャスト（`01:00:5E:00:00:FB`）、
IPv6 の全ノードマルチキャスト（`33:33:00:00:00:01`）、ベンダ独自の
ブロードキャスト（ethertype `0x8899`）など。**宛先6 byte・送信元6 byte・
ethertype という並びがそのまま出ており、前提は満たされている。**

アドレスを設定する前は自局宛のフレームは1つも来ない。IPを持っていない
ステーションを名指しする理由がネットワーク側に無いためで、これは異常ではない。
最初これを「期待した通信が無い」と読んでしまったので、`netdump`は宛先を
自局宛／ブロードキャスト／マルチキャストに分類して表示し、自局宛が0のときは
その旨を出すようにした。

### STA MAC取得では`wifi_interface_t`を使う

無線を持つのはP4ではなくC6なので、`GetMacAddress(WIFI_IF_STA)`が返す
アドレスは本体ラベルのMACと一致しない。`netdump`が先頭で表示する。

このRPCのprotobufフィールドは`mode`という名前だが、C6側は要求値を
`wifi_interface_t`として`esp_wifi_get_mac`へ渡す。STAは0、SoftAPは1である。
以前は`wifi_mode_t`の`WIFI_MODE_STA = 1`を渡していたため、SoftAP側のMACを
STAのIPインタフェースに使っていた。定数を`WIFI_IF_STA = 0`へ修正した。

### 送信側は外から見えない ── カウンタとダンプを足した

「PCからのpingが返らない」ときに、こちら側で観測できるものが何も無かった。
C6が送らなかったフレームは、相手から見ると「そもそも生成されなかった
フレーム」と完全に同じに見える。そこで足したもの:

- `ipconfig`の`rx queued/delivered/dropped`と`tx sent/throttled/failed`
- `netdump tx` ── 直近8フレームのヘッダと、送信元MACが自局のものかの検査
- `ipconfig`の1行目にアソシエート状態、コマンド後に切断イベントを理由コード付きで報告

### スレーブは未接続の間データフレームを黙って捨てる

`slave/main/esp_hosted_coprocessor.c`の`process_rx_pkt`:

```c
if (buf_handle->if_type == ESP_STA_IF && station_connected) {
        ret = esp_wifi_internal_tx(WIFI_IF_STA, payload, payload_len);
} else if (...)
```

`station_connected`が false だとどの分岐にも入らず捨てられる。一方こちら側では
smoltcpはフレームを作り続け、SDIOへの書き込みも成功するので`tx sent`は増える。
**切断とネットワークの無反応が、送信カウンタまで含めて同じ症状になる。**
このため`ipconfig`はアソシエート状態を最初に出す。

### 参照実装と一致することを確認した項目（原因から除外済み）

| 項目 | 参照 | こちら |
| --- | --- | --- |
| `ESP_STA_IF` | `common/esp_hosted_interface.h`で1（`ESP_INVALID_IF`が0） | `IF_STA = 1` |
| ヘッダ | `len`／`offset=12`／`if_type`／`if_num`／`seq_num`／`flags` | 同一 |
| チェックサム | ヘッダ＋ペイロード、checksum欄を0にして計算 | 同一 |
| 書き込みアドレス | `CMD53_END_ADDR - data_left`（**未パディング長**）、転送長は512境界へ切り上げ | 同一 |
| `CMD53_END_ADDR`／`BLOCK_SIZE`／`RX_BUFFER_SIZE`／`TX_BUFFER_MAX` | `0x1F800`／512／1536／`0x1000` | 同一 |
| `TAG_HOST_CAPABILITY` | スレーブの`host_to_slave_reconfig`は**ログに出すだけ**で何にも使わない | 0を送って問題ない |

### C6のファームウェアは相当古い

`wifiup`の出力:

```text
chip id: 0x0D (ESP32-C6)  firmware: 0.0.0
capabilities: 0x0D  extended: 0x00000000
slave queues: rx 20, tx 0  mode: packet
```

- `firmware: 0.0.0`・`extended: 0x00000000`は、スレーブが
  `ESP_PRIV_FIRMWARE_VERSION`(0x17)と`ESP_PRIV_CAP_EXT`(0x16)のTLVを
  **送っていない**ということ。これらのタグより前の版である。
  upstreamの現行は**2.12.12**
- `capabilities: 0x0D` = `ESP_WLAN_SDIO_SUPPORT | ESP_BT_SDIO_SUPPORT |
  ESP_BLE_ONLY_SUPPORT`。**`ESP_CHECKSUM_ENABLED`(bit 7)が立っていない**ので、
  こちらはチェックサム欄を0のままにする（スレーブも検証しない）
- TLVのタグ番号は`common/transport/esp_hosted_transport_init.h`の
  `ESP_PRIV_TAG_TYPE`と一致していることを確認済み

`IF_STA`を0にして試すと通らない。`IF_SERIAL=3`・`IF_PRIV=5`が動いている以上、
列挙体は`ESP_INVALID_IF=0`を持つ新しい方であり、**`ESP_STA_IF=1`で正しい**。

### 送信失敗の原因: STA/AP MACの取り違え（修正・実機確認済み）

アソシエート済み・受信は正常・ARP応答とDHCP DISCOVER（304 byte、
`14+20+8+262`で最小構成として正しい長さ）は正しく生成されて`tx sent`も増える。
ただし以前のフレームは、送信元とDHCPの`chaddr`にSoftAP側のMACを使っていた。
無線のSTAが使うMACと一致しないため、ARP応答やDHCP応答が成立しない。

`netdump tx`も同じRPC結果を正解値として送信元を検査していたので、誤ったMACでも
`src=... (us)`と表示されていた。このため従来の診断結果だけでは取り違えを
検出できなかった。RPC引数とIPスタック、`wifimac`、`netdump`の各呼び出しを
`WIFI_IF_STA = 0`へ統一した。修正後の実機で`ipconfig dhcp`によるリース取得と、
P4からPC・PCからP4の双方向pingに成功したため、この取り違えが原因だったと確定した。

**修正前に実施した切り分け**:

- **リンクの非対称性**。APからのブロードキャスト／マルチキャストは最低基本
  レートで強力に飛ぶので受信できるが、こちらからの送信が届いていない可能性。
  `wifistatus`のRSSIを確認し、APのすぐ隣で再試験する
- **APの入れ替え**。スマホのテザリングに繋ぎ変えれば、AP起因かどうかが
  一度で分かる（クライアント間分離、VLAN、バンド分離をまとめて除外できる）
- **実施済みで空振り**: RSSI -50（机上）／-15（AP隣）でも駄目。
  スマホのテザリングに繋ぎ替えても駄目。**電波品質とAPは原因ではない**

**スレーブ側のソースは新旧2版で確認済み。** 現行（2.12.12相当のmain）と
2023年12月の`6122c0d5`のどちらも`process_rx_pkt`の論理は同一で、

```c
if (buf_handle->if_type == ESP_STA_IF && station_connected) {
        esp_wifi_internal_tx(ESP_IF_WIFI_STA, payload, payload_len);
}
```

`station_connected`は`WIFI_EVENT_STA_CONNECTED`のハンドラで
`esp_wifi_internal_reg_rxcb`の直後に立つ。**受信できている＝rxcbが登録済み＝
`station_connected`はtrue**なので、この門番は満たされている。
`recv_task`の`datapath`もRPCが通る以上立っている。
ホスト側の設定TLV（`HOST_CAPABILITIES`はスレーブがログに出すだけ、
スロットル閾値0は流量制御を無効にするだけ）も影響しない。

スレーブのSDIO受信層（`slave/main/sdio_slave_api.c`）まで読んだが、そこにも
落とす理由は無い:

- 受信キューは`PRIO_Q_SERIAL`／`PRIO_Q_BT`／`PRIO_Q_OTHERS`の3本に分かれ、
  ステーションデータは`OTHERS`へ入る。`sdio_read`は3本とも順に読むので、
  データ用キューが放置されることはない
- チェックサム検証は`#if CONFIG_ESP_SDIO_CHECKSUM`。スレーブが
  `ESP_CHECKSUM_ENABLED`を申告していない＝無効。RPCがチェックサム0で
  通っている事実とも一致する
- `start_rx_data_throttling_if_needed`は`throttle_high_threshold > 0`が
  条件。こちらは0を送っているので流量制御ごと無効であり、この経路は動かない
- ヘッダの`flags`は`FLAG_POWER_SAVE_STARTED = (1 << 2)`、
  `FLAG_POWER_SAVE_STOPPED = (1 << 3)`。データフレームでは0を送っており、
  スレーブに省電力状態だと誤認させることはない

つまり **SDIO書き込み → スレーブの受信タスク → 長さ検査 → キュー →
`recv_task` → `process_rx_pkt` → `esp_wifi_internal_tx`** の全段には、
別の脱落理由は見つからなかった。MAC修正後も失敗する場合は、この調査結果を
前提にC6側の観測へ進む。

### TFTPの実測は111 KiB/s ── 1往復あたり約4.47 ms

`big.bin`（512,123 byte、1,001ブロック）が4,474 msだった。

```text
in 4474 ms, 111 KiB/s (memory only; there is no filesystem)
```

計画で見込んだ「数十〜数百KiB/s」の範囲に収まっている。**この数字の意味は
速度そのものより1往復の値段**で、4,474 ms ÷ 1,001往復＝**約4.47 ms**である。
TFTPが遅いのはロックステップだからで、リンクの帯域ではない。同じ往復コストは
ロックステップで動く他のプロトコルにもそのまま効くので、将来この上に何かを
載せるときの見積りにはKiB/sではなくこの4.47 msを使うこと。

**同じファイルをTCPで取ると7.4倍速い**ので、111 KiB/sがリンクの上限では
ないことは測定で確定した。下の比較を参照。

### Stage 6: TCPは3ケースとも成功した

PC側は`python3 -m http.server 8080`。`/`（ヘッダ表示）、`/big.bin`
（512 KiBの本文受信）、`/nosuchfile`（404）のいずれも期待どおりだった。
`http.server`は既定でHTTP/1.0なので本文の終わりで接続を閉じ、`may_recv`が
落ちることを終端に使う実装の前提と噛み合っている。

### TFTPが遅いのはリンクではなく往復の待ち（測定で確定）

同じ`big.bin`をTFTPとHTTPで取った結果:

| | 転送量 | 時間 | 速度 |
| --- | --- | --- | --- |
| TFTP（512 byteロックステップ） | 512,123 byte | 4,474 ms | 111 KiB/s |
| HTTP／TCP | 512,327 byte | 605 ms | **827 KiB/s** |

**リンクは少なくとも827 KiB/s出る。** したがってTFTPの111 KiB/sは帯域では
なく、1往復あたり約4.47 msという待ち時間で決まっている。その4.47 msのうち
512 byteの転送そのものが占めるのはTCPの実効速度換算で**約0.6 ms**なので、
**残りの約9割は往復の待ち**である。TCPが速いのは複数セグメントを同時に
飛ばして、この待ちを隠しているからで、smoltcpのTCPが正しく動いていることの
傍証にもなっている。

**この測定は`max_burst_size`を申告しなかった判断の裏付けにもなっている。**
1 MSS（1,448 byte）ずつしか飛ばせなければ、往復4.47 msでは316 KiB/sが上限に
なる。実測の827 KiB/sはその2.6倍なので、平均して2〜3セグメントは同時に
飛んでいる。`Some(1)`と正直に申告していたら、smoltcpはこれをTCPウィンドウの
上限として使うので、**この2.6倍は丸ごと失われていた**。

ブロックサイズ拡張（RFC 2348のblksize）を入れれば1,428 byteブロックで
**312 KiB/s程度**までは行く計算になるが、待ちが往復ごとに残る以上TCPには
届かない。パイプライン化はTFTPの仕様上できないので、速度が要るなら
プロトコルを変えるのが正しい。blksizeは今回の非目標のままとする。

**受信量が独立した検算になっている**: HTTPの512,327 byteは
ファイル512,123 byteとの差が204 byteで、`python3 -m http.server`が返す
ヘッダの長さとして妥当である。`httpget`はCRCを取らないが、1 byteも
落としていないことはこの一致から言える。

### Stage 5: TFTPは3ファイルともCRCが一致した

PC側はdnsmasqをTFTPだけで起動し（`--port=0 --enable-tftp --tftp-root=...`）、
次の4ケースを試した。**すべて期待どおり**である。

| ファイル | 長さ | 何を確かめたか |
| --- | --- | --- |
| `tiny.txt` | 10 byte | 1往復で終わる最短経路と、サーバのエフェメラルポートへの追従 |
| `boundary.bin` | 51,200 byte | 512の倍数。**長さ0のDATAで終端する**経路 |
| `big.bin` | 512,123 byte | 1,000往復。サイズ・CRC-32・進捗表示 |
| （存在しない名前） | ― | ERRORパケットの解釈 |

512の倍数のファイルが空のDATAで終わる件は事前に潰してあった罠で、
`parse`が`datagram[4..]`を空スライスとして返し、`data.len() < BLOCK_BYTES`が
そのまま終端判定になる。実機でもそのとおりに終わった。

### TFTPのERRORメッセージはサーバ固有の文字列

存在しないファイルを要求したときの表示:

```text
server error 1: file /tmp/claude-1000/-home-nerry-proj-tab5test/29bbb86b-f05d-40
```

RFC 1350が定めているのは**エラーコードだけ**で（1 = File not found）、続く
文字列はサーバが自由に決める。dnsmasqはTFTPルートからの絶対パスをそのまま
入れるため、`Line`の80 byteに収まらず末尾が切れている。切れているのは表示
だけで、判定に使うコード1は正しい。`Line::push_ascii`は溢れた分を黙って
捨てるので、**長いサーバメッセージは常にこう見える**。切り捨てを示す記号は
出ないので、末尾が中途半端に見えても異常ではない。

## C6へのアクセス（再開時の手順）

Tab5の基板にはC6専用のダウンロード用ヘッダ**J1（`C6_ISP`、6ピン）**がある。
回路図1ページ目、`ESP32-C6-MINI-1U`(U2)の右隣。

| J1ピン | ネット | C6モジュール側 |
| --- | --- | --- |
| 1 | `WLAN_3.3V` | 電源3.3 V |
| 2 | `RF_C6_TXD` | pin 31 `TXD0`（22Ω R14経由） |
| 3 | `RF_C6_RXD` | pin 30 `RXD0`（22Ω R15経由） |
| 4 | `RF_C6_RST` | pin 8 `EN`（P4のGPIO15とR4 1kΩで共有） |
| 5 | `RF_C6_IO9` | pin 23 `IO9`（ブートストラップ、R91 10kΩプルアップ） |
| 6 | `GND` | GND |

ピン1と6は回路図PDFの座標からの推定なので、**配線前に目視で確認すること**。
2〜5は明確。

**ログを読むだけなら2本**（3.3V系のUSB-TTL変換器、115200 baud）:
J1 pin 2 →変換器のRX、J1 pin 6 →GND。**pin 1は繋がない**（Tab5が自分で
給電しているところへ外から電圧をかけることになる）。

ただし`process_rx_pkt`は送信失敗を`pkt_stats.sta_rx_out_fail`に数えるだけで
**既定のログレベルでは何も出さない**（`ESP_HEXLOGV("STA_Put", ...)`もverbose）。
ログで見えるのはスレーブの素性とWi-Fiドライバ側のエラーまで。

**同じ症状が再発した場合の手段**:

1. `wifimac`と`netdump tx`でSTA MACが送信元になっていることを確認する
2. それでも失敗する場合は**C6のUARTを読む**。低リスクだが上記のとおり
   決定打にならない可能性がある
3. **C6のファームウェアを新しくする**。同じJ1から esp-hosted-mcu の slave を
   書き込む（6ピン全部使用、pin 5をGNDに落としながらリセットでダウンロード
   モード）。出荷時の版は検証に使ったソースよりはるかに古く、自前ビルドなら
   ログレベルも上げられるので観測と更新を同時に行える。**ただしこれは診断では
   なく仮説に基づく賭け**で、古い版のTXパスにも欠陥は見つけられていない

参照した文書: [Tab5回路図](https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1132/Tab5_Schematics_PDF.pdf)、
[C6ファームウェアの復元手順](https://docs.m5stack.com/en/guide/restore_factory/m5tab5_c6_wifi)
