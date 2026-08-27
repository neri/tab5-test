# TLS／HTTPS実装計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: **Stage 8まで完了（初期範囲の実装は完了、2026-08-28実機受入済み）**

| Stage | 内容 | 状態 |
| --- | --- | --- |
| 0 | TLS実装候補、最低限の暗号検証、メモリ、非ブロッキング駆動の成立確認 | 選定完了。実測はStage 3以降 |
| 1 | RTCのUTC化と、既定JST（`+0900`）を分離した壁時計API | 完了（2026-08-28実機確認） |
| 2 | ESP32-P4の真性乱数とCSPRNG | 完了（2026-08-28実機確認） |
| 3 | 未認証TLS 1.3クライアントの最小接続 | 完了（2026-08-28実機確認、Stage 4と同時） |
| 4 | smoltcp上の中断可能なTLS transaction | 完了（2026-08-28実機確認、Stage 3と同時） |
| 5 | HTTP transactionを平文／TLS共通transportへ分離 | 完了（2026-08-28実機確認） |
| 6 | 未認証`https://`、redirect、警告表示への統合 | 完了（2026-08-28実機確認） |
| 7 | firmware組み込みSPKI pinning | 完了（2026-08-28実機確認） |
| 8 | CLI診断、実機受入、回帰、現状文書への反映 | 完了（2026-08-28実機受入） |
| 9 | 公開CA、hostname、有効期間を含むWeb PKI検証 | 条件成立時に別途判断 |

## この計画で固定する決定

- RX8130CEのカレンダーは**UTC**を保持する。将来、証明書時刻検証を導入する場合は
  UTCから作ったUnix秒だけを使い、表示用タイムゾーンを混ぜない。
- 表示とFAT timestampに使う既定タイムゾーンは**JST `+0900`**
  （UTCから`+09:00`、オフセット`+540`分）とする。
- 将来ほかのタイムゾーンを扱える型と呼び出し境界はこの計画で作る。ただし、
  **タイムゾーンを変更するUI／CLI、設定値の保存場所、夏時間規則は別途検討**し、
  この計画には入れない。したがって本計画完了時の実効値は常にJSTである。
- TLSはクライアントだけ、初期対応はTLS 1.3だけとする。TLSサーバ、TLS 1.2、
  client certificate、PSK、session resumption、0-RTTは対象外とする。
- 初期版は、真性乱数をseedにしたCSPRNG、TLS 1.3のkey exchange、AEAD、transcript、
  `CertificateVerify`、Finishedを検証する。ただしcertificate chain、hostname、
  証明書の有効期間は検証せず、接続先identityを保証しない**未認証TLS**とする。
- 未認証TLSは受動的な盗聴を防ぐが、active man-in-the-middleを防がない。画面とCLIでは
  必ず`TLS UNVERIFIED`と表示し、`SECURE`、鍵アイコン、検証済みHTTPSと誤認する表示をしない。
- libraryの`NoVerify`をそのまま利用して`CertificateVerify`まで省略する経路は作らない。
  未認証profileでも、peerが提示したleaf public keyに対応するprivate keyを持つことと、
  handshake／recordの完全性は確認する。
- 固定接続先を認証する第2段階として、hostnameごとのSHA-256 SPKI pinをfirmwareへ
  組み込む。pin一致時だけ`TLS PINNED`と表示し、不一致を未認証TLSへfallbackしない。
- Mozilla系root集合による公開CA検証は初期完了条件にしない。任意の公開Webを本人確認付きで
  開く、credentialを送る、永続ファイル／設定／firmwareを取得する、または表示内容を
  信頼して操作する用途が入る時点でStage 9を別途見積もる。
- 平文HTTPは診断とLAN内fixtureのため残す。HTTPSからHTTPへのredirectは
  downgradeとして拒否し、黙って平文へ落とさない。HTTPからHTTPSへのredirectは許可する。
- runtimeでcertificateやpublic keyを自動登録するTOFUは行わない。pinの追加／更新は
  firmware更新だけで行う。

## 背景と現状

現状のIP層は[`NETWORK.md`](NETWORK.md)のとおり、ESP32-C6のESP-Hostedが渡す
802.3 frameをsmoltcp 0.14へ載せています。`src/net/http.rs`の`Transaction`は
TCP socketを所有し、1回の`poll`で決まった量だけ進んで返るため、ブラウザは
読み込み中も入力とWi-Fi受信を処理できます。同期`httpget`も同じ`Transaction`を
完了まで回すwrapperであり、HTTP parserは1つです。

[`BROWSER.md`](BROWSER.md)のURL型はすでに`http`と`https`を区別し、HTTPSの既定portを
443と認識します。ただし`Scheme::is_fetchable`はHTTPだけを返し、HTTPS linkは
「未対応」と表示します。この区別はdowngradeを防ぐ境界として残し、Stage 6で
「TLS transportを用意できるHTTPS」もfetch可能に変えます。

現在のbrowserはform送信、cookie、認証、JavaScriptを持たず、`httpget`の保存先も
再起動で消える`/tmp`だけです。この初期用途では、公開Web全体のidentityを保証する費用より、
HTTPS専用serverと通信でき、受動的な盗聴を防げることを先に取ります。ただし未認証TLSで
得たpage、link、downloadはactive attackerに差し替えられるものとして扱い、credential送信、
永続設定、実行ファイル、firmware更新の入力に使いません。

RTC driver（[`RTC.md`](RTC.md)）はタイムゾーンを持たないカレンダー値を返します。
filesystem側は現在その値をlocal timeとしてFAT directory entryへ書いています。
本計画ではRTCの意味をUTCへ固定し、将来の公開CA検証用UTCと表示／FAT用JSTを明示的に
分けます。未認証TLSとSPKI pinningはRTCを接続条件にしません。

Stage 0開始前のrelease baselineは次を採取し直します。2026-08-27時点の参考値は
`tools/check_elf_layout.py`で次のとおりです。

```text
IRAM=10100  DRAM-rodata=1364  DROM=589528  IROM=814210  stack=189952
```

DROMはfont bitmapのため0x90000 byteまで広げてあり、末尾に約91 KiBのpaddingが
あります。初期版のpin tableは小さい固定生成物とし、公開root store用の容量はStage 9で
別途測ります。足りなければ`memory.x`のDROM／IROM境界を64 KiB単位で移します。
内部RAM stackは既存assertどおり128 KiB未満にしません。TLS recordと受信certificateは
大きな局所配列にせずPSRAM heapへ置きます。

## 到達目標

- `https://` URLへ接続し、TLS 1.3でHTTP responseを取得できる。
- DNS名をSNIへ送り、表示URL、接続先、SNI、`Host:`、pin検索keyを1つの`Url`から生成し、
  別々の文字列を正にしない。
- 未認証profileでもserverの`CertificateVerify`、Finished、全application recordのAEADを
  検証し、失敗した接続のapplication dataをHTTP層へ渡さない。
- pin登録済みhostnameではleafのDER `SubjectPublicKeyInfo`をSHA-256で照合し、
  一致した接続だけを認証済みとする。pin未登録とpin不一致を区別する。
- RTCが読めない、BCDが不正、`VLF`／`STOP`が立っている場合も、未認証TLSとpinningは
  接続できる。Stage 9の公開CA profileだけは時刻を取得できなければ通信開始前に失敗する。
- 既存のHTTP header／chunked／`Content-Length`／close終端、redirect、本文上限、
  `.part`保存をTLS上でも同じコードで処理する。
- ブラウザの読み込み中止、Wi-Fi再接続、socket回収の契約をHTTPSでも維持する。
- malformed certificate、handshake signature不正、pin未登録、pin不一致、TLS alert、
  record上限超過を区別して診断できる。
- 平文HTTPの既存fixture、512 KiB download、ブラウザ巡回、USB input、表示に
  回帰がない。

## 非目標

- TLSサーバ、HTTPSサーバ、client certificate、相互TLS
- TLS 1.2以前、DTLS、QUIC、HTTP/2、HTTP/3、WebSocket
- cookie、Basic／Bearer認証、password入力、credential保存
- OCSP、CRL download、Certificate Transparency、DANE
- proxy、CONNECT、VPN、IPv6、IDNA変換
- SNTP／NTPによる自動時刻設定
- タイムゾーン設定コマンド、設定保存領域、夏時間、IANA tz database
- 初期版での公開CA root store、certificate path building、SAN、証明書有効期間の検証
- 実行時のCA／pin追加・削除、利用者証明書のimport、TOFU
- 暗号hardware accelerator。Stage 0のsoftware実測が中止条件に当たった場合だけ
  別計画として検討する

## 時刻とタイムゾーン

### 保存する時刻はUTCだけ

RX8130CEにはタイムゾーンを記録する場所がありません。レジスタへ書く値をUTCと定義し、
`rtc set <YYYY-MM-DD> <HH:MM:SS>`の入力もUTCとします。既存利用者がlocal timeを
設定済みの場合は意味が変わるため、Stage 1導入後に一度UTCで設定し直す必要があります。
helpと実行結果には`UTC`を必ず表示し、暗黙のlocal timeとして受け付けません。

`rtc`表示は少なくとも次の形にします。

```text
UTC  2026-08-27 03:00:00
JST  2026-08-27 12:00:00 +0900
```

UTCからUnix秒への変換、Unix秒からcalendarへの逆変換、offsetをまたぐ日／月／年の
繰り上がりはpure codeへ分離し、host testを置きます。RX8130CEの有効範囲は現状どおり
2000〜2099年です。leap year、月末、年末、`2000-02-29`、`2099-12-31`のJST変換を
境界fixtureにします。

### 将来のタイムゾーンを型で隔離する

タイムゾーンは当面、次の情報を持つ固定offsetとして扱います。

```text
TimeZone { name: "JST", offset_minutes: 540 }
```

呼び出し側は定数を直接足さず、`wall_clock::local_datetime(utc, timezone)`のような
APIを通します。既定値を返す`default_timezone()`はJST固定です。将来、設定方法と
保存方法が決まったときはこの取得元だけを差し替えます。Stage 9の公開CA検証だけが
`wall_clock::unix_time_utc()`を使い、未認証TLSとSPKI pinningは時計を参照しません。

FAT timestampはタイムゾーンを格納できないため、従来の利用感を維持して既定JSTへ
変換したcalendarを書きます。将来timezoneが変更可能になった場合も、FAT timestampは
その時点のlocal timeを書き、TLS時刻はUTCのまま変えません。

### 将来の公開CA検証で時計を信用できる条件

公開CA検証用の壁時計は、1回の取得でcalendarとstatusを検査し、次をすべて満たす場合だけ
Unix秒を返します。このAPIはStage 1で用意しますが、Stage 9まで接続条件にはしません。

- `read_datetime`が成功し、calendarが2000〜2099年の有効な日時である
- `read_status`が成功する
- `Status::voltage_low()`がfalse
- `Status::stopped()`がfalse

失敗は「時刻が分からない」であり、epoch 0やbuild日時へ置き換えません。Stage 9の
公開CA profileは`clock-unset`／`clock-invalid`として失敗します。平文HTTP、未認証TLS、
SPKI pinning、組み込みページは引き続き使えます。

## TLS方式とライブラリ選定

第一候補はpure Rust、`no_std`、TLS 1.3 clientを持つ`embedded-tls`です。ただし公式文書が
work in progressと明記しており、blocking／async APIを現在のframe駆動
`Transaction`へ安全に保持できるか、`NoVerify`を使わずにleaf public keyで
`CertificateVerify`を検証できるかをStage 0で実証します。標準verifierが公開Web PKIを
完全に扱えることは初期選定ゲートにしません。

比較対象はrustlsのunbuffered APIです。これはcaller所有bufferと明示的な状態機械を持ち、
`no_std`の駆動形は本プロジェクトに合いますが、ESP32-P4向けに使える暗号providerが
必要です。built-in providerがtargetへ通らず、第三者providerの`no_std`品質も受入条件を
満たさない場合は採りません。

Stage 0で固定するもの:

- crate名、version、`default-features = false`と有効feature
- TLS 1.3 cipher suite（初期候補AES-128-GCM／SHA-256）
- key exchange（初期候補secp256r1）
- `CertificateVerify`署名方式。最低限RSA PSS SHA-256とECDSA P-256 SHA-256を候補にする
- 未認証profileのsignature verifierと、SPKI抽出／SHA-256 pin tableの形式
- 1回のframeで進めるAPI形、buffer所有、cancel／close方法
- 最大record、certificate、chain、handshake総量
- release IROM／DROM／内部stack／PSRAM peakとhandshake時間

### 選定結果（2026-08-28）

**`embedded-tls` 0.19.0を採用**します。ゲートの判定は次のとおりです。

| ゲート | 判定 | 根拠 |
| --- | --- | --- |
| targetへ`std`／OS socket／native libなしでlink | ○ | `riscv32imafc-unknown-none-elf`向けに`default-features = false`でbuildできた |
| `NoVerify`なしで`CertificateVerify`とFinishedを検証 | ○ | `TlsVerifier`はcrate rootから実装できる。leaf DERの`SubjectPublicKeyInfo`を自前で取り出し、p256／rsaで検証する自作verifierをPoCでbuild済み |
| SPKI一致だけを認証済みにし不一致を拒否 | ○ | 同じverifier内でSHA-256照合し、不一致は`InvalidCertificate`。`Unverified`へ落ちる経路を作っていない |
| frame loopから中断可能に駆動、dropでsocketを漏らさない | ○ | 下記「駆動方式」をPoCでbuild済み |
| certificate／record長を入力前に制限 | ○ | record bufferは呼び出し側所有の`&mut [u8]` |
| crypto処理中の長時間停止 | 未測定 | 実機が要る。Stage 3で測る |

rustls unbufferedは不採用です。`no_std`で使える暗号providerが
`rustls-rustcrypto` 0.0.2-alphaしかなく、「第三者providerの`no_std`品質」の
条件を満たしません。

### 駆動方式

`embedded-tls`のblocking APIは`open()`がhandshake全体を返らずに回すため、
1 frameで戻る契約と両立しません。async APIを`Waker::noop()`で自前にpollする形にします。
自己参照futureを避けるため、TLS engineとsocketの間にbyte queueを置き、
queueを`Rc<RefCell<..>>`で共有します。

```text
smoltcp socket ──(cipher_rx/cipher_tx)── TLS future ──(plain_rx/plain_tx)── HTTP Transaction
```

1回の`poll`は「smoltcpを回す→TLS futureを1回pollする→smoltcpを回す」で、queueが
空になるとfuture内のawaitが`Poll::Pending`を返してframe loopへ戻ります。futureは
`Box::pin`で所有し、socketはfutureの外にあるので`close(stack, rpc)`の契約は
現在のHTTP `Transaction`と同じままです。unsafeも生ポインタも使いません。

### 固定する構成

| 項目 | 値 |
| --- | --- |
| crate | `embedded-tls` 0.19.0、`default-features = false` |
| feature | `alloc`（`rsa`が要求）。`rsa`はRSA PSS検証のため。`rustpki`は自作verifierを使うので有効にしない |
| cipher suite | `Aes128GcmSha256` |
| key exchange | secp256r1（`p256`） |
| `CertificateVerify` | ECDSA P-256 SHA-256とRSA PSS SHA-256。他は`tls-cert`で明示的に失敗 |
| SPKI pin | leafのDER `SubjectPublicKeyInfo`のSHA-256、32 byte |
| CSPRNG | ChaCha20（`rand_chacha` 0.3）、種はSAR ADCノイズ源から32 byte |

依存として`sha2`／`digest`／`p256`／`rsa`が直接必要です。`embedded-tls`はこれらを
re-exportしないため、自作verifierが自分で持ちます。

### 未測定の項目

次はStage 3で実コードがリンクされてから測ります。Stage 0時点では測れません。

- release IROM／DROM／内部stack／PSRAM peak（dead code eliminationで消えるため）
- ClientHelloからhandshake完了までの時間、最長の不可分処理、総poll回数
- C6 rx queued／delivered／dropped、link recovery、display underrun

Stage 2完了時点のbaselineは次のとおりです（2026-08-28）。

```text
IRAM=10336  DRAM-rodata=1364  DROM=589528  IROM=826062  stack=189952
```

Stage 0開始前（2026-08-27）からの増分はIRAM +236、IROM +11,852 byteで、内訳は
暦計算（`tab5-time`）、壁時計、`regi2c`の共有化、ADCノイズ源とChaCha20です。

**選定ゲート**: 次を1つでも満たせない候補は採用しません。

- `riscv32imafc-unknown-none-elf`へ`std`、OS socket、native libraryなしでlinkできる
- `NoVerify`なしでserver `CertificateVerify`とFinishedを検証できる
- pin登録済みfixtureでSPKI一致だけを認証済みにし、不一致を拒否できる
- browserのframe loopから中断可能に駆動でき、dropだけでsocketを漏らさない
- certificate／record長を入力前に制限できる
- crypto処理中にC6受信経路を壊す長時間停止が起きない

Stage 9で公開CA検証へ進む場合は、上記に加えて複数root、順不同のintermediateを使った
path building、Basic Constraints、Key Usage、critical extension、DNS／IP SAN、
有効期間を検証できることを別ゲートにします。現行`embedded-tls`の標準verifierだけで
満たすと仮定せず、custom verifier、別library、C providerのいずれにするか再選定します。

## 接続先identityの段階

TLS transportと接続先identityの判定を分離します。接続ごとに次のいずれかを保持し、
HTTP層とUIへ明示的に渡します。

| 状態 | 保証 | 表示 |
| --- | --- | --- |
| `Unverified` | 暗号化、record完全性、提示leaf keyの所有。接続先identityは保証しない | `TLS UNVERIFIED` |
| `Pinned` | 上記に加え、URL hostnameへfirmwareで対応付けたSPKI SHA-256と一致 | `TLS PINNED` |
| `PublicCa` | 将来のWeb PKI chain、SAN、有効期間検証 | `SECURE HTTPS` |

### 未認証TLS

未認証profileはcertificate messageを無視しません。leaf certificateからpublic keyを取り出し、
TLS 1.3 `CertificateVerify`がhandshake transcriptに対する正しい署名であることを確認します。
FinishedとAEAD検証も通常どおり必須です。これにより通信相手が提示leaf keyを所有することは
確認できますが、そのkeyがURL hostnameの所有者のものかは確認できません。

active attackerは別のcertificateとkeyで接続を終端できるため、一般用途の安全なHTTPSとは
扱いません。未認証profileはこのプロジェクトの、credentialを送らず、取得物を一時的な
`/tmp`へ置くだけの初期用途に限定します。将来credential、永続保存、設定変更、firmware更新へ
利用範囲を広げる前に、`Pinned`または`PublicCa`を必須にします。

### SPKI pinning

pinはleaf certificate全体ではなく、DER `SubjectPublicKeyInfo`へSHA-256を適用した32 byte値と
します。生成物は正規化したhostnameとpinの対応を持ち、1 hostnameにつき現用keyと更新予定keyの
最大2件を許可します。certificate更新で同じkeyを使う場合はfirmware更新を不要にし、key rotationは
新旧pinを重ねたfirmwareを先に配布してから行います。

pin検索key、SNI、`Host:`は同じ`Url`から作ります。pin登録済みhostnameで不一致になった場合は
`tls-pin`で失敗し、`Unverified`へfallbackしません。未登録hostnameは利用者が明示した
未認証profileとして接続できます。pin tableの実行時変更、ネットワーク取得、TOFUは行いません。

IPv4 literalにもpinを登録できます。この場合もURLの正規化済みIP文字列とpinをfirmwareで
対応付けます。certificateのDNS SANやCNとの比較はpinningの認証根拠にしません。

### 将来の公開CA検証

Stage 9へ進む場合、公開CA root集合は生成元とversionを記録した生成物としてflashへ置きます。
PEM parserを実機へ載せず、build時に必要なDERまたはverifier用構造へ変換します。
生成scriptは入力の
hash、root件数、出力byte数を表示し、同じ入力から同じRust生成物を作ります。

Mozilla系root集合を候補としますが、Stage 9でverifierの対応algorithmとDROM量を
測り、対応できないrootを理由付きで除外します。「容量に入る先頭N件」のような順序依存の
切り詰めはしません。除外規則と残ったroot一覧を生成物に記録します。

root更新はfirmware更新です。runtime download、TOFU、接続先が送ったself-signed rootの
自動登録は行いません。公開CA fixture用rootは明示的なbuild featureでだけ組み込み、通常の
release imageに含まれていないことを生成物検査で確認します。

HTTPS URLがIPv4 literalの場合は、選定したverifierがX.509 `iPAddress` SANを検証できる
場合だけ対応します。DNS SANを文字列のIPと比較したりCNへfallbackしたりしません。
未対応なら`tls-name`として接続前に拒否します。この制約は`PublicCa`だけに適用し、
`Pinned`ではhostnameに対応付けたSPKI pinをidentityの根拠にします。

## 真性乱数とCSPRNG

現在`net::Stack`がTCPのrandom seedへ使うcycle counterとMAC addressは、TLSの秘密値には
使えません。ESP32-P4のhardware RNGは、継続して真性乱数を得るには内部entropy sourceの
条件があります。Stage 2でESP-IDFの`bootloader_random_enable`相当とRNG read間隔を
ECO2のregister実装として確認し、UARTの時間揺らぎやMAC addressをentropy量として数えません。

基本形は次とします。

1. SAR ADC entropy sourceを排他的に有効化する
2. hardware RNGから規定量（初期候補256 bit以上）を取得する
3. software CSPRNG／DRBGをseedまたはreseedする
4. entropy sourceを停止し、ADCを元の利用可能状態へ戻す
5. TLS handshakeの乱数要求はCSPRNGから供給する

enable中に別のADC利用を始めないよう、entropy sourceはRAII相当のguardで所有します。
途中失敗でもdisableされる経路を検査します。CSPRNG stateは複製せず、接続ごとに一意な
randomを供給し、rebootをまたぐseed保存はしません。TLS接続開始時にentropyが得られない
場合は`entropy`として失敗し、弱いseedへfallbackしません。

randomnessの統計試験は真性を証明しないため、Stage完了条件にはしません。確認するのは
entropy sourceのenable／disable、読み出し間隔、連続接続で同じClientHello random／key shareを
再利用しないこと、失敗時にTLSを開始しないことです。

## transport構成

HTTP parserを平文用とTLS用に複製しません。構成は次の形にします。

```text
smoltcp TCP socket
        |
        +-- Plain transport -------------------+
        |                                      |
        +-- TLS transaction -> plaintext ------+--> HTTP Transaction
                                                       |
                                                       +--> httpget
                                                       +--> browser Fetch
                                                       +--> hs / browsertest
```

`src/net/tls.rs`（案）はsocket handle、TLS engine、record buffer、送受信途中位置、timeout、
`Unverified`／`Pinned`／`PublicCa`の認証状態を所有します。`poll`の間だけ
`&mut Stack`と`&mut Rpc`を借り、現在のHTTP
`Transaction`と同じく所有しません。cancel後も`close(stack, rpc)`でsocketを必ず回収し、
close忘れはUARTへ出します。

`src/net/http.rs`は`Transport` enum（`Plain`／`Tls`）からplaintextを読み、requestを
plaintextとして書きます。status、header、chunked、本文終端、sink、上限は変更しません。
`HeadReady`を本文と別pollで返す契約も維持します。

TLS 1.3 recordの最大plaintextは16 KiBを基準にします。Stage 0の暫定上限は次です。

| 項目 | 暫定上限 |
| --- | ---: |
| TLS plaintext record | 16 KiB |
| certificate 1件 | 8 KiB |
| chain entry | 8件 |
| encoded certificate chain | 32 KiB |
| handshake途中保持 | 64 KiB |
| TLS接続が所有する動的メモリ | 128 KiB |

上限は実測と公開server fixtureで変更してよいものの、無制限にはしません。入力依存の確保は
`try_reserve`系を通し、失敗は`tls-limit`／`out-of-memory`として接続を閉じます。

1回のbrowser frameで処理する暗号化／復号byte数にはbudgetを置きます。ただし署名検証
1回がlibrary内の不可分処理になる場合があるため、Stage 0で最長poll時間を測ります。
単一pollが100 msを超える、display underrunを発生させる、またはC6受信stagingを回復不能に
する場合はその候補を中止し、hardware acceleratorまたは別libraryを別途判断します。

## HTTPSとブラウザの規則

- `Scheme::Https`の既定portは既存どおり443。明示portはSNIには含めず、`Host:`には
  非既定portとして含める。
- HTTPとHTTPSのrequest target、header制限は同じ。TLSだからCR／LFやuserinfoを
  許すことはない。
- HTTP→HTTPS redirect、HTTPS→HTTPS redirectは従来の上限5回の中で許可する。
- HTTPS→HTTP redirectは`https-downgrade`で拒否する。利用者が新しいHTTP URLを
  明示入力する操作まで禁止するものではない。
- `Pinned`／`PublicCa`から`Unverified`になるHTTPS redirectは`tls-auth-downgrade`で拒否する。
  利用者がredirect先URLを明示入力したり、page上のlinkとして選択したりする操作までは
  禁止せず、移動後は`TLS UNVERIFIED`と表示する。
- 読み込み中は直前に完成したpageを表示する既存契約を維持する。TLS失敗で途中の
  documentを表示しない。
- toolbarは接続の認証状態を隠さず表示する。未認証TLSは`TLS UNVERIFIED`を警告色、
  SPKI一致は`TLS PINNED`、将来の公開CA検証成功だけを`SECURE HTTPS`とする。
  接続開始だけ、暗号化だけ、pin不一致後には認証済み表示をしない。HTTP pageは従来どおり
  `INSECURE HTTP`を赤で表示する。
- HTTPS pageからHTTP linkを選択した場合は、URLを隠さず平文pageとして読み込み、完成後の
  toolbarを`INSECURE HTTP`へ変える。redirectによる自動downgradeとは区別する。

`httpget`は既存の`<host>[:port] [path]`を平文HTTPとして後方互換で残し、明示した
`http://`／`https://` URLも受け付ける形を候補とします。HTTPSを選ぶにはschemeを必須とし、
command名だけから暗黙にTLSへ変えません。`hs`、browser address欄、link、redirectは同じ
`Url`とtransport選択を使います。

## 失敗の分類

ブラウザfixtureが安定して判定できる1語のnameをTLSにも設けます。library固有enumは
この境界で正規化し、画面には短い説明、UARTにはTLS alertや検証段階を追加表示します。

| name | 意味 |
| --- | --- |
| `clock-unset` | RTCが読めない、VLF、STOP、または未設定 |
| `clock-invalid` | calendarは読めたがUTCとして成立しない |
| `entropy` | 真性乱数seedを用意できない |
| `tls-connect` | TCP接続またはClientHello送信前後の失敗 |
| `tls-version` | 共通のTLS version／cipher suiteがない |
| `tls-alert` | peerがTLS alertで終了した |
| `tls-cert` | certificate形式または`CertificateVerify`署名が不正 |
| `tls-pin-missing` | 認証必須の接続先にpinが登録されていない |
| `tls-pin` | leaf SPKIが登録pinと一致しない |
| `tls-ca` | Stage 9でchainを構築できない、または未信頼root |
| `tls-name` | Stage 9でSANが接続先DNS名またはIPと一致しない |
| `tls-time` | Stage 9で証明書がnot-yet-validまたはexpired |
| `tls-limit` | record、certificate、chain、handshakeの上限超過 |
| `tls-auth-downgrade` | 認証済みTLSから未認証TLSへの自動redirect |
| `https-downgrade` | HTTPSからHTTPへのredirect |

証明書エラーを通常のTCP timeoutに潰さず、逆にASN.1 parserの詳細を画面へ無制限に出しません。
失敗時もTLS socket、DNS query、HTTP transactionを回収し、次のnavigationが開始できることを
全fixtureで確認します。

## 段階分け

### Stage 0: 候補の成立確認

本体のHTTPへ接続する前に、候補crateを最小featureでtarget buildします。`embedded-tls`と
rustls unbuffered＋利用可能なproviderを比較し、上記の選定ゲートを実コードで判定します。
fixture serverのleaf public keyで`CertificateVerify`を検証する最小handshakeを作ります。
libraryの`NoVerify`でhandshake signatureを省略した接続は測定対象にも入れません。

測るもの:

- release IROM／DROM／内部stack／PSRAM peak
- ClientHelloからhandshake完了まで、最長の不可分処理、総poll回数
- C6 rx queued／delivered／dropped、link recovery、display underrun
- server handshakeを1〜数byteずつ分割した場合の前進
- cancel、TCP切断、TLS alert後のsocket回収
- RSA PSSとECDSA P-256の`CertificateVerify`対応可否

**完了条件**: 採用crate、version、feature、algorithm、signature verifier、SPKI pin形式、
buffer上限をこの文書へ記録し、signature不正とFinished不正を拒否できる。ゲートを満たす候補が
無ければStage 1以降へ進まず、C provider移植または暗号acceleratorを別計画にする。

### Stage 1: UTC壁時計と既定JST

**完了（2026-08-28実機確認）。** 変換は新しいworkspace member`time/`（`tab5-time`）に、
RTCの値の意味づけは`src/wall_clock.rs`に置きました。`local_now`が表示とFAT
timestamp用、`unix_time_utc`が証明書検証用で、後者だけが`VLF`／`STOP`を拒否します。
`rtc`はUTC行、JST行、`cert clock`行の3行を出します。host testは`mise run test`で
12件（境界の年、閏日、月末、日付をまたぐローカル変換、Unix秒の往復、weekdayの連続性）。

> 実機確認済み。`rtc set`はUTCを受け取るようになったため、既にJSTで設定して
> あった基板は一度UTCで設定し直しています。

calendar／Unix秒／固定offset変換をpure codeへ分離し、RTCの値をUTCと定義します。
`rtc set`／`rtc` helpと出力、filesystem timestampを更新します。将来の公開CA検証向けAPIは
VLF／STOPも含めて信用可否を返し、local表示APIとは型または関数名で区別します。

host test:

- 2000年と2099年の両端、leap year、各月末
- UTC±offsetで前日／翌日、前年／翌年へ移る場合
- Unix秒の往復
- JSTが常に`+540`分で、TLS Unix秒には足されないこと

**完了条件**: 実機RTCへUTCを書き、`rtc`がUTCとJST `+0900`を期待どおり表示する。
FAT entryがJSTを持ち、VLFを立てた状態では公開CA検証用の時刻取得だけが失敗する。
未認証TLSとSPKI pinningの可否はRTC状態で変わらない。

### Stage 2: 真性乱数とCSPRNG

**完了（2026-08-28実機確認）。** `src/entropy.rs`がESP-IDFの`bootloader_random_enable`
相当をECO2向けregister実装として持ち、`Source`がRAII guardです（`Drop`が必ず停止させる）。
`Csprng::from_hardware`が32 byteを取ってChaCha20へ入れます。ECO2は
`HAL_CONFIG(CHIP_SUPPORT_MIN_REV) >= 300`ではないので、ESP-IDFの`trng_ll`経路は
使いません。

アナログ設定バス（`I2C_ANA_MST`）は`src/psram.rs`にMPLL専用の実装があったので、
`src/regi2c.rs`へ切り出してblockを引数に取る形にしました。SAR ADCはblock 0x69・
`ANA_CONF2` bit 7、MSPI PLLはblock 0x63・bit 9です。`psram.rs`のboot経路は
`.iram.text.critical.psram`のまま共有実装を呼びます。

観測は`entropy`コマンドです（[CONSOLE_SHELL.md](CONSOLE_SHELL.md)）。

> `regi2c`の共有化はPSRAM初期化のboot経路に触れているため起動確認が必要でしたが、
> 2026-08-28に実機で起動を確認しました。

ESP32-P4 ECO2のentropy source enable／disable、RNG register readと規定間隔、CSPRNG adapterを
実装します。既存ADC利用とclock/reset registerへの影響を記録し、error pathを含むguardの
寿命を検査します。

**完了条件**: 100回のseed取得とTLS接続相当のreseedを繰り返し、enable／disableが必ず対に
なり、異なるClientHello random／key shareを生成する。entropy取得を故意に失敗させる
test hookではTLSが1 packetも送られない。

### Stage 3: 未認証TLS 1.3最小接続

**Stage 4と同時に実装済み（実機未確認）。** 分けなかったのは、Stage 0で決めた
駆動方式ではTLSエンジンとsocketが1つのfileの表と裏だからである。engineだけ作っても
socketが無ければ1 byteも動かず、確認できることが増えない。成果物は
`src/net/tls.rs`と`spki/`（`tab5-spki`）の2つ。

証明書から鍵を取り出すDER walkは`tab5-spki`に分け、host testを12件置いた
（`mise run test`）。OpenSSLが出したECDSA P-256／RSA-2048／v1証明書と答えが
一致すること、切り詰めた証明書の全prefix・長さフィールドの改変・tagの取り違え・
unused-bitが0でないBIT STRINGをすべて拒否することを検査する。

verifierは`LeafVerifier`で、`TlsVerifier`を自前で実装している。ライブラリの
`NoVerify`は使わない。`verify_certificate`がleafのSPKIを控えてpinを照合し、
`verify_signature`がRFC 8446 4.4.3のcontext stringを組んで署名を検証する。
`open`がcontextを値で受け取って捨ててしまうため、verifierの結論は
`Rc<Cell<Outcome>>`経由で外へ出す。これで`tls-pin`と`tls-cert`を区別できる。

> **既知の制限**: `TlsConfig`のsignature scheme listはライブラリ内部でprivateなため、
> ClientHelloからEd25519とECDSA P-384を落とせない。それらの証明書しか持たない
> serverは`tls-cert`で失敗する。互換性の制限であって検証の緩和ではない。
> 必要になればverifier側にp384／ed25519を足す（DROM／IROMと引き換え）。

fixture leafを使い、TLS engine、CSPRNG、signature verifierを結びます。SNIをURLのDNS名から
与え、leaf public keyによる`CertificateVerify`、Finished、AEADを検証してapplication dataを
1往復します。certificate chain、hostname、期間を検証していないことは認証状態へ反映します。

**完了条件**: self-signed、unknown root、wrong hostname、expired certificateでも
`Unverified`として接続できる一方、malformed certificate、bad `CertificateVerify`、bad Finished、
改変recordは失敗し、application dataへ到達しない。RTCがVLFでも結果が変わらない。

### Stage 4: smoltcp上の中断可能なTLS transaction

**Stage 3と同時に実装済み（実機未確認）。** `Transaction`は`net::http::Transaction`と
同じ所有規則で、socket handleだけを持ち、`poll`のあいだだけ`&mut Stack`と`&mut Rpc`を
借りる。`close`忘れは`Drop`がUARTへ出す。1 pollは「socket→cipher_rx」
「futureを1回poll」「cipher_tx→socket」の順で、復号予算は既定8 KiBである。

観測は`tls <host>[:port] [path]`コマンド。handshake時間、総時間、poll回数、
**1回のpollの最長時間**を出す。最後の値が中止条件（100 ms）の判定材料になる。

Stage 2完了時からのbaseline変化は次のとおり（2026-08-28）。

```text
IRAM=10336  DRAM-rodata=1364  DROM=589528  IROM=1089578  stack=189952
```

IROMが826,062から1,089,578へ+263,516 byte増えた。内訳はTLSエンジン、AES-GCM、
P-256、RSA、SHA-2である。`ROM_TEXT`は0x370000（3,604,480 byte）なので余裕があり、
`factory`パーティション（0x3f0000）にも収まる。

#### 実機実測（2026-08-28）

| 接続先 | 署名方式 | handshake | 総時間 | 1 pollの最長 | 平文 |
| --- | --- | ---: | ---: | ---: | ---: |
| `www.google.com` | ECDSA P-256 | 122 ms | 427 ms | 70.6 ms | 89,824 B |
| `www.rfc-editor.org` | RSA-PSS | 127 ms | 550 ms | 69.4 ms | 179,477 B |

どちらも`TLS UNVERIFIED`でHTTP status行まで到達した。**1 pollの最長は約70 ms**で、
中止条件（100 ms）には当たらない。ただし57.3 Hzでは約4 frame分の停止であり、
Stage 6でブラウザへ入れるときはhandshake中の見え方に効く。hardware acceleratorは
中止条件に当たった場合だけ検討する取り決めなので、この計画では扱わない。

#### 実機で見つかった不具合（修正済み）

`www.google.com`は本文を返し切ったあと`close_notify`を送らずTCPを閉じるため、
正常終了が`tls-connect`として報告されていた。embedded-tlsはtransportの0バイト読みを
`IoError`にするので、「serverが終わった」と「接続が壊れた」を区別できない。
`Io::read`が終端に達した事実をqueueへ記録し、handshake完了後の受信中に限って
終端として扱うよう修正した。handshake中の0バイト読みは従来どおり失敗である。
`close_notify`なしで終わったことは`Stats::closed_without_notify`として残し、
本文が完結しているかはHTTP側のframingが判断する（Stage 5）。

Cloudflare（`www.rfc-editor.org`）は`close_notify`を送るため、この経路では
最初から成功していた。片方のserverだけで確認していたら見逃していた不具合である。

`src/net/tls.rs`を追加し、smoltcp socketへrecordを増分送受信します。1回のpollで戻ること、
timeout、link loss、cancel、closeを現在のHTTP transactionと同じ所有規則へ合わせます。

**完了条件**: frame loopからhandshake／送信／受信／closeを駆動でき、どの状態でEscapeしても
次frameまでにUIへ制御が戻る。途中切断後も次の接続が成功し、socket handleが増え続けない。

#### Stage 3-4の実機確認項目

1. `tls <公開HTTPS host>`が`TLS UNVERIFIED`とHTTPのstatus行を出す。ECDSA P-256の
   serverとRSA-PSSのserverを1つずつ
2. **1回のpollの最長時間**。100 msを超える場合は中止条件に当たるので、
   hardware acceleratorか別libraryを別途判断する
3. handshake時間と、C6のrx queued／delivered／dropped、display underrunの有無
4. 存在しないhost、平文HTTPのportへの接続、途中でWi-Fiを切る、の3つで
   failure nameが期待どおりになり、socketが回収される
5. `entropy fail on`のあと`tls`を実行して、`entropy`で失敗し1 packetも出ないこと
6. 100回程度の連続実行でPSRAM使用量とsocket数がbaselineへ戻ること

### Stage 5: HTTPの共通transport化

**完了（2026-08-28実機確認）。** `src/net/transport.rs`を追加し、`net::http`から
socket操作を全部取り除いた。`Transaction`は`Transport`を持ち、`Security`
（`Plain`／`Tls`）は`start`の引数として上から1回だけ渡す。status行、ヘッダ、
chunkデコーダ、本文の終端判定、`HeadReady`を本文と別pollで返す契約はすべて
共通のままである。

`tls`コマンドは自前のHTTP要求を組むのをやめ、`net::http::Transaction`を
TLS transportの上で回すよう書き換えた。これがStage 5の「同じHTTP層がTLS上でも
動く」ことの実証になっている。

失敗名は変えていない。`transport::Error`は平文側について
`link-lost`／`not-connected`／`local`という既存の名前をそのまま出し、
`http::Error::Transport`がそれを透過させる。TLS側の失敗は`tls::Error`の
細かい名前（`tls-cert`、`tls-pin`など）のまま上がる。

> 途中で1つ回帰を入れて直した。`stack.connect_tcp`が失敗した場合の名前が
> `local`から`not-connected`へ変わっていた。socketをsetへ入れた直後の失敗は
> 相手が拒否したのではなくこちら側の失敗なので、`Plain`に`local_failure`を
> 持たせて`local`のままにした。

HTTP `Transaction`のTCP直接操作を`Plain`／`Tls` transportへ分離します。HTTP parser、
body decoder、header、redirect判断は複製しません。同期`get`も同じtransactionを回します。

**完了条件**: 既存HTTP fixtureが結果・受信byte数・failure nameを変えず完走し、同じfixtureを
HTTPS化したserverでもHTTP層の期待結果が一致する。512 KiB保存のCRCが両方で一致する。

#### Stage 5の実機確認項目

1. `browsertest`の全fixtureが従来どおり完走し、failure nameが変わらないこと
2. `httpget`の保存／404／chunked／truncatedが従来どおり
3. 512 KiB downloadのCRCが従来と一致すること
4. `tls <host>`がstatus行・`Content-Length`／chunkedの別・本文byte数を出すこと
5. ブラウザの巡回に回帰がないこと

### Stage 6: browser、未認証表示、redirect

**完了（2026-08-28実機確認）。**

- `Scheme::is_fetchable`を廃止し、`is_cleartext`に置き換えた。前者は
  「httpだけ取得できる」の意味で、両方取得できるようになると常にtrueを返す
  同語反復になる。置き換えたことで、httpsを拒否していた4箇所すべてを
  見直すことが強制された。
- `app::fetch`がURLのschemeからtransportを選ぶ。SNI名・`Host:`・request
  target・接続先アドレスはすべて同じ`Url`から作る。
- redirectの降格を拒否する。`https`→`http`は`https-downgrade`、pin済み
  接続からpinの無いhostへは`tls-auth-downgrade`。`http`→`https`は許可。
  利用者がアドレスを打つ／リンクを選ぶ操作は禁じない。
- toolbarのバッジを接続状態から描く。`INSECURE HTTP`と`TLS UNVERIFIED`は
  同じ赤、`TLS PINNED`だけ緑、handshake前は`CONNECTING`。バッジ幅は最長の
  文字列に固定してアドレス欄がずれないようにした。読み込み中のバッジは
  読み込み中の接続の状態であり、画面に残っている前ページのものではない。
- `httpget`が明示スキームのURLを受け付ける。bare hostは従来どおり平文。
- `hs`／`bt`／`browsertest`も`security_for(url)`でschemeからtransportを選ぶ。
  ここを直さないと`hs https://…`が443へ平文接続する降格になっていた。

#### fixtureの置き換え

`/redirect/https`は`error:https`（HTTPSは未対応）を期待していたが、
`http`→`https` redirectは許可になったのでこの期待値は意味を失う。fixture
serverはTLSを話さないので、逆向きの`https-downgrade`はここでは作れない。
そこでこのfixtureは「upgradeを拒否せず試みる」ことの確認に変え、期待値を
`error:tls-connect`にした。redirect先はfixture server自身のアドレス
（requestの`Host`から作る）で、ClientHelloを受け取ったserverが即座に切断する
ようにした。到達不能アドレスだと毎回5秒のtimeoutを払うことになるため。

`https-downgrade`と`tls-auth-downgrade`の実機確認はStage 8のTLS fixture
serverが要る。

HTTPS URLをfetch可能にし、toolbar、error page、history、relative link、redirectへ統合します。
表示中pageの状態は最終hopのschemeとTLS認証状態から決めます。pinning導入前のHTTPSは常に
`TLS UNVERIFIED`であり、画面遷移中だけ認証済み表示へ変えません。

**完了条件**: HTTP→HTTPSとHTTPS→HTTPSは成功し、HTTPS→HTTP redirectは拒否される。
HTTPS page内の明示HTTP linkは選択時にURLが見え、取得後は`INSECURE HTTP`になる。
未認証HTTPSは全画面とtoolbarで警告状態を確認できる。TLS失敗時は直前の完成pageを壊さず、
error pageからHome／Backが使える。

#### Stage 6の実機確認項目

1. `browser https://www.google.com/`でページが出て、toolbarが赤い
   `TLS UNVERIFIED`になること。読み込み中は`CONNECTING`
2. `browser`で平文fixtureを開き`INSECURE HTTP`のままであること。バッジが
   変わってもアドレス欄が横にずれないこと
3. `bt http://<pc>:8080` の全fixture。特に`/redirect/https`が
   `tls-connect`になること（従来は`https`）
4. `hs https://www.google.com/`が取得でき、`hs`のsocket／heap回収が従来どおり
5. `httpget https://www.google.com/` と、従来形`httpget <host> /path`の両方
6. HTTPSページからHTTPリンクを辿ると、取得後に`INSECURE HTTP`へ変わること
7. 読み込み中のEscapeがHTTPSでも効き、直前のページが壊れないこと

### Stage 7: SPKI pinning

**完了（2026-08-28実機確認）。**

- `tools/pins/generate.py`が`pins.txt`／`fixture_pins.txt`から
  `src/net/pins/generated.rs`を作る。入力のSHA-256、host数、pin数、出力byte数を
  毎回表示し、同じ入力からは同じバイト列になる。`--check`で生成物の鮮度、
  `--from-cert`でpinの算出、`--check-release`でELFにfixture pinが無いことを検査。
- 1 hostにつき最大2 pinを生成器が強制する。3つ目は「攻撃者が持っているかも
  しれない鍵が1つ増えるだけ」なので拒否する。
- `src/net/pins.rs`がhostname（URL中の文字列そのもの、SNIと同じ）で検索する。
  `policy_for`が`PinPolicy`を返し、TLSを開く全経路（browser、`hs`、`bt`、
  `httpget`、`tls`）がこれを使う。
- `fetch::host_is_pinned`が表を見るようになったので、`Pinned`から未登録hostへの
  自動redirectが実際に`tls-auth-downgrade`で止まる。
- `tools/pins/pins.txt`は**空**。通常buildで`TLS PINNED`になる接続先は無い。
- 試験用pinは`tls-fixture-pins` featureにだけ入る。秘密鍵は`tools/tls/`。
- `browser_fixture_server.py`に`--tls-port`（既定8443）と`--tls-key-name`
  （`current`／`next`／`other`）を足した。同じfixtureをTLSで出すので、
  平文とHTTPSで結果を突き合わせられる。

> **fixture pinのアドレス**: 基板は`tls-fixture.invalid`を解決できないので、
> `fixture_pins.txt`にはfixture serverのLANアドレス（記録時点で
> `192.168.0.159`）にも同じpinを入れてある。pinはURL中の文字列で引き、
> 証明書のSAN／CNは見ないので、これで成立する。機械が変わったら書き換えて
> `generate.py`を再実行する。

build時にhostnameとSHA-256 SPKI pinの対応をRust生成物へ変換し、TLS transactionへ照合を
追加します。1 hostnameあたり最大2 pinを持てるようにし、fixtureでkey rotationを再現します。
pin必須指定と、pin未登録hostnameを未認証で開く通常動作をAPI上で区別します。

**完了条件**: 現用pinと更新予定pinは`Pinned`で成功し、別key、1 bit改変、別hostnameのpinは
`tls-pin`で失敗する。pin不一致は`Unverified`へfallbackせず、application dataをHTTPへ渡さない。
`Pinned`から未登録hostnameへの自動redirectは拒否し、明示linkは警告表示へ遷移できる。
通常releaseにfixture pinが無く、pin生成入力のhash、件数、出力byte数を再現できる。

#### Stage 7の実機確認項目

`tools/pins/fixture_pins.txt`のアドレス行を自分の機械に合わせてから
`tools/pins/generate.py`を実行し、`cargo run --release --features tls-fixture-pins`
で書き込む。PC側は`python3 tools/browser_fixture_server.py --tls-key-name <名前>`。

1. `--tls-key-name current` → `browser https://<addr>:8443/` が **`TLS PINNED`**（緑）
2. `--tls-key-name next` → 同じく`TLS PINNED`（鍵rotationの受入）
3. `--tls-key-name other` → **`tls-pin`で失敗**し、`TLS UNVERIFIED`にはならない。
   本文が1 byteも表示されないこと
4. `tls <addr>:8443` でも 1〜3 と同じ結果になること
5. **通常build**（featureなし）で`https://<addr>:8443/`が`TLS UNVERIFIED`に
   なること。pinを持たないbuildなので未認証で開けるのが正しい
6. `tools/pins/generate.py --check-release <通常buildのELF>`が通り、
   fixture pins付きELFでは失敗すること

### Stage 8: 診断、実機受入、文書化

`hs`／`browsertest`のfixture manifestをTLSへ拡張します。TLS fixture serverはTLS 1.3、
signature／Finished／record改変、SPKI pin一致／不一致、record分割、途中close、alert、
oversize certificate messageを再現します。fixture pinは専用featureだけに入れ、通常releaseには
入れません。

**完了条件**: 全fixtureを複数round完走し、期待するfailure nameと一致する。各失敗後に
socket／DNS slot／PSRAM使用量がbaselineへ戻り、通常release imageにfixture pinが無い。
公開serverは少なくともRSA PSSとECDSA P-256で`CertificateVerify`する複数endpointを
未認証状態で確認します。
同じ512 KiBをHTTP／HTTPSで取得してCRC、速度、peak memoryを比較し、ブラウザ操作中の
cancel、Wi-Fi切断・再接続、100回の短い接続を試します。

完了時に更新する現状文書（すべて反映済み）:

- [`NETWORK.md`](NETWORK.md): TLS transport、暗号方式、認証状態、pin、上限、error、実測
- [`BROWSER.md`](BROWSER.md): 未認証TLS／pin済みTLSの表示、redirect規則、failure name
- [`RTC.md`](RTC.md): RTCはUTC、`rtc set`／表示、VLFとTLSの関係、既定JST
- [`FILESYSTEM.md`](FILESYSTEM.md): FAT timestampはUTCから既定JSTへ変換
- [`FILE_LAYOUT.md`](FILE_LAYOUT.md): TLS、壁時計、entropy、生成pinの責務（Stage 9ではrootも追加）
- [`DIAGNOSTICS.md`](DIAGNOSTICS.md): 正常handshakeと主な失敗log
- `DESIGN.md`: TLSなしという制約を実装結果へ更新

README.mdは人間管理なので、現在の依頼では変更しません。実装後にREADME.mdと不一致が
生じても編集せず、最終報告で具体的な箇所を示します。

### Stage 9: 条件付きの公開CA検証

任意の公開Webを本人確認付きで開く、credential送信、永続保存、設定変更、firmware更新、
または表示内容を信頼する用途が追加された場合だけ開始します。Mozilla系root、複数候補からの
path building、intermediate、Basic Constraints、Key Usage、critical extension、SAN、
有効期間をすべて検証し、成功時だけ`PublicCa`とします。

**開始条件**: 具体的な保護対象と脅威、root更新方法、時刻設定方法、必要な公開server互換性を
別途決め、library／verifierと追加工数を再見積もりする。初期版の完了やStage 8の受入を
Stage 9の未着手で妨げない。

## 受入試験一覧

### 時計

- UTC設定とJST `+0900`表示
- VLF、STOP、不正BCD、I2C failureでも未認証TLSとSPKI pinningの結果が変わらない
- 公開CA検証用時刻APIだけがVLF、STOP、不正BCD、I2C failureを拒否する
- UTCの日付境界とJSTの日付境界をまたぐ変換

### TLS暗号検証

- self-signed、unknown root、SAN不一致、expiredでも`Unverified`として接続
- `CertificateVerify`正常／署名改変／別leaf key
- Finished正常／改変、application recordのAEAD tag改変
- malformed leaf、空certificate message、未対応signature algorithm
- chain順序、最大件数ちょうど、1件超過、1 certificateの長さ超過
- 対応するRSA PSS／ECDSA P-256と、未対応algorithmの明示失敗

### SPKI pinning

- hostnameごとの現用pin／更新予定pin
- 別key、1 bit改変、別hostnameのpinを拒否
- pin不一致から`Unverified`へfallbackしない
- pin必須なのに未登録の場合は`tls-pin-missing`
- certificate更新でSPKI不変の場合は同じpinで成功
- 通常releaseにfixture pinを含めない

Stage 9開始後は、上記に正しいleaf＋intermediate＋root、unknown root、SAN、wildcard、
CN-only、`notBefore`／`notAfter`境界、Basic Constraints、Key Usage、critical extensionを
追加します。

### transport

- handshake byteを1、2、3、5、7、13、MTU近辺で分割
- application recordとHTTP header／chunk境界が一致しない場合
- ClientHello中、certificate中、HTTP head中、body中、close中の切断／cancel
- TLS alert、TCP RST、無応答timeout、Wi-Fi link loss
- HTTP→HTTPS、HTTPS→HTTPS、HTTPS→HTTP redirect
- `Pinned`→`Pinned`、`Pinned`→`Unverified` redirect
- 512 KiB downloadのCRCと100接続soak

### 回帰

- `mise run test`
- `cargo build --release`
- `tools/check_elf_layout.py`
- ESP imageが2本のXIP segmentを維持
- HTTP版`browsertest`全fixture
- `httpget`の保存／404／chunked／truncated
- display underrun、USB keyboard／touch／mouse、Escape cancel

## Stageを越えて固定する中止条件

- server `CertificateVerify`またはFinishedを検証せずに接続する
- 未認証TLSを`SECURE`、鍵アイコン、検証済みHTTPSとして表示する
- pin不一致／pin必須時の未登録を`Unverified`へfallbackする
- 認証済みTLSから未認証TLSへ自動redirectする
- networkから得たcertificate／keyを自動登録してTOFUする
- Stage 9でRTC異常時に証明書時刻検証を省略する
- hardware entropyが得られないとcycle counterやMACだけへfallbackする
- HTTPSからHTTPへ自動downgradeする
- TLS入力のrecord／certificate／chain／allocationが無制限になる
- browserの1回のstepが100 msを超える、display underrunを起こす、またはC6受信経路を
  回復不能にする
- cancel／errorのたびにsocket、DNS query、PSRAM allocationが漏れる
- HTTPとHTTPSで別のHTTP parser／redirect実装を持つ
- 通常releaseがfixture用pinまたはprivate CAを信頼する
- release内部stackが128 KiBを下回る、またはXIP segmentが2本でなくなる

中止条件に当たったStageは完了扱いにせず、この文書へ候補、実測、失敗点を記録して
設計を戻します。

## 見積もり

Stage 0〜6の未認証TLS、RTCのUTC／JST分離、browser／CLI統合は**10〜15人日**、
Stage 7のSPKI pinningとStage 8の受入を加えた推奨初期範囲は合計**12〜18人日**とします。
Stage 0を2〜3人日で先に行い、採用library、`CertificateVerify`対応、最長poll時間が確定した
時点で残りを再見積もりします。

Stage 9の公開CA検証は初期範囲に含めません。開始時の追加目安は**5〜9人日**ですが、
採用libraryに安全な複数root／intermediate path verifierが無ければ、別libraryまたは
C provider移植となるためこの目安を超えます。TLS 1.2、広いalgorithm互換性、CA更新機構、
SNTP、timezone設定／保存も含みません。

## 参照資料

- [embedded-tls](https://docs.rs/embedded-tls/latest/embedded_tls/)
- [rustls unbuffered API](https://docs.rs/rustls/latest/rustls/unbuffered/)
- [ESP32-P4 Random Number Generation](https://docs.espressif.com/projects/esp-idf/en/stable/esp32p4/api-reference/system/random.html)
- [RFC 9846: TLS 1.3](https://www.rfc-editor.org/rfc/rfc9846)
- [RFC 5280: X.509 PKI certificate and CRL profile](https://www.rfc-editor.org/rfc/rfc5280)
- [RFC 9525: Service identity](https://www.rfc-editor.org/rfc/rfc9525)
