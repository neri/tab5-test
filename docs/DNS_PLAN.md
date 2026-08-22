# DNSクライアント実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は現状文書
> （[`NETWORK.md`](NETWORK.md)）とコードを優先してください。

## 状態: **完了**（Stage 0〜5を実機確認済み）

コードと文書はStage 0〜5まで入っており、ビルドと`tools/check_elf_layout.py`は
通ります。現状文書は[`NETWORK.md`](NETWORK.md)です。

実機では、DNS本体の不具合は出ませんでした。代わりに**エラーの報告のしかた**で
2つ、**既存のDHCPクライアント**で1つ問題が見つかり、いずれも修正して再確認
済みです（下の「実機での判断記録」）。

| Stage | 状態 |
| --- | --- |
| 0 featureの追加とビルド確認 | 確認済み |
| 1 `Stack`へのDNSソケット常設とサーバ一覧の管理 | **実機確認済み** |
| 2 `net/dns.rs`の解決関数 | **実機確認済み**。タイムアウトとフォールバックは2度直した |
| 3 `nslookup`コマンド | **実機確認済み**（成功・NXDOMAIN・リゾルバ無しの3経路） |
| 4 既存コマンドのDNS対応 | **実機確認済み**。`Host:`は応答ヘッダに反射させて確認 |
| 5 文書化 | 完了 |
| 6 mDNS（`.local`）※任意 | 未着手・任意 |

**この計画は完了です。** 非目標のまま残しているのはIPv6・AAAA、キャッシュ、
サーチドメイン、逆引き、DNSSEC／DoT／DoHで、mDNS（Stage 6）も入れていません。
以降の作業は新しい計画書を立てて進めます。

## 方針

[`TCPIP_PLAN.md`](TCPIP_PLAN.md)は完了し、DHCP・ping・TFTP・HTTPまでが実機で
動いています。**DNS解決はそのときの非目標**で、`socket-dns`を無効にしたまま
残してありました。この計画はそれを有効にし、既存のネットワークコマンドが
ホスト名を受け付けるところまでを作ります。

プロトコル層を自前実装しない方針は[`NETWORK.md`](NETWORK.md)と同じです。DNSは
RFC 1035で完全に規定され、パケットキャプチャで外から観測できるので、
smoltcpの`socket-dns`をそのまま使います。**このリポジトリが書くのは、
ソケットをスタックに常設する部分・サーバ一覧をDHCPから流し込む部分・
「名前かアドレスか」を判定してコマンドへ渡す部分**の3つだけです。

DHCPは既に`Ipv4Config::dns_servers`へリゾルバのアドレスを持っています。
`ipconfig`はそれを表示しますが、現在は`dns (none; there is no resolver
anyway)`という行が示すとおり、**取得しても誰も使っていません**。この計画は
まずその配線をつなぐところから始めます。

## 到達目標と非目標

到達目標:

- `nslookup <name>` — 名前をAレコードで引き、得たアドレスを表示する
- `ping <name|a.b.c.d> [count]` — 名前でもアドレスでも受ける
- `tftpget <name|a.b.c.d> <file>` — 同上
- `httpget <name|a.b.c.d>[:port] [path]` — 同上。加えて`Host:`ヘッダに
  **アドレスではなく入力された名前**を載せる（バーチャルホストが引ける）
- `ipconfig` — リゾルバの表示を「使われている」表現に直し、静的設定でも
  リゾルバを手で指定できるようにする

非目標:

- **IPv6・AAAAレコード**。`proto-ipv6`が無効なので`RecordData::Aaaa`の
  分岐はコンパイルされません。Aレコードだけです
- **キャッシュ**。同じ名前を2回引けば2回問い合わせます。シェルは対話操作の
  速度でしか動かないので、TTLの管理を持ち込む価値がありません
- **サーチドメイン・hostsファイル・逆引き（PTR）**
- **DNSSEC、DoT、DoH**
- **mDNS**（`.local`）はStage 6の任意扱いです。`socket-mdns`とマルチキャスト
  グループ参加が要るので、他と切り離します
- `ipconfig <a.b.c.d/len>`のアドレス指定を名前で行うこと。インタフェースへ
  付けるアドレスを名前で書く意味がありません

## smoltcpの設定変更

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
    "socket-dns",              # 追加
    "dns-max-server-count-3",  # 追加
    "dns-max-result-count-4",  # 追加
    "auto-icmp-echo-reply",
] }
```

`dns-max-*`はsmoltcpの`build.rs`が読む**設定用feature**で、有効にしないと
既定値のままです。既定値は次のとおりで、**どちらも1**です。

| 設定 | 既定 | この計画での値 | 理由 |
| --- | --- | --- | --- |
| `DNS_MAX_SERVER_COUNT` | 1 | **3** | DHCPの`MAX_DNS_SERVER_COUNT`が3。1のままだと`Socket::update_servers`が黙って先頭1つに切り詰め、そのリゾルバが落ちているときに冗長構成の意味が消える |
| `DNS_MAX_RESULT_COUNT` | 1 | **4** | ラウンドロビンで複数のAレコードが返る名前で、`nslookup`が全部見せられる。溢れた分はsmoltcpが黙って捨てるのでエラーにはならない |
| `DNS_MAX_NAME_SIZE` | 255 | 既定のまま | RFC 1035の上限そのもの |

`socket-mdns`はStage 6まで有効にしません（`.local`で終わる名前の宛先を
smoltcpが勝手にマルチキャストへ差し替えるため、参加処理を入れる前に
有効にすると`.local`が必ず失敗するようになります）。

コードサイズは実測して記録します。FLASH XIP領域（`ROM_TEXT`は約3.9 MiB、
smoltcp導入後のIROMは334,900 byte）に対して、DNSソケットが問題になる規模には
なりません。

## モジュール構成

| 追加/変更 | 責務 |
| --- | --- |
| `src/net/dns.rs`（新規） | 名前を引いて`Ipv4Address`にする。`ping.rs`・`tftp.rs`・`http.rs`と同じく「スタックの上に載るクライアント1つで1ファイル」 |
| `src/net/stack.rs`（変更） | DNSソケットを常設し、DHCPと手動設定からサーバ一覧を流し込む。`start_query`を`connect_tcp`と同じ理由でメソッドとして持つ |
| `src/net.rs`（変更） | `pub mod dns;`と、モジュール一覧コメントへの追記 |
| `src/net/http.rs`（変更） | `Host:`ヘッダに名前を載せられるよう引数を1つ増やす |
| `src/app/shell.rs`（変更） | `nslookup`、`resolve_target`ヘルパ、既存3コマンドの引数処理、`ipconfig`の表示とリゾルバ設定、ヘルプ |
| `Cargo.toml`（変更） | 上記のfeature |

## 段階分け

### Stage 0: featureの追加とビルド確認

`Cargo.toml`へ上記3つのfeatureを足し、`cargo build --release`と
`tools/check_elf_layout.py`が通ることだけを確認します。コードはまだ書きません。

**確認**: ビルドが通ること。`socket-dns`を足しただけの状態で
IROM／DROMがどれだけ増えたかを記録する（Stage 5でNETWORK.mdへ書く）。

### Stage 1: `Stack`へのDNSソケット常設とサーバ一覧の管理

DNSソケットは**コマンドのたびに作るのではなく`Stack`に常設**します。
`Interface::poll`はソケットセットに居るDNSソケットしか再送・受信処理をしないので、
問い合わせの寿命とソケットの寿命を一致させると、ポンプの外でソケットを
作った瞬間に取りこぼしが起きます。TFTPやHTTPのソケットが使い捨てなのは
それらが「コマンド実行中しか存在しない会話」だからで、リゾルバの設定は
リースと同じ寿命です。

- `Stack::new`で`dns::Socket::new(&[], Vec::new())`を追加し、ハンドルを保持する。
  `alloc`があるので問い合わせスロットの`Vec`は空で始めてよく、
  `find_free_query`が必要に応じて伸ばす
- `apply_config`で`config.dns_servers`を`update_servers`へ渡す
- `clear_addresses`で`update_servers(&[])`する。**アドレスを失ったのに前の
  リゾルバを覚えているのは、リースを失ったのにアドレスを持ち続けるのと同じ
  嘘**です（[`NETWORK.md`](NETWORK.md)の「リンクが切れたら両方捨てる」と同じ理由）
- `Stack::set_static`にリゾルバの引数を足す。静的設定は今`dns_servers`が
  常に空なので、**手で指定できないと静的アドレスでは名前が一切引けません**
- `Stack::dns_servers()`アクセサを足し、`ipconfig`が「今ソケットが持っている
  一覧」を表示できるようにする（`Ipv4Config`の写しではなく実際に使われる値）

**確認**: `ipconfig dhcp`のあと`ipconfig`がリゾルバを表示し、
`ipconfig release`のあとは消えること。この時点ではまだ誰も引きません。

### Stage 2: `net/dns.rs`の解決関数

```rust
pub enum Error {
    /// No resolver is configured; nothing to ask.
    NoServers,
    /// The name is not something that can be put in a query.
    InvalidName,
    /// The servers answered, and the name does not exist.
    NotFound,
    /// Nobody answered in time.
    TimedOut,
    LinkLost,
    Local,
}

pub fn resolve(stack: &mut Stack, rpc: &mut Rpc, name: &str)
    -> Result<Ipv4Address, Error>;
```

`start_query`は**インタフェースの`Context`とソケットの両方**を要ります。
どちらも`Stack`のフィールドなので、`&mut Stack`を1つ持った呼び出し側からは
分割できません。`connect_tcp`と同じ形にして`Stack`のメソッドとして生やします
（この借用の形は[`TCPIP_PLAN.md`](TCPIP_PLAN.md)の判断記録に既にある話で、
DNSでもう一度同じ壁に当たります）。

流れ:

1. `stack.dns_servers().is_empty()`なら`NoServers`で即座に戻る。
   **これを先にやらないと区別が付かなくなります**（下の罠を参照）
2. `stack.start_dns_query(name)`（新設メソッド）で`QueryHandle`を得る
3. `pump_until`で完了を待つ。述語は`get_query_result`が`Pending`以外を
   返すこと
4. 結果を1回だけ取り出す。`Ok`なら先頭のAレコード、`Failed`なら`NotFound`
5. タイムアウトしたら`cancel_query`してから`TimedOut`

**想定される罠**:

- **サーバ一覧が空だと`Failure`が即座に返ります。** smoltcpの`dispatch`は
  `pq.server_idx >= servers.len()`を「全サーバを試し終えた」と解釈するので、
  0台の一覧では1回も送らずに失敗します。呼び出し側から見ると
  **「リゾルバが無い」と「その名前は存在しない」が同じ`Failed`になる**ので、
  問い合わせを始める前に一覧を見て別のエラーにします
- **`get_query_result`は結果を返した時点でスロットを解放し、空きスロットへの
  再呼び出しは`panic!`します。** `cancel_query`も同じです。「取り出したら
  もう触らない」「タイムアウト経路でだけ`cancel_query`する」を1か所に閉じ込め、
  `pump_until`の述語の中で`get_query_result`を呼んで結果を捨てるような
  書き方をしない（述語は何度も呼ばれます）。述語では`Pending`かどうかだけを
  見て、取り出しはループを抜けてから1回行う
- **smoltcpの内部タイムアウトは1サーバあたり10秒**（`RETRANSMIT_TIMEOUT`）で、
  再送間隔は1→2→4→8→10秒と伸びます。3台構成では内部的に最大30秒かかるので、
  こちらのタイムアウトをそれより短くするなら**自分で`cancel_query`する**責任が
  こちらにあります。シェルは単一スレッドで、待っている間は他が止まります。
  （**当初は5秒と書いていましたが誤りでした。** 10秒より短い上限では
  サーバの切り替えが起きず、2台目以降が一度も試されません。下の
  「タイムアウトはsmoltcpの予定から導く」を参照）
- **応答は送信元アドレスがサーバ一覧に載っているものだけ受け付けます**
  （`Socket::accepts`）。ルータによっては問い合わせ先と違うアドレスから
  返すことがあり、そのとき症状は**完全な無音のタイムアウト**です。疑ったら
  `netdump`でDNSの応答フレームが届いているかを見て切り分けます
- **UDPソケットはDNSソケットより先に照合されます。** `iface`の受信経路は
  `socket-udp`を全部見てから`socket-dns`を見るので、TFTPのエフェメラルポートが
  DNS問い合わせの送信元ポートと偶然一致すると応答を横取りされます。
  現状は解決が転送の**前**に終わるので重なりませんが、将来「転送中に引く」
  経路を足すときはこの順序を思い出してください
- **`start_query`は`&str`を取ります。** シェルの引数は`&[u8]`なので
  `core::str::from_utf8`を通し、失敗したら`InvalidName`にします。
  ラベルが64 byte以上・空ラベル・255 byte超も`InvalidName`／`NameTooLong`です
- アドレスが無いと`get_source_address`が`None`を返して`Failure`になります。
  既存のコマンドと同じく`has_address()`を先に見ます

**確認**: Stage 3の`nslookup`と同時に行います。

### Stage 3: `nslookup`コマンド

`nslookup <name>`。解決だけを行う最小のコマンドで、既存コマンドを触る前に
**DNSそのものが動くことを単独で確かめる場所**として先に入れます。

```text
> nslookup example.net
server 192.168.1.1
example.net has address 93.184.216.34
in 12 ms
```

- 複数のAレコードが返ったら全部表示する（`DNS_MAX_RESULT_COUNT`を4にした理由）
- `Line`は80桁なので、長い名前は黙って切れます。判定には
  アドレスの行を見てください
- 引数がそのままIPv4アドレスとして読める場合も、`nslookup`は**問い合わせを
  投げます**（このコマンドの目的はDNSを試すことなので、ここだけは
  先読みのショートカットを入れない）

**実機確認**:

- `wificonnect`→`ipconfig dhcp`のあと、LAN内の名前と外部の名前の両方が引けること
- 存在しない名前が`NotFound`になること（タイムアウトではなく即座に返るはず）
- `ipconfig release`のあとは`NoServers`になること
- `netdump`で53番宛のUDPが出ていること

### Stage 4: 既存コマンドのDNS対応

`ping`・`tftpget`・`httpget`の宛先を「アドレスまたは名前」にします。
共有ヘルパを1つ置きます。

```rust
/// Turns a command argument into an address: a literal `a.b.c.d` is taken
/// as-is, anything else is resolved. Reports the failure itself and
/// returns `None`, so callers just `?`-shape out of the command.
fn resolve_target(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
    text: &[u8],
) -> Option<Ipv4Address>;
```

**数字のアドレスは先に`parse_ipv4`で判定し、その場合は問い合わせません。**
リゾルバが無い状態でも`ping 192.168.1.1`が今までどおり動くことは、
切り分けの手段として残す価値があります。

**引数の処理順が変わります。** 現在の`cmd_ping`・`cmd_tftpget`・`cmd_httpget`は
`net_session`より**前**に宛先をパースしていますが、名前の解決にはスタックが
要るので順序が入れ替わります。

1. 引数の**形**だけ検査する（宛先が空でないか、`count`が1..64か、
   ファイル名があるか）。ここで弾かれるものは、C6のリンクを立てる前に
   使い方の誤りとして報告する
2. `net_session`でリンクとスタックを用意する
3. `has_address()`を確認する
4. `resolve_target`で宛先を確定する

`httpget`の`<host>[:port]`は、**コロンで切ってから**ホスト部を解決します
（`parse_ipv4_port`を、アドレス専用のものからホスト部とポートを返すものへ
分ける）。加えて`http::get`へ「`Host:`に書く文字列」を渡す引数を足します。
現在はアドレスを10進で組み立てていますが、**名前で引いたときにアドレスを
`Host:`へ書くとバーチャルホストが引けません**。名前が与えられたときは名前を、
アドレスが与えられたときは今までどおりアドレスを載せます。

出力は「何を引いて何になったか」が見えるようにします。

```text
> ping example.local
example.local is 192.168.1.42
pinging 192.168.1.42
seq 0: 3 ms
```

**実機確認**: 3コマンドすべてを名前で実行して成功すること。加えて
**数字のアドレスでも今までどおり動くこと**（回帰）と、
リゾルバが無い状態で数字のアドレスなら動き、名前なら`NoServers`になること。

### Stage 5: 文書化

- [`NETWORK.md`](NETWORK.md): DNSの節を新設。層構造の表に`src/net/dns.rs`、
  シェルコマンドの表に`nslookup`と各コマンドの引数変更、smoltcpの設定の
  featureリストと`dns-max-*`の理由、「制約」から「DNS解決なし」を外して
  残る制約（IPv6・キャッシュ・サーチドメイン・逆引き）に書き換える。
  冒頭の「DNS解決とIPv6は入れていません」も直す
- [`../DESIGN.md`](../DESIGN.md): 「ドキュメント構成」表のNETWORK.mdの説明と、
  「制約」の`**DNS解決とIPv6、サーバ機能、TLSはありません**`の行
- [`FILE_LAYOUT.md`](FILE_LAYOUT.md): `src/net/dns.rs`の行を追加
- [`DIAGNOSTICS.md`](DIAGNOSTICS.md): `NET:`接頭辞の説明に、DNS関係のログを
  足すなら追記する
- `shell.rs`の`HelpEntry`: `nslookup`を追加し、`ping`・`tftpget`・`httpget`の
  `usage`を`<host|a.b.c.d>`の形に直す。`httpget`のヘルプにある
  「no name resolution」は事実でなくなる
- **`README.md`は変更しません。**現時点でネットワークコマンドにもDNSにも
  言及がないので、この作業で古くなる記述はありません。もしStage 5の時点で
  該当箇所があれば、直さずに報告します

### Stage 6: mDNS（`.local`）※任意

他のStageから独立していて、要らなければ入れません。

- `socket-mdns`を有効にすると、smoltcpは`.local`で終わる名前の宛先を
  **サーバ一覧ではなく224.0.0.251:5353へ差し替えます**
- 受け取るには`Interface::join_multicast_group(Ipv4Address::new(224,0,0,251))`が
  要ります。`IFACE_MAX_MULTICAST_GROUP_COUNT`は既定4なので枠は足ります
- 参加するとIGMPのmembership reportが出ます。`netdump tx`で確認できます
- [`NETWORK.md`](NETWORK.md)に既に書いてあるとおり、アドレスを付ける前でも
  mDNSのマルチキャスト（`01:00:5E:00:00:FB`宛）は`netdump`に見えています。
  つまり**フレームは届いており、受け取る側が居ないだけ**です

**先に有効にしてはいけません。** 参加処理を入れる前に`socket-mdns`だけを
有効にすると、それまでユニキャストのリゾルバへ投げられていた`.local`が
マルチキャストへ回されて必ず失敗するようになります（環境によっては
ルータが`.local`を引けていたものが壊れます）。

## 検証環境

- PC側にdnsmasqを立てるか、ルータのDHCPが配るリゾルバをそのまま使う
- 存在しない名前（`no-such-name.invalid`）でNXDOMAINの経路を確認する
- リゾルバを1台だけ**故意に到達不能なアドレス**にして、
  タイムアウトと次サーバへのフォールバックを確認する（`ipconfig`の
  静的設定でリゾルバを指定できるようにするのは、この試験のためでもあります）
- `netdump` / `netdump tx`で53番のやり取りを外から確認する

## 参照

- RFC 1035（DNS）、RFC 6762（mDNS）
- smoltcpの`src/socket/dns.rs`（`accepts` / `process` / `dispatch`）と
  `src/iface/interface/udp.rs`（UDPソケットを先に照合する順序）
- [`NETWORK.md`](NETWORK.md)（現状のIPv4層）
- [`TCPIP_PLAN.md`](TCPIP_PLAN.md)（借用の形・ポンプ・背圧についての判断記録）

## 実装時の判断記録

### リゾルバ一覧の正は`Ipv4Config`側に置いた

計画では`Stack::dns_servers()`を「今ソケットが持っている一覧」と書きましたが、
**smoltcpのDNSソケットは一覧を読み返すAPIを持ちません**（`update_servers`は
あるが getter が無い）。そこで`Ipv4Config::dns_servers`を正とし、
`install_dns_servers`が設定からソケットへ一方向に写す形にしました。
2つが食い違える窓はその関数の中だけです。

### 切り詰めは`apply_config`と`set_dns_servers`に置いた

`update_servers`は`DNS_MAX_SERVER_COUNT`を超えた分を黙って捨てるので、
設定側を切り詰めずに置くと**`ipconfig`が問い合わせ先にならないサーバを
表示します**。設定へ入る2か所で`truncate`し、表示と実際を一致させました。
`install_dns_servers`側にも`take`を残していますが、これは配列へ書く場所で
上限を守らせるためで、実際に落ちることはありません。

### `resolve`は`&str`ではなく`&[u8]`を取り、`Answer`を返す

計画では`start_query`に合わせて`&str`としていましたが、シェルの引数は
`&[u8]`です。変換を`resolve`の中に置くと、UTF-8でないこと・空ラベル・
長すぎる名前がすべて`Error::InvalidName`という1つの概念にまとまり、
呼び出し側が前もって検査する必要がなくなります。

返り値も`Ipv4Address`ではなく`Answer { addresses, elapsed_ms }`にしました。
`nslookup`が複数のAレコードと所要時間を出すのに必要で、宛先だけ欲しい
`resolve_target`は先頭を取ります。

### `get_query_result`の1回きりの取り出しは、述語の中で捕まえる形にした

`pump_until`の述語は何度も呼ばれ、`get_query_result`は結果を渡すと
スロットを解放し、空きスロットへの再呼び出しは`panic!`します。
「述語では`Pending`かどうかだけ見る」と計画に書きましたが、
**smoltcpには覗くだけのAPIがありません**（`get_query_result`が唯一の観測手段で、
それが消費してしまう）。そこで述語の中で呼び、`Pending`以外だったら
外側の`Option`へ捕まえて`true`を返す形にしました。`pump_until`は述語が
`true`を返した時点で戻るので、取り出しはちょうど1回です。
`outcome`が`None`であることが「まだスロットが残っている」ことと同値になり、
`cancel_query`を呼んでよい経路がその1か所に閉じます。

### `ipconfig dns`は位置引数ではなく動詞にした

計画では静的設定の`gateway`の後ろに続ける案でしたが、動詞にすると
**DHCPで取ったリースに対しても使えます**。到達不能なリゾルバを指定して
フォールバックを見る試験は、静的アドレスでしかできないより、リースの上で
できるほうが実際の経路に近くなります。

### `httpget`の`Host:`には解決前の文字列を載せる

`parse_ipv4_port`を`split_host_port`に置き換え、ホスト部を**解釈せずに
そのまま返す**ようにしました。`http::get`は接続先アドレスと`Host:`用の
文字列を別々に受け取ります。名前で指定したときにアドレスを`Host:`へ書くと、
1つのアドレスを複数サイトで共有しているサーバがどのサイトか判断できません。

### 引数の処理順が変わった

`ping`・`tftpget`・`httpget`は宛先のパースが`net_session`より前にありましたが、
名前の解決にはスタックが要るので後ろへ移りました。**形の検査だけは前に
残して**あります（宛先が空、`count`が範囲外、ファイル名が無い）。
使い方の誤りを報告するためにC6のリンクを立てるのは遅すぎるためです。

### タイムアウトはsmoltcpの予定から導く（当初案の5秒は誤り）

計画では「5秒程度で打ち切る」と書きましたが、**この値ではフォールバックが
起きません**。smoltcpが次のサーバへ移るのは今のサーバで
`RETRANSMIT_TIMEOUT`＝10秒が経ってからなので、5秒で`cancel_query`すると
切り替えの前に終わってしまい、**2台目以降は一度も試されません**。
`dns-max-server-count-3`で枠を確保した意味も、`Cargo.toml`にそう書いた
「ネットワークが提示した冗長性」も、そのままでは嘘になります。

最初の修正は`10,500 × min(台数, 2)`（＝2台で21秒）としましたが、これも
誤りでした。理由は下の「枯渇とNXDOMAINは同じ`Failure`」を参照。
最終的な値は**`RETRANSMIT_TIMEOUT`＝10秒という1つの区切りの、内側**に
置いています。

- `PER_SERVER_MS` = **10,000**。smoltcpの区切りそのもの
- `SINGLE_SERVER_MS` = **5,000**（1台のとき）。乗り換え先が無いので
  「死んだと判断するまで」だけ
- `FALLBACK_MS` = **13,000**（2台以上のとき）＝10秒＋3秒。
  **2台目に丸10秒を与える必要はありません**。10秒は待つのをやめる区切りで
  あって応答に要る時間ではなく、生きているリゾルバはミリ秒で答えます

3つの大小関係は`const _: () = assert!(...)`でコンパイル時に検査します。
このファームウェアはreleaseでしかビルドしない（debugはRAMに収まらない）ので
`debug_assert!`では検査になりません。

### コードサイズ

| | IROM | DROM |
| --- | --- | --- |
| 作業前 | 344,222 | 130,776 |
| featureを足しただけ（Stage 0） | 347,980 | 130,776 |
| 実装後（DHCP修正とフォールバックを含む） | 354,526 | 130,776 |

合計で**+10,304 byte（約10.1 KiB）**です。featureを有効にしただけで
+3,758 byte増えるのは、`SocketSet`が持つ`Socket` enumにDNSの腕が生えて
`Interface::poll`のdispatchから参照されるため、使っていなくても
リンクされるからです。

## 実機での判断記録

### Stage 1の確認中に、DHCPの既存不具合が出た（修正済み・再確認待ち）

`ipconfig release`のあと`ipconfig dhcp`をしてもアドレスが復活しない、と
実機で報告がありました。**DNSの作業で入れたものではなく、TCP/IP実装の
時点からあった不具合です**（`start_dhcp`はこの計画の差分で触っていません）。

`ipconfig release`は`clear_addresses`しか呼ばず、DHCPソケットに触れて
いませんでした。一度リースを取ったソケットは`Renewing`状態のままなので、
次の`ipconfig dhcp`はそれを再利用してDISCOVERを出さず、自分の更新タイマを
待ちます。15秒後に出るのは`no DHCP answer; is the station associated?`で、
**原因はこちら側なのに案内はAPを疑わせます**。放置すればreleaseした
アドレスが更新タイマで黙って復活する、という2つ目の症状もありました。

修正は`start_dhcp`で`dhcpv4::Socket::reset()`を呼ぶことと、`release`を
`Stack`のメソッドにしてソケットごと捨てることの2つです。**リセットを
`clear_addresses`へ入れてはいけません**——リースを失ったときの
`Event::Deconfigured`も同じ関数を通り、そこはクライアントを走らせたまま
再取得させたい場面だからです。詳細は[`NETWORK.md`](NETWORK.md)の
「DHCPクライアントの寿命」。

この修正自体の実機確認はまだです。

### 枯渇とNXDOMAINは同じ`Failure`（実機で発覚・修正済み）

到達不能なリゾルバ2台で`nslookup`したところ、期待した
`the resolver did not answer`ではなく`example.com: no such name`が出ました。

**smoltcpは「サーバを全部試し尽くした」と「NXDOMAIN」を同じ
`State::Failure`で表します。** `get_query_result`はどちらも`Err(Failed)`を
返すので、呼び出し側からは区別できません。当時の予算は21秒、smoltcpの
枯渇は`10秒 × 2台`＝20秒ちょうどなので、**枯渇の方が先に起き**、こちらの
タイムアウトには一度も到達していませんでした。

区別する手段が無い以上、**区別しなくて済む位置に予算を置く**のが答えです。
枯渇点（`10秒 × 台数`）より手前で必ず打ち切れば、観測される`Failure`は
本物の応答由来だけになり、`no such name`と報告して正しくなります。
時間で見分けるような当て推量は入れていません。

なおこの修正は待ち時間の短縮も兼ねています（2台で21秒→13秒、
1台なら5秒）。

### 実機で見つからなかったもの

Stage 1〜4の実機確認で、**DNSの経路そのものは一度も失敗しませんでした**。
解決・NXDOMAIN・リゾルバ無し・フォールバックのいずれも、コードを書いた
ときの想定どおりに動いています。実機で直したのは全て**「失敗をどう伝えるか」**と
**既存のDHCPクライアント**で、計画時に洗い出した罠（`get_query_result`の
パニック、空のサーバ一覧、UDPソケットの照合順）はどれも踏みませんでした。

計画段階でsmoltcpのソースを読んで書き出した罠は有効でしたが、
**`State::Failure`が2つの意味を持つことだけは読み落としていました**。
`dispatch`と`get_query_result`を別々に読んで、状態の合流に気づかなかった
のが原因です。
