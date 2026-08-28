# TCP/IP（smoltcp）

> 索引: [`../DESIGN.md`](../DESIGN.md) ／ 段階分けと実機での判断記録:
> [`TCPIP_PLAN.md`](TCPIP_PLAN.md)

[`WIFI.md`](WIFI.md)のリンク層（ESP32-C6のESP-Hosted）の上に、IPv4を載せた
層です。ARP・IPv4・ICMP・UDP・DHCP・TCPは**自前実装せず
[smoltcp](https://docs.rs/smoltcp) 0.14を使います**。このリポジトリが
ハードウェアを触る層を自前で書くのは「ベンダのコードの中で何が起きているか
分からなくなるのを避ける」ためで、RFCで完全に規定され、パケットキャプチャで
外から観測できるプロトコル層にはその理由が当てはまりません。

到達点はDHCPでアドレスを取得し、名前解決・ping・TFTP読み出し・最小の
HTTP GETができるところまでです。受け取ったファイルは`/tmp`へ保存できます
（[`FILESYSTEM.md`](FILESYSTEM.md)）。**IPv6は入れていません**（smoltcpの
`proto-ipv6`を有効にすれば足せますが、現状は無効）。名前解決はAレコードだけで、
キャッシュ・サーチドメイン・逆引きはありません。サーバ機能とTLSもありません。

## 層構造

| モジュール | 役割 |
| --- | --- |
| `src/tick.rs` | SYSTIMERによる1 kHzの単調なミリ秒時刻源。ネットワーク専用ではなく汎用（`uptime`が最初の利用者） |
| `src/wifi/rpc.rs` | `IF_STA`フレームの受信キューと送信（下層は[`WIFI.md`](WIFI.md)） |
| `src/net/device.rs` | smoltcpの`phy::Device`実装。受信キューとトークンの橋渡し |
| `src/net/stack.rs` | `Interface`・`SocketSet`・DHCPクライアント・DNSソケット・ポンプ |
| `src/net/dns.rs` | 名前解決（Aレコード）。ソケットは`stack.rs`側 |
| `src/net/ping.rs` | ICMP echoの送信と往復時間の測定 |
| `src/net/tftp.rs` | TFTP読み出しクライアント（RFC 1350） |
| `src/net/http.rs` | HTTP/1.0 GET。中断可能な`Transaction`と、その上の同期`get` |
| `src/net/tls.rs` | TLS 1.3クライアント。TCP socketを所有し、平文を出すtransport |
| `spki/src/lib.rs` | leaf証明書のDERから`SubjectPublicKeyInfo`を取り出す（`tab5-spki`、host test付き） |
| `src/entropy.rs` | SAR ADCノイズ源から種を取るCSPRNG。TLSの秘密値はここからだけ来る |

`net::Stack`は**C6のリンクを所有しません**。`wifi::Rpc`がトランスポートを
持ったままで、パケットを触る呼び出しはその都度`&mut Rpc`を借ります。
`src/app/wifi_manager.rs`の接続管理器が両方の`Option`と接続元、IP設定方針、接続状態を
1つの所有者として保持します。リンク喪失とSTA切断時はstackも同時に捨てるため、存在しない
リンクで取得したアドレスが残りません。Wi-Fi OFFではstack、アドレス、route、resolverを
まとめて破棄し、ネットワークコマンドから下層sessionを暗黙に作りません。`wifiinfo`・`wifiup`も
セッションを張り直すので管理器を通してstackまで破棄し、診断後に元のON/OFF状態へ戻します。

メニューから始めたassociationでは、接続管理器がreason／timeout／RPC結果を分類して最大30秒の
backoffで再試行し、成功後にDHCPを自動開始します。接続後のSTA切断でも古いstackを破棄して
同じRAM内資格情報で再associationし、新しいstackでDHCPを取り直します。C6リンク喪失では
`Rpc`から再構築します。DHCP leaseだけを失った場合はassociationを維持し、既存DHCP clientの
再取得を続けます。ブラウザの進行中socketは旧stackとともに無効になり、画面は操作可能なまま
新しい接続を待ちます。

C6 NVSに保存profileがある起動も同じ`MenuManaged`／`Dhcp`方針へ入り、画面操作なしで
associationとDHCPを開始します。メニューの`Connect once`とCLIはC6 RAM設定を使うため、
保存済みprofileを上書きしません。

この処理はCLIの契約を変えません。`wificonnect`は1回のassociationだけで資格情報を管理器へ
保持せず、IPv4設定は利用者が`ipconfig dhcp`またはstatic設定を実行するまで
`Unconfigured`のままです。したがってCLI接続は自動再接続しません。

## 時刻源（`tick.rs`）

smoltcpの`Instant`は**単調であることが前提**で、巻き戻ると再送タイマと
リースタイマが壊れます。`delay.rs`の`cycle_count`は`rdcycle`の下位32 bitだけを
読んでおり360 MHzでは約11.9秒で一周するので、基準には使えません。

- SYSTIMER（`0x500E_2000`）のunit 0を自走させ、comparator 0を周期モードで
  16,000ティック＝1 kHzに設定します。クロックはXTAL 40 MHzを**固定分周2.5**で
  16 MHz（`SOC_SYSTIMER_FIXED_DIVIDER`）
- 周期は`SYSTIMER_TARGET0_CONF_REG`へ書いたあと`SYSTIMER_COMP0_LOAD_REG`へ
  書かないと反映されません
- バスクロックとクロック源は`HP_SYS_CLKRST`側（`SOC_CLK_CTRL2`bit 23、
  `PERI_CLK_CTRL21`bit 29/30）にあります。`psram.rs`・`sdmmc.rs`と同じ
  分かれ方で、ここを忘れるとレジスタは書けるのに動きません
- 割り込みソースは`ETS_SYSTIMER_TARGET0_INTR_SOURCE`＝**53**、CPU外部線3
  （CLIC 20）。レベル割り込みなのでISRで`SYSTIMER_INT_CLR_REG`を書かないと
  張り付きます
- `clicintctl`の上位3 bitが**レベル**、残り5 bitがレベル内の優先度です
  （`cliccfg.nlbits`＝3）。しきい値が`0x1F`＝レベル0なので、**動かしたい割り込みは
  全部レベル1に置く**必要があります。表示`0x3F`、USB`0x20`、ティック`0x20`。
  これより小さい値にするとレベル0に落ちてしきい値に潰され、割り込みが
  一度も来ません。レベル内では互いにプリエンプトしないので、ティックのISRが
  「カウンタを進めて`INT_CLR`を書く」だけであることのほうが順序より重要です
- ミリ秒は64 bitですが、RV32には64 bitのアトミックがないので`AtomicU32`
  2本に分けます。書き手はISRだけで、**上位を先に繰り上げてから下位を
  書きます**。読み手は上位→下位→上位の順に読み、上位が変わっていたら
  読み直します（逆順だと49.7日ぶん過去の時刻を返す窓ができます）

`uptime`はこのティックの秒数と、フレーム数からの概算の両方を出します。
2つが離れていくのは、ティックを取りこぼしているかフレームを落としているか
どちらかなので、そのまま検査になります。

## 受信キューと背圧

C6は受信したフレームをホストが読むまで保持し、**溜まった総量が
トランスポートのステージングバッファ（8,704 byte）を超えるとリンクは
復帰できません**。そのため`app.rs`のフレームループは毎フレーム
`Stack::poll`を呼び、コマンドの実行中はコマンド自身がポンプループを
回します。

- `Rpc`は受信した`IF_STA`フレームを最大32フレームのキューへ積みます
- `Rpc::service`はキューが満杯になったら**トランスポートを読むのをやめます**。
  読んでいないフレームはステージングバッファに残るので失われません
  （一度読んだフレームは戻せないため、これが唯一の非破壊な止め方です）
- RPC応答を待つ経路（`wifiscan`・`wificonnect`など）は止まれません。誰も
  キューを捌いていない状況で止まると応答を永久に待つことになるので、
  こちらは**最も古いフレームを捨てます**。捨てた数は`ipconfig`の
  `dropped`に出ます
- `Stack::poll`は「キューを埋める→インタフェースに捌かせる」を最大4回
  繰り返します
- **スタックがまだ無い状態**（CLIの`wificonnect`は済んだが`ipconfig`を実行して
  いない、という一番長く居る状態）では、フレームループが
  `Rpc::discard_station_frames`で読んでは捨てます。行き先が無くても、
  読まないことが最終的にリンクを殺すためです。捨てた数は同じ`dropped`に
  入ります（`dropped`は「IPスタックに届かなかったフレーム数」）

送信側は、スレーブがスロットル（`INTERRUPT_START_THROTTLE`）を要求している
間はフレームを捨てます。上位はすべて再送するプロトコルなので、捨てるほうが
データ経路を詰まらせるより安全です。

**送信バッファは受信のステージングバッファと別です。** smoltcpは受信トークンと
同時に渡した送信トークンで応答を組み立てる（ARP応答がまさにこれ）ので、
未解析の受信フレームが残っているまま送信が走ります。バッファを共有すると
その残りを上書きし、しかも読み手にはそれが起きたことが分かりません。

`ipconfig`はこの両方向のカウンタを出します。

```text
rx queued 0, delivered 12, dropped 0
tx sent 4, throttled 0, failed 0
slave throttling: no, leases lost 0
```

**送信側は外から見えない唯一の半分です。** C6が送らなかったフレームは、
相手から見ると「そもそも生成されなかったフレーム」と区別がつきません。
`sent`が増えているのに相手に届いていなければ原因はこのボードの外、
`sent`が0のままなら中です。`throttled`が増えるならスレーブがスロットルを
要求したまま、`failed`が増えるならトランスポートが送信を拒否しています。

**アソシエートが切れても`sent`は増え続けます。** ESP-Hostedのスレーブは
未接続の間ステーションのデータフレームを黙って捨てます
（`process_rx_pkt`の`station_connected`ゲート）。一方こちら側では、smoltcpは
フレームを作り続け、SDIOへの書き込みも成功し続けます。**切断とネットワークの
無反応は、送信カウンタまで含めて同じ症状になります。** そのため`ipconfig`は
最初にアソシエート状態を表示し、ネットワークコマンドの後には切断イベントを
`the station was disconnected ...`として報告します。

ただし`sent`は「SDIOへの書き込みが完了した」ところまでで、C6が電波に出したかは
分かりません。**何を送ったのか**は`netdump tx`が直近8フレームのヘッダを出します。
送信元MACが自局のものでない場合は`(NOT US)`と表示します。SDIOのリンクは
そのようなフレームも受け取りますが、Wi-Fiドライバかアクセスポイントが捨てるので、
症状は「無反応」と区別がつきません。

## `IF_STA`のペイロード

**802.3イーサネットフレーム**です。ESP-Hostedのホスト側TXは`esp_netif`の
transmitコールバックで、Wi-Fiステーション用netifが渡すのは14 byteヘッダ付きの
イーサネットフレームであり、802.11との変換はC6側のWi-Fiドライバが行います。
したがってmediumは`Medium::Ethernet`です。`DeviceCapabilities`へ申告するのは
**イーサネットヘッダを含む1,514 byte**である点に注意してください。smoltcpの
`max_transmission_unit`は`Medium::Ethernet`では frame 全体の上限で、smoltcp側が
14 byteを引いてIP MTUにします。ここに見慣れた1500を書くと、IP MTUが黙って
1486になります。ESP-Hostedのペイロード上限は1,524 byteなので、1,514 byteの
フレームはそのまま載ります。

`netdump`コマンドが宛先MAC・送信元MAC・ethertypeをそのまま表示するので、
この前提は実機で確認できます。

**アドレスを付ける前は自局宛のフレームは来ません。** IPを持っていない
ステーションを名指しする理由がネットワーク側に無いためで、見えるのは
ブロードキャストとマルチキャスト（mDNSの`01:00:5E:00:00:FB`、IPv6の
`33:33:...`など）だけです。それでも宛先・送信元・ethertypeの並びは確認できるので、
802.3であることの検査としては十分です。自局宛のユニキャストが届くことは、
アドレスを付けてPCからpingしたとき（`(us)`と表示される）に確かめます。

インタフェースのハードウェアアドレスは、RPCの`GetMacAddress`で取得した
**C6自身のSTA MAC**です。無線が使っていないアドレスでARPに応答しても
返事は返ってきません。このRPCの要求フィールド名は`mode`ですが、値の意味は
`wifi_mode_t`ではなく`wifi_interface_t`です。STAを取得するときは
`WIFI_IF_STA = 0`を渡します。`WIFI_MODE_STA = 1`を渡すとSoftAP側のMACが
返るため、そのアドレスをIPインタフェースへ設定してはいけません。

## シェルコマンド

| コマンド | 内容 |
| --- | --- |
| `netdump [count]` | 自局のSTA MACと、C6が押し込んでくるフレームのイーサネットヘッダ（宛先・送信元・ethertype）を表示。宛先は自局宛／ブロードキャスト／マルチキャストに分類する |
| `netdump tx` | 直近にC6へ渡した送信フレームのヘッダを表示。送信元MACが自局のものかも検査する |
| `ipconfig` | アソシエート状態、現在のIPv4設定、station traffic の送受信カウンタを表示 |
| `ipconfig dhcp` | DHCPクライアントを起動してリースを待つ（最大15秒） |
| `ipconfig <a.b.c.d[/len]> [gateway]` | アドレスを手で設定（長さ省略時は`/24`）。リゾルバは空になる |
| `ipconfig dns [<a.b.c.d> ...]` | リゾルバだけを差し替える。アドレスは触らない。引数なしで削除 |
| `ipconfig release` | アドレス・既定経路・リゾルバを外し、**DHCPクライアントも止める** |
| `nslookup <name>` | 名前をAレコードで引く |
| `ping <host\|a.b.c.d> [count]` | ICMP echoの送信と往復時間。既定4回 |
| `tftpget <host\|a.b.c.d> <file>` | TFTPで読み出してカレントディレクトリへ保存し、サイズとCRC-32と速度を表示 |
| `httpget <host\|a.b.c.d>[:port] [path]` | HTTP/1.0 GET。ヘッダの先頭数行を表示し、本文をカレントディレクトリへ保存 |
| `hs <url> [r <n>\|p [n]\|c <n>]` | `Transaction`を直接回して結果を数値で報告。schemeを省くと`http://`を補う（[`BROWSER.md`](BROWSER.md)） |

Wi-FiがONなら、いずれも必要に応じてC6のリンクとstation modeを用意します（`wifiscan`以降と
同じ`wifi_session`を通ります）。OFF中はC6を起動せず`wifi on`を案内します。APへのアソシエートは別で、`wificonnect`が
済んでいないと`ipconfig dhcp`はリースを取れません。

全画面の`wifi`メニュー、または起動時の保存profileから接続した場合は、管理器がassociationイベントをフレームごとに読み、
成功後にSTA MACで`net::Stack`を新しく作ってDHCPを自動開始します。15秒でleaseを取得できない
場合もDHCP clientを維持し、画面を閉じた後を含め毎フレームのpollで取得を継続します。
stackがなければ`Rpc::discard_station_frames`を呼び、C6の受信queueを溜めません。

CLIの`wificonnect`はこの自動DHCPを使いません。従来どおりassociationだけで戻り、利用者が
`ipconfig dhcp`を実行した時点で初めてDHCPを開始します。管理器のIP方針もこの時点で
`Dhcp`に変わり、static設定では`Static`、`ipconfig release`では`Unconfigured`になります。

**こちら宛のICMP echoには、アドレスが設定されていればいつでも応答します。**
smoltcpの`auto-icmp-echo-reply`によるもので、`ping`コマンドの実行中に
限りません。

### DHCPクライアントの寿命

**インタフェースからアドレスを外しても、DHCPソケットには何も伝わりません。**
一度リースを取ったソケットは`Renewing`状態で「自分のリースはまだ有効だ」と
信じ続けます。ここを取り違えると次の2つが起きます。

- `ipconfig dhcp`が**DISCOVERを1つも出しません**。再利用したソケットは自分の
  更新タイマ（数分〜数時間先）を待つだけなので、コマンドは15秒待って
  `no DHCP answer; is the station associated?`と表示します。**原因はこちら側
  なのに、案内はAPを疑わせます**
- `release`したはずのアドレスが、更新タイマが来たときに**黙って復活します**

そのため`start_dhcp`は取得済み・新規を問わず`dhcpv4::Socket::reset()`を
呼んでDiscoveringからやり直し、`ipconfig release`はソケットごと捨てます。

**リセットを`clear_addresses`の中に置いてはいけません。** リースを失った
ときの経路（`Event::Deconfigured`）も`clear_addresses`を通りますが、
そこはクライアントを走らせたまま再取得させたい場面です。ここでソケットを
捨てたりリセットしたりすると、回復しようとしている当人を壊します。

## smoltcpの設定

```toml
smoltcp = { version = "0.14", default-features = false, features = [
    "alloc", "medium-ethernet", "proto-ipv4", "proto-dhcpv4",
    "socket-dhcpv4", "socket-udp", "socket-icmp", "socket-tcp",
    "socket-dns", "dns-max-server-count-3", "dns-max-result-count-4",
    "auto-icmp-echo-reply",
] }
```

- `default-features = false`は必須です。既定には`std`・`phy-raw_socket`・
  `phy-tuntap_interface`などホスト向けの機能が入っています
- `alloc`は有効。ソケットバッファをPSRAMヒープから取ります
- `log`・`defmt`は無効のまま。ログは`uart.rs`の接頭辞方式（`NET:`）です
- `DeviceCapabilities::max_burst_size`は**設定しません**。smoltcpはこれを
  「TCPのウィンドウを何MSSに制限するか」として使うので、SDIOの
  1フレームずつという性質をここに書くと全接続が1 MSSに制限されます
- `dns-max-*`は**コードの有無ではなく数値の設定**で、smoltcpの`build.rs`が
  読みます。どちらも**既定は1**なので、有効にしないとリゾルバは1台しか
  持てません（DHCPが3台配っても`update_servers`が黙って先頭だけ残します）。
  サーバ3台はDHCPの`MAX_DNS_SERVER_COUNT`に合わせた値、結果4件は
  ラウンドロビンの名前を`nslookup`で全部見るためです
- `socket-mdns`は**有効にしていません**。有効にするとsmoltcpは`.local`で
  終わる名前の宛先をユニキャストのリゾルバから224.0.0.251へ差し替えるので、
  `join_multicast_group`を入れる前に有効にすると`.local`が必ず失敗します

コードサイズはsmoltcp導入前後でIROMが260,142→334,900 byte（約+73 KiB）、
DROMは変化なしです。DNSの追加ではIROMが344,222→354,526 byte（約+10.1 KiB、
うちfeatureを有効にしただけで+3,758 byte）、DROMは130,776 byteのまま変化なし。
FLASH XIP領域（`ROM_TEXT`は約3.8 MiB）に対して十分小さく、
ソケットバッファもPSRAMヒープ（約30 MiB）なのでどちらも制約になりません。

## DNS

Aレコードだけを引きます。HTTPと同じく**中断可能な`Query`（`start`／`poll`／
`cancel`）が本体で、同期の`resolve`はその上のループ**です。名前が死んだと
判定されるまでリゾルバ1台で5秒、複数で13秒かかり、その間フレームループが
回らない画面はEscapeの効かない画面になります——そしてEscapeを押したくなるのは
まさにその状況です。`Query`はDNS socketのslotを持つので、全経路で`cancel`を
呼びます。

プロトコルはsmoltcpの`socket-dns`で、こちらが
書いているのは**ソケットをスタックに常設する部分・サーバ一覧を流し込む
部分・「名前かアドレスか」を判定する部分**の3つだけです。

**DNSソケットだけは`Stack`が持ち続けます。** 他のクライアント（TFTP・HTTP・
ping）のソケットはコマンドの実行中しか存在しませんが、`Interface::poll`は
ソケットセットに居るソケットしか再送・受信処理をしないので、問い合わせの
たびにソケットを作ると誰もポンプしていない間の応答を落とします。加えて
リゾルバの設定はリースと同じ寿命のもので、コマンド1回の寿命ではありません。

- **リゾルバは`Ipv4Config`が持つのが正**です。smoltcpのDNSソケットは一覧を
  読み返すAPIを持たないので、`Stack::install_dns_servers`が設定から
  ソケットへ一方向に写します
- **アドレスを外すとリゾルバも外れます**（`clear_addresses`）。もう成立して
  いない設定で学んだ値であり、残すと「消えたリンクで取ったアドレスを持ち
  続ける」のと同じ嘘になります。しかも古いリゾルバへの問い合わせは
  タイムアウトで失敗するので、症状が何も語りません
- 一覧は`DNS_MAX_SERVER_COUNT`（3）で切り詰めます。切り詰めは
  `apply_config`と`set_dns_servers`で行い、**表示される一覧が実際に問い合わせ
  先になる一覧と一致する**ようにします
- 静的アドレス（`ipconfig <a.b.c.d>`）はリゾルバを空にします。手で付けた
  アドレスにはリゾルバを学ぶ経路が無いためで、必要なら`ipconfig dns`で
  続けて指定します

**サーバ一覧が空のときは問い合わせを始めません。** smoltcpは空の一覧を
「全サーバを試し終えた」と解釈して最初のdispatchで失敗させるので、
**1パケットも出ないまま「その名前は存在しない」と同じ結果になります**。
`net::dns::resolve`は開始前に一覧を見て、`NoServers`という別の失敗にします。

**数字のアドレスは問い合わせません。** `ping`・`tftpget`・`httpget`は宛先を
まず`parse_ipv4`で読み、成功したらそのまま使います。リゾルバが無い状態でも
`ping 192.168.1.1`が動くことは、切り分けの手段として残す価値があります。
逆に`nslookup`は**アドレスに見える引数でも必ず問い合わせます**。リゾルバ
そのものを試す場所が他に無いためです。

タイムアウトは**リゾルバ1台なら5秒、2台以上なら13秒**です。勘で選んだ値では
なく、smoltcpの`RETRANSMIT_TIMEOUT`＝**10秒**という1つの区切りから導いて
います。この10秒には性質の違う2つの意味がぶら下がっていて、しかも
**互いに逆向きに効きます**。

- **乗り換えの時刻**。次のサーバへ移るのは今のサーバで10秒経ってから
  なので、**10秒より短い予算では2台目が一度も試されません**
- **諦めの時刻**。最後のサーバで10秒経つと、smoltcpは問い合わせ全体を
  `State::Failure`にします。これは**存在しない名前（NXDOMAIN）と同じ状態**で、
  `get_query_result`はどちらも`Failed`を返します。つまり
  `10秒 × 台数`に届く予算を置くと、**リゾルバが全滅しているネットワークを
  「その名前は存在しない」と報告します**

そこで予算は必ず**この2点の間**に置きます。2台以上のときの13秒は
「10秒＋乗り換え後の応答待ち3秒」で、**2台目に丸10秒を与える必要はありません**。
10秒はsmoltcpが待つのをやめる区切りであって応答に要る時間ではなく、
生きているリゾルバはミリ秒で答えます。1台のときは乗り換え先が無いので、
「死んだと判断するまで」だけの5秒です。

この関係は`const _: () = assert!(...)`でコンパイル時に検査しています。
このファームウェアはreleaseでしかビルドしない（debugはRAMに収まらない）ので、
`debug_assert!`では検査になりません。

予算内で終わらなければ**こちらが`cancel_query`します**。

長く待つのは**リゾルバが実際に死んでいるときだけ**です。答えがあれば
ミリ秒、存在しない名前はNXDOMAINで即座に返ります。

- **`get_query_result`は結果を渡すときにスロットを解放し、空きスロットへの
  再呼び出しは`panic!`します**（`cancel_query`も同じ）。`pump_until`の述語は
  何度も呼ばれるので、述語の中で結果を取り出したらその場で捕まえて`true`を
  返し、**取り出せた経路では二度と触りません**。`cancel_query`を呼ぶのは
  何も取り出せなかった経路だけです
- **応答は送信元アドレスが一覧に載っているものしか受け付けません**
  （`Socket::accepts`）。問い合わせ先と違うアドレスから返すルータでは、
  症状は**完全な無音のタイムアウト**になります。疑うときは`netdump`で
  応答フレームが届いているかを見てください
- **UDPソケットはDNSソケットより先に照合されます**（`iface`の受信経路）。
  TFTPのエフェメラルポートがDNSの送信元ポートと偶然一致すると応答を
  横取りされます。現状は解決が転送の前に終わるので重なりません
- 名前は`Line`の80桁を超えると黙って切れます。判定にはアドレスの行を
  見てください

`httpget`に名前を渡したときは、**`Host:`ヘッダにも名前が載ります**。
解決後のアドレスを載せると、1つのアドレスを複数のサイトで共有している
サーバがどのサイトか判断できません。

## TFTP

RFC 1350のみで、オプション拡張（RFC 2347/2348）は入れていません。512 byte
ブロックのロックステップなので、往復遅延がそのまま速度になります。

実測は512,123 byteが4,474 msで**約111 KiB/s**、1,001往復なので**1往復あたり
約4.47 ms**です。同じファイルをHTTP／TCPで取ると827 KiB/sなので、**遅さの
理由はリンクの帯域ではなくロックステップの待ち**だと測定で確定しています
（往復の4.47 msのうち、データそのものは約0.6 ms）。見積りにはKiB/sではなく
この往復コストを使ってください。

- **サーバは最初の応答を別のエフェメラルポートから返します。** RRQの宛先は
  69番ですが、以降のやり取りは最初のDATAの送信元ポートに対して行います
- 最終ブロックは512 byte未満のDATAです。長さがちょうど512の倍数のファイルは
  **長さ0のDATAで終端**します（エラーではなく、応答が必要です）
- タイムアウトすると直前に送ったパケット（RRQまたはACK）を送り直します。
  ACKの再送だけで、失われたACKからの回復は済みます
- ERRORパケットは**コードだけがRFCの定めるもの**で（1 = File not found）、
  続く文字列はサーバが決めます。`server error <code>: <message>`と表示しますが、
  1行80桁に収まらない分は黙って切れます（dnsmasqは絶対パスを入れるので必ず
  切れます）。判定にはコードを見てください
### 受け取ったバイトはsinkへ渡す

`tftp::get`はファイルを溜めません。ブロックが届くたびに
`&mut dyn FnMut(&[u8]) -> bool`へ渡し、返すのは転送したbyte数だけです。

以前は`Vec<u8>`に全部溜めて返していました。そのため**最大の転送サイズが
ヒープの大きさに縛られ**、8 MiBという上限（TFTPではなくこのファームウェアの
メモリについての数字）が必要でした。ボリュームへ保存したい呼び出し側は、
いったん全部メモリに載せる必要もありました。sinkにすると、呼び出し側が
CRCを取る・数える・書く・捨てるのどれを選んでも転送の大きさは変わりません。
**上限は無くなりました。**

- sinkが`false`を返すと転送を打ち切り`SinkRefused`になります。何が起きたかを
  報告するのはsink側の仕事です。バイトを何に使っていたかを知っているのは
  そちらだけなので
- sinkが呼ばれるのは**未見のブロックにつき1回だけ**です。ACKが失われて
  サーバが同じブロックを送り直した場合、ACKはもう一度返しますがsinkへは
  渡しません（同じバイトを二度書くことになるため）
- CRC-32は`Crc32`（`new`／`update`／`finish`）で流しながら計算します。
  ファイル全体が同時に手元にあることはもう無いためです

### 保存

`tftpget`は**カレントディレクトリへ**、リモート名の最後の要素で保存します。
サーバ側の`pub/images/thing.bin`のような名前をパスとして扱うと、こちら側に
無いディレクトリが必要になるか、`..`が保存先に紛れ込むためです。

`Vfs::write_stream`（[`FILESYSTEM.md`](FILESYSTEM.md)）1回で転送全体を包み、
届いたブロックをそのままボリュームへ流します。TFTPはロックステップなので、
書き込みが遅ければ転送が待つだけです。**サイズの上限はメモリではなく
ボリュームの空き**になりました。

書き込むのは`<名前>.part`で、完了してから本来の名前へ改名します。したがって
**本来の名前で現れたファイルは完全**です。途中で電源が落ちても、切れたファイルが
本物の名前を着ているのではなく`.part`が残ります。失敗した転送は`.part`を
削除します（削除できなければ、残した場所を1行出します）。

同じ名前のファイルが既にあれば置き換えます。失敗した転送をやり直すのが
一番ありがちな使い方だからです。`Vfs::rename`は名前が埋まっていると断るので、
改名の前に削除します。

書き込みの失敗は**全byteが届いていても転送の失敗**として扱います。末尾の欠けた
ファイルが保存されているのは、保存されていないより悪いためです。

**書き込めるのは`/tmp`だけ**なので、`cd /tmp`してから実行します。カレント
ディレクトリが書けない場合は、ネットワークに触る前に断ります。

## HTTP

HTTP/1.1ではなく**1.0**を使うのは、本文の終わりでサーバに接続を閉じさせ、
それを終端の判定に使うためです。要求には`Accept-Encoding: identity`を
明記します。ヘッダを省略したときの既定は「何でも」で、gzipで返されると
展開する手段がないためです。

`User-Agent`も送ります（`tab5-browser/<version>`）。**無いと相当数のサイトが
403で断ります** — 2026-08-28の実測で、`en.wikipedia.org`と`stackoverflow.com`は
このヘッダを外した要求に403を返し、付けると200になりました（`Accept`を足しても
変わりません）。

名乗る内容は実物どおりにします。ブラウザを騙るのは不誠実なうえに**成績も悪い**
のが実測で分かりました——`reddit.com`は素の`Mozilla/5.0`に403を返し、
`tab5-browser/0.2.0`には200を返します。fixture serverの`/require-user-agent`が
このヘッダを送り続けることを固定しています。

### ソケットには触らない

`net::http`はソケットを持ちません。バイトは`net::transport::Transport`から
来て、それが平文TCPかTLSセッションかは上の層が`Security`で1回だけ決めます。
status行、ヘッダブロック、chunkデコーダ、本文の終端判定は**どちらでも同じ
コード**です。

```text
                   +-- Transport::Plain -- TCP socket
HTTP Transaction --+
                   +-- Transport::Tls ---- net::tls::Transaction -- TCP socket
```

分けなかった理由は単純で、「本文はどこで終わるか」の答えが2つあると、
どちらが正しいか誰にも分からなくなるからです。半分のページを1ページとして
表示してしまう失敗は、この層全体がそれを避けるために組んであります。

`Transport`が答える質問は4つだけです——使える状態か（`poll`）、このバイト列を
受け取れ（`write`）、バイトをよこせ（`read`）、終わったか（`at_end`）。
平文側は`recv_slice`と`may_recv`がそのまま答え、TLS側は自前のfutureと
record bufferとhandshakeの裏でそれを作ります。

ただし**認証状態だけは平坦化しません**。`Transport::authentication`は平文で
`None`、TLSで`Some`を返します。`None`は「安全でない」ではなく「この質問は
当てはまらない」なので、表示側はschemeから推測せずこの値を見る必要があります。

### 2つの顔、実装は1つ

- **`Transaction`**が本体です。ソケットを`SocketSet`へ登録したまま、1回の
  `poll`ごとに決まった量だけ進めて戻ります。`Stack`と`Rpc`は所有せず、
  pollの間だけ借ります。ブラウザ画面がこれを使います。取得が終わるまで
  戻らない実装では、Escapeの効かない画面になるからです——そして「相手が
  ちゃんと応答を終える」ことは相手が約束してくれることではありません
- **`get`**は`Transaction`を回すループです。`httpget`が使います。コンソールを
  占有するコマンドは待ってよいので、こちらは取得完了まで戻りません

ヘッダ解析・本文の終端判定・接続の後始末は1箇所にしかありません。
`Transaction`は`close`を呼ばないとソケットが`SocketSet`から抜けないので、
`Drop`で「closeせずに捨てられた」ことをUARTへ出します。ここから`Stack`へは
手が届かないので、言うことが治療のすべてです。

`poll`は`Connecting`／`HeadReady`／`Body`／`Idle`／`Complete`／`Failed`を
返します。**`HeadReady`は本文と同じpollでは返しません。** headと同じ
読み出しに本文の先頭が入っていても、その分は次のpollまで保持します。
redirectや表示しないstatusを、本文を1 byteも読まずに判断できる状態を
保証するためです。

### 応答から読むもの

status code、`Content-Type`と`charset`、`Content-Length`、
`Transfer-Encoding: chunked`、`Location`、`Content-Encoding`。

`charset`はここでは小文字化して持つだけです。使うのはブラウザ側で、
本文の1 byte目より前に`Parser::declare_charset`へ渡します
（[BROWSER.md](BROWSER.md)の「文字符号化」）。

`Head::is_html`は`text/html`と`application/xhtml+xml`、および
`Content-Type`が無い応答に真を返します。`Head::is_text`はそれ以外の
`text/*`で、ブラウザはこちらをplain textとして表示します
（[BROWSER.md](BROWSER.md)の「HTML以外のもの」）。

本文の終端は3種類あり、どれかによって「接続が閉じた」の意味が変わります。

- `Content-Length`: その byte 数で終わり。超過分は捨てる
- `chunked`: chunk size・拡張・terminal chunk・trailerを増分解析する。
  chunk境界は読み出しをまたぐので、byte単位の状態機械で解く
- どちらも無い: 接続が閉じたら終わり（HTTP/1.0の既定）

**最後の1つだけが、接続断を正常終了として扱います。** `Content-Length`が
満たされていない場合と、terminal chunkの来ないchunkedは`Truncated`です。
半分のページを1ページとして見せないためで、この層全体がそのために
組んであります。

`Content-Encoding`は`identity`以外を拒否します。本文上限は呼び出し側が
渡します（`httpget`は無制限、ブラウザは2 MiB）。`Content-Length`が上限を
超える場合は本文を読む前に拒否します。どうせ拒否する数MiBは、誰かの帯域と
利用者の数秒です。

### ヘッダと本文を分ける

### ヘッダと本文を分ける

**ヘッダの終わりの空行で切り分け**、ヘッダは保持し（上限
`MAX_HEADER_BYTES` = 16 KiB）、**本文だけをsinkへ渡します**。以前は先頭4 KiBを
ヘッダごと保持し、残りは受信して捨てていました。ステータス行を見る手段では
あっても、何かを取ってくる手段ではありませんでした。

- 空行は2回の読み出しにまたがり得るので、走査は**溜めたバッファ**に対して
  行います。前回の末尾3 byteから探し直すので、読み直しは最小限です
- 空行が`MAX_HEADER_BYTES`までに現れなければ`HeadersTooLong`です。本文が
  どこから始まるか分からない以上、その先は当て推量になります
- ステータスコードは**報告するだけ**で、この層は判断しません。404の本文を
  取っておく価値があるかは呼び出し側の問いです。1行目がステータス行の形を
  していなければ`None`で、200と決めつけるよりそう言う方がましです

### 保存

`httpget`もカレントディレクトリへ、パスの最後の要素で保存します（`.part`と
改名の扱いはTFTPと同じ）。クエリ文字列は名前から外します（`?`はFATの予約文字
でもあります）。

- **パスが`/`または`/`で終わる場合は保存しません。** 名前にするものが無いため
  です。この形は元からのTCP疎通確認の使い方で、そのまま動きます——ヘッダを
  表示し、本文は数えて捨てます
- **2xx以外は保存しません。** エラーページが利用者の求めた名前でツリーに
  残るのを避けるためです。ヘッダは表示するので何が起きたかは見えます

実測は512,327 byte（512 KiBのファイル＋ヘッダ204 byte）が605 msで
**約827 KiB/s**です。**リンクの実力を示すのはTFTPの111 KiB/sではなくこちら**で、
差は複数セグメントを同時に飛ばして往復の待ちを隠せるかどうかによります。

## TLS

`src/net/tls.rs`はTLS 1.3クライアントである。`net::http`と同じく、TCP socketを
`SocketSet`に持ち、`poll`のあいだだけ`&mut Stack`と`&mut Rpc`を借りる。違うのは
出てくるものが平文であることで、HTTP層はこの上に載る（統合は未着手、下記「現状の範囲」）。

### 1 frameで戻るための構造

TLSエンジンは[embedded-tls](https://docs.rs/embedded-tls/) 0.19である。採用理由と
検討したほかの候補は[TLS_PLAN.md](TLS_PLAN.md)にある。

このライブラリのblocking APIは`open()`がhandshake全体を回してから返るため、
「1回のpollで決まった量だけ進んで返る」という契約と両立しない。そこでasync APIを
`Waker::noop()`で1 frameに1回だけpollする。futureがsocketを所有すると
`SocketSet`の借用をfutureへ持ち込むことになるので、両者のあいだにbyte queueを置く。

```text
smoltcp socket ──(cipher_rx/cipher_tx)── TLS future ──(plain_rx/plain_tx)── 呼び出し側
```

4本のqueueは`Rc<RefCell<Shared>>`で双方が所有する。自己参照structにならないので
unsafeも生ポインタも要らない。1回の`poll`は「socket→cipher_rx」「futureを1回poll」
「cipher_tx→socket」の順で、queueが枯れた時点でfuture内のawaitが`Poll::Pending`を
返してframe loopへ戻る。1 pollで復号するciphertextには予算（既定8 KiB）があり、
まとめて届いたbufferの消化に1 frameを使い切らない。

`cancel`はqueueへcancelledを立ててfutureを捨て、`close`がsocketをabortして
setから外す。closeを忘れるとsocketがsetに残るので、`Drop`はUARTへその旨を出す。

### 検証するものとしないもの

すべての接続で、serverの`CertificateVerify`をleaf証明書の公開鍵に対して検証し、
Finishedとすべてのrecordのaead tagを検証する。ライブラリの`NoVerify`は使わず、
これらを飛ばす経路をこのファイルは持たない。

それで分かるのは「相手が、提示した証明書の秘密鍵を持っている」ことだけである。
その証明書がURLのhostのものかは**確認していない**。chainを辿らず、rootを見ず、
名前を照合せず、有効期限も読まない。したがって能動的な攻撃者は自前の証明書で
接続を終端でき、上の検証はすべて通る。これが`Authentication::Unverified`で、
`TLS UNVERIFIED`と表示する。`SECURE`や鍵アイコンにはしない。

認証状態を外へ出すのは**handshakeが終わってから**である。`verify_certificate`は
証明書が主張する内容を控えるだけで、`verify_signature`が通って初めてそれを
`Outcome::Authenticated`にし、さらに`open()`が返って（server Finishedが検証されて）
初めて`Transaction::authentication`が答える。証明書を受け取っただけ、暗号化された
だけの段階で認証済み表示に変わることはない。

hostnameの照合をしないのは手抜きではない。信頼されたrootまでのchainが無い状態で
証明書中の名前を照合しても何も証明できない（誰でも自分で署名した証明書に任意の名前を
書ける）ので、照合するふりをするのは照合しないより悪い。

`Authentication::Pinned`はもう一段強い。leafのDER `SubjectPublicKeyInfo`のSHA-256が
firmwareに埋め込んだそのhost用のpinと一致した場合である。pinが登録されているhostで
鍵が一致しない場合は`tls-pin`で失敗し、`Unverified`へは落ちない。

### SPKI pin

pin tableは`src/net/pins.rs`で、実体は`tools/pins/generate.py`が
`tools/pins/pins.txt`から作る生成物（`src/net/pins/generated.rs`）である。
**実行時にpinを追加する方法は無く、TOFU（初回接続した鍵を覚える）もしない。**
初回に応答した鍵を覚えても、その瞬間に攻撃者がいれば攻撃者の鍵を覚えるだけで、
基板は誰も選んでいない鍵を抱えることになる。pinはコードと同じ経路、つまり
firmware更新でしか変わらない。

pinの対象は証明書全体ではなくDER `SubjectPublicKeyInfo`なので、**同じ鍵のまま
証明書を更新してもpinは変わらない**。1 hostにつき最大2 pin（現用と更新予定）で、
rotationは「新旧2 pinを載せたfirmwareを先に配る→serverの鍵を切り替える」の順に
行う。逆順にすると更新前の基板が繋がらなくなる（警告ではなく接続失敗になる）。

pin検索のkeyはURLの中の文字列そのもの、つまりSNIに送る名前と同じである。
IPv4リテラルにも登録できる。**証明書のSANやCNはpinningの判断材料にしない。**

`tools/pins/pins.txt`は**空である**。通常のbuildで`TLS PINNED`になる接続先は
無く、`https://`は常に未認証である。hostをpinすることは「firmware更新まで
他の鍵では絶対に話さない」という約束で、この基板が今している用途にその約束を
する理由が無いため。書式とrotation手順は[`../tools/pins/README.md`](../tools/pins/README.md)。

試験用のpinは`tls-fixture-pins` featureを付けたbuildにだけ入る。対応する
秘密鍵は`tools/tls/`にあり、それが安全なのは通常releaseに入らないからである。
`tools/pins/generate.py --check-release <elf>`がELF全体からpinの32 byteを
探して、入っていないことを確かめる。

証明書からの鍵の取り出しは`tab5-spki`にある。TLS経路で唯一「攻撃者が選んだ入力」を
解析する部分なので、workspace memberにしてhost testを置いてある。切り詰めた証明書の
全prefix、長さフィールドの改変、tagの取り違え、unused-bitが0でないBIT STRINGは
すべて拒否する。OpenSSLの出力と一致することも確認している（`spki/data/`）。

### 暗号方式

| 項目 | 値 |
| --- | --- |
| cipher suite | AES-128-GCM / SHA-256 |
| 鍵交換 | secp256r1 |
| `CertificateVerify` | ECDSA P-256 SHA-256、RSA-PSS SHA-256/384/512 |
| pin | leafのDER `SubjectPublicKeyInfo`のSHA-256、32 byte |
| 乱数 | ChaCha20 CSPRNG、種はSAR ADCノイズ源から32 byte（`src/entropy.rs`） |

真性乱数が得られない場合は`entropy`で失敗し、**1 packetも送らない**。
サイクルカウンタやMACアドレスへfallbackしない。`net::Stack`がTCPの初期シーケンス番号に
使っているそれらは、TLSの秘密値には使えない。

ライブラリはClientHelloでEd25519とECDSA P-384も広告する（署名方式リストが
ライブラリ内部でprivateなため、こちらから減らせない）。それらの証明書しか持たない
serverは`tls-cert`で失敗する。互換性の制限であって、検証を緩めているのではない。

### 上限

| 項目 | 値 |
| --- | ---: |
| record buffer（送受信各1本） | 16,640 byte |
| 復号待ち平文の滞留（背圧） | 64 KiB |
| 送信待ちciphertextの滞留（背圧） | 32 KiB |
| TCP socket buffer（送受信各1本） | 8 KiB |

入力に依存する確保は`try_reserve`を通し、失敗は`tls-limit`または`out-of-memory`として
接続を閉じる。record bufferはPSRAMヒープに置く。大きなローカル配列にすると
内部RAMのstack（128 KiB保証）を食うためである。

### ストリームの終わり方

TLSの正しい終わり方は`close_notify`だが、HTTP/1.0のserverは応答を書き終えたら
そのままTCPを閉じることが多い（Google、2026-08-28実測）。embedded-tlsから見ると
どちらもtransportの0バイト読みで、`IoError`として同じ形で返ってくる。そこで
`Io::read`が終端に達した事実をqueueへ記録し、handshake完了後の受信中に限って
「serverが終わった」と「接続が壊れた」を区別する。handshake中の0バイト読みは
従来どおり失敗である。

`close_notify`なしで終わった場合は`Stats::closed_without_notify`が立つ。
これ自体はエラーではないが、TLSとしては「末尾が切られていないこと」を保証できない
という意味なので、本文が完結しているかはHTTP側のframing（`Content-Length`・
chunked・close終端）が判断する。

### 失敗の名前

`entropy`、`link-lost`、`tls-connect`、`tls-timeout`、`tls-version`、`tls-alert`、
`tls-handshake`、`tls-cert`、`tls-pin`、`tls-pin-missing`、`tls-limit`、`cancelled`、
`out-of-memory`、`tls-local`。ライブラリ固有のenumはこの境界で正規化する。
証明書エラーをTCP timeoutに潰さず、逆にASN.1 parserの詳細を画面へ出さない。

`tls-alert`と`tls-handshake`は**どちらが断ったか**が違う。前者はserverがfatal
alertを送ってきた場合、後者はserverのhandshakeをこちらが解釈できずalertを送って
中断した場合である。後者を「serverが拒否した」と表示すると、こちら側の制約を
探しにserverの設定を見に行かせることになる。

serverのalertのうち`protocol_version`、`handshake_failure`、
`insufficient_security`の3つは`tls-alert`ではなく**`tls-version`**として上げる。
どれも「話せるものが1つも無い」という意味で、こちらがTLS 1.3・
AES-128-GCM-SHA256・secp256r1しか出さないことが原因だからである。TLS 1.2までの
serverはこの経路で`tls-version`になる（実測: 2026-08-28）。

alertの中身（`handshake_failure`、`protocol_version`、`unrecognized_name`など）は
**UARTにだけ**出す（[DIAGNOSTICS.md](DIAGNOSTICS.md)）。画面に出しても読み手には
できることが無い一方、シリアルを繋いでいる人には3つがまったく別の対処になる。

### 実測（2026-08-28、実機）

| 接続先 | 署名方式 | handshake | 総時間 | 1 pollの最長 | 平文 |
| --- | --- | ---: | ---: | ---: | ---: |
| `www.google.com` | ECDSA P-256 | 122 ms | 427 ms | 70.6 ms | 89,824 B |
| `www.rfc-editor.org` | RSA-PSS | 127 ms | 550 ms | 69.4 ms | 179,477 B |

**1回のpollの最長は約70 ms**である。これはhandshake中の1 pollで、鍵交換と署名検証が
ライブラリ内部の分割できない処理としてまとまっているためで、こちらから分割できない。
[TLS_PLAN.md](TLS_PLAN.md)の中止条件（1 stepが100 msを超える）には当たらないが、
57.3 Hzのframe loopでは約4 frame分の停止に相当する。ブラウザへ統合する際は、
handshake中は「読み込み中」の表示のまま止まって見える時間がこの長さになる。

### 現状の範囲

HTTP層とは`net::transport`経由で繋がっており、`tls`コマンド
（[CONSOLE_SHELL.md](CONSOLE_SHELL.md)）が`net::http::Transaction`を
TLS transportの上で回す。まだ無いのは`https://` URLのfetch、ブラウザ表示、
redirect規則、pin tableの生成で、段階分けは[TLS_PLAN.md](TLS_PLAN.md)にある。
`httpget`とブラウザは引き続き平文専用である。

## 制約

- IPv6なし。名前解決もAレコードだけで、AAAAは引けません
- DNSにキャッシュ・サーチドメイン・逆引き（PTR）・mDNS（`.local`）はありません。
  同じ名前を2回使えば2回問い合わせます
- サーバ機能（TFTPサーバ、HTTPサーバ、TLSサーバ）なし
- TLSはクライアントの1.3のみ。TLS 1.2以前、client certificate、PSK、session
  resumption、0-RTT、ALPN、DTLS、QUICはありません
- TLSは接続先のidentityを保証しません（未認証TLS）。`TLS PINNED`になる
  経路はpin tableが未実装のためまだありません
- 受信したファイルの保存先は`/tmp`だけです。8 MiBで、リセットで消えます
  （[`FILESYSTEM.md`](FILESYSTEM.md)）
- リンクが切れると古いアドレスを破棄します。管理対象のメニュー／保存profile接続は
  自動再接続とDHCPを行い、CLI接続は`wificonnect`と`ipconfig`の手動操作が必要です
- シェルは単一スレッドなので、長いコマンドの実行中は他のことが止まります
