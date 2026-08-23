# 起動時USB MSC認識マージンの計測計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画です。現在の実装仕様は現状文書
> （[`STORAGE.md`](STORAGE.md)、[`USB.md`](USB.md)）とコードを優先してください。

## 状態: **Stage 1〜3完了（実機計測済み）／Stage 4は別計画**

| Stage | 状態 |
| --- | --- |
| 1 計測機構（起動ログ・`usbmargin`・connect待ちのruntime化） | **実機確認済み** |
| 2 実機計測（デバイス別・直結／ハブ別の分布を取る） | **実機計測済み**（全条件、下記の記録） |
| 3 計測値から起動時の待ち時間を決め、定数を確定する | **確定**（connect 1,000 ms／ready 4,000 ms） |
| 3.5 メディア無しの即断（REQUEST SENSE ASC `0x3A`） | **実機確認済み**（1 ms／1試行で判定） |
| 4 ファイルシステム層の起動時メディア選択へ反映 | 未着手（別計画） |

## 目的

ファイルシステム実装では、**起動時にUSB MSCが挿さっていればそれを最優先の
ファイルシステムにする**予定です。これは起動シーケンスが「USBメモリが出て
くるのを待つ」ことを意味し、待ち時間が短すぎるとUSBを挿しているのにSDや
Flashで起動してしまい、長すぎるとUSBを挿していない起動が毎回その分遅く
なります。**この値は推測ではなく実測から決めます。**

計測する量は1つです。**USB-Aの5V（VBUS）が入ってから、LBA 0を読める
（＝ファイルシステムがスーパーブロックを読める）状態になるまでの時間。**
その内訳も同時に取り、どの段階がデバイス依存で伸びるのかを分けます。

## 何がデバイス依存で、何が固定か

現在の直結経路の内訳です。固定分は`hcd.rs`と`hub.rs`の定数から出せます
（ESP-IDF `hcd_dwc.c`のKconfig既定に合わせてあります）。

| 区間 | 実装 | 長さ |
| --- | --- | --- |
| VBUS on → コア初期化開始 | `probe_port`の`delay_ms(50)` | 固定 50 ms |
| UTMI／コアsoft reset・FIFO設定 | `reset_utmi_and_core`ほか | 固定 30 ms＋数ms |
| **connect待ち（D+/D−のpull-up検出）** | `wait_for_connect` | **デバイス依存**（上限は可変、下記） |
| debounce | `DEBOUNCE_DELAY_MS` | 固定 250 ms |
| port reset | `RESET_HOLD_MS`＋`RESET_RECOVERY_MS` | 固定 60 ms |
| 標準列挙（chapter 9） | `protocol::enumerate_device` | **デバイス依存**（SET_ADDRESS後の10 ms固定を含む） |
| BOTのclass driver接続 | `msc::UsbMassStorage::attach` | ほぼ制御転送分だけ |
| **TEST UNIT READYがreadyを返すまで** | `msc::measure_ready_and_first_read` | **デバイス依存**。実測では100 ms間隔のpollではなく1本目のコマンドが返らない（Stage 2） |
| READ CAPACITY(10)＋READ(10) LBA 0 | 同上 | ほぼ転送分だけ |

したがって**下限は約80 ms＋connect時間＋310 ms**で、その上に列挙とSCSIの
ready待ちが乗ります。ハブ経由ではさらに、ポート給電（`PORT_POWER_INTERVAL_MS`
20 ms×ポート数＋hub descriptorの`bPwrOn2PwrGood`×2 ms＋余裕50 ms）、
ポートごとのdebounce 250 ms、ポートreset＋復帰30 msが加算されます。
**ハブ配下は原理的に直結より遅く、ポート番号にも依存します。**

connect待ちの上限は`hcd::set_connect_wait_ms`でruntimeに切り替えます。

- 定常時 500 ms（`DEFAULT_CONNECT_WAIT_MS`）。フレームループが空ポートを
  定期再probeする間ずっとブロックするので、ここは長くできません。
- 起動時の初回スキャンだけ1,000 ms（`input.rs`の`BOOT_CONNECT_WAIT_MS`）。
  Stage 2の実測（最悪246 ms）から決めた値です。
- `usbmargin`実行中は5,000 ms。計測値が上限で切り詰められて「デバイスが
  無い」と誤って記録されるのを防ぐためで、常用の値ではありません。

## Stage 1: 計測機構（実機確認済み）

### 起動ログ

`InputManager::new`の初回スキャンをそのまま計測し、UARTへ出します
（[`DIAGNOSTICS.md`](DIAGNOSTICS.md)）。値は10進のミリ秒です。`ms=`の起点は
`connect`〜`scan total`がVBUS on、`unit ready`以降が最初のSCSIコマンドです。

```text
USB BOOT: scan began at uptime ms=NNN
USB BOOT: root connect ms=NNN
USB BOOT: port enabled ms=NNN
USB BOOT: root enumerated ms=NNN
USB BOOT: scan total ms=NNN
USB BOOT: mass storage attached ms=NNN
USB BOOT: unit ready ms=NNN
USB BOOT: unit ready attempts=N
USB BOOT: read capacity ms=NNN
USB BOOT: first LBA 0 read ms=NNN
USB BOOT: usable from VBUS on, total ms=NNN
```

最後の1行が求める値です。SCSIコマンドはファイルシステム層が起動時に出す
ものと同じ3つ（TEST UNIT READY／READ CAPACITY(10)／READ(10) LBA 0）なので、
この計測のために増える起動コストはほぼありません。読み出しだけで、
デバイスへは何も書きません。

### `usbmargin [rounds]`

電源投入からの起動を何度も繰り返さずに分布を取るためのコマンドです。
1ラウンドごとに、レジストリを破棄→VBUS off 1秒→`rescan`（この中でVBUSが
入り、そこから計測が始まる）→SCSIのready待ちとLBA 0読み出し、を行います。
既定5ラウンド、最大20。read-onlyです。

VBUSを実際に切るので、**電源投入直後の状態（デバイス側のコントローラが
コールドスタートする）を再現できます。**バスresetだけの`usbrescan`とは
別物で、そちらはデバイスが給電済みのまま再列挙するだけです。

出力（コンソールとUARTの両方）:

```text
boot scan: connect=NNN enum=NNN msc=NNN total=NNNms
1: con=NNN ena=NNN enum=NNN msc=NNN rdy=NNN/N lba0=NNN total=NNN
...
usbmargin: usable 5/5 total min=NNN max=NNNms
worst connect=NNNms scsi=NNNms; 1.5x boot budget=NNNms
```

最終行の`1.5x boot budget`は**目安の表示であって決定ではありません。**
1台1回の実行から採用してよい値ではなく、Stage 3では全デバイス分を見てから
決めました。

## Stage 2: 実機計測（実施済み）

計測した条件（デバイスを増やして取り直すときも同じ手順で足ります）:

1. 手持ちで最も遅いUSBメモリを直結して`usbmargin`。
2. 同じ個体をハブのポート1と最終ポートで`usbmargin 10`。
3. 同じ構成で電源からの**冷起動を5回**行い、`USB BOOT:`の最も遅い回を採る。
4. ハブだけを挿してMSC無しの冷起動（USBストレージを使わない起動のコスト）。
5. カードリーダーのメディア有り／無し
   （**「待てば来るのか、待っても来ないのか」の区別**）。

6. **USB-Aに何も挿さない**冷起動（connect待ちを使い切る唯一の経路）。

4と6は紛らわしいので注意します。**4はハブが挿さっていてUSBメモリだけ無い**
状態で、connectは118 msで返り列挙まで進みます。**6は本当に何も繋がっていない**
状態で、`wait_for_connect`が上限まで待ってから
`USB: no device detected on USB-A within timeout`を出します。
起動コストが違うので、混同すると「USB無しの起動は速い」と誤解します。

### 計測結果（実機）

`connect`〜`scan total`はVBUS onが起点、`ready`以降は最初のSCSIコマンドが
起点、`total`はVBUS onからLBA 0が読めるまでです。単位はすべてミリ秒。

| デバイス | 経路 | 方法 | connect | scan total | ready／試行 | total |
| --- | --- | --- | --- | --- | --- | --- |
| 手持ちで最も遅いUSBメモリ | 直結 | `usbmargin` 5回 | 246 | — | — | 3,235〜3,248 |
| 同上 | hub port 1 | `usbmargin` 10回 | 96 | — | — | 3,637〜3,660 |
| 同上 | hub port 4 | `usbmargin` 10回 | 96 | — | — | 3,643〜3,668 |
| 同上 | 直結 | 冷起動5回の最遅 | 246 | 705 | 2,553／1 | **3,261** |
| 同上 | hub port 1 | 冷起動5回の最遅 | 118 | 1,126 | 2,556／1 | **3,685** |
| 同上 | hub port 4 | 冷起動5回の最遅 | 118 | 1,126 | 2,561／1 | **3,690** |
| カードリーダー（メディア有） | 直結 | 冷起動 | 118 | 979 | 768／2 | 1,748 |
| カードリーダー（メディア無） | 直結 | 冷起動 | 120 | 981 | 10,047／91 | 読めず |
| 同上（Stage 3.5適用後） | 直結 | 冷起動 | — | 981 | **1／1** | 読めず（即断） |
| ハブのみ（MSC無し） | 直結 | 冷起動 | 118 | 672 | — | — |
| **何も挿さない** | — | 冷起動 | 上限まで待つ | **1,081** | — | — |

分かったことは5つです。

1. **connectは速い。** 最悪246 ms、そのうち約80 msは`probe_port`自身の固定
   遅延なので、デバイス依存分は170 ms程度。**従来の500 ms上限でも足りて
   いました**。暫定で置いた3,000 msは過剰です。
2. **支配項はSCSIのready待ちで、しかもpollingではありません。**
   `unit ready attempts=1`のまま2,553 ms経過している、つまり**最初の
   TEST UNIT READY 1本が2.5秒返ってこない**という意味です。`READY_POLL_
   INTERVAL_MS`（100 ms）を触っても何も変わらず、効いているのはBOT転送側の
   タイムアウト（約5秒）のほうです。
3. **ハブ経由は約430 ms遅い**（ポート給電＋ポートdebounce）。ポート1と
   ポート4の差は5 ms以下で、ポート番号は実質効きません。
4. **冷起動と`usbmargin`の差は25 ms以内。** VBUSの再投入は冷起動の代用として
   妥当で、以後の再計測は`usbmargin`で足ります。
5. **メディア無しカードリーダーは待っても来ません。** 約110 msごとに
   not readyを返し続け（10秒で91回）、途中でpacket error＋BOT Reset Recovery
   も1度起きています。これは時間で打ち切るしかない相手です。
6. **何も挿さない起動は1,081 ms。** 固定分約80 ms＋`BOOT_CONNECT_WAIT_MS`
   1,000 msという計算どおりで、この経路には測定誤差もデバイス差もありません。
   **USBストレージを使わない人が毎回払うコストがこの1,081 msです。**
   縮めたければ`BOOT_CONNECT_WAIT_MS`を下げるしかなく、その分だけ遅い
   デバイスを取りこぼす確率と引き換えになります（実測最悪246 msなので、
   500 msまでなら約2倍の余裕を保ったまま起動を0.5秒短縮できます）。
   なおこの所要時間は当初ログに出ておらず、`scan total ms`を追加して
   取得しました。

## Stage 3: マージンの決定（確定）

| 定数 | 確定値 | 根拠 |
| --- | --- | --- |
| `BOOT_CONNECT_WAIT_MS`（`input.rs`） | **1,000 ms** | 実測最悪246 msの約4倍。**デバイスが無い起動だけがこの全額を払う**ので、無挿入時の起動コストは約1.1秒に収まる |
| `BOOT_MASS_STORAGE_READY_MS`（同上） | **4,000 ms** | 実測最悪2,564 msの約1.55倍。メディア無しリーダーを諦めるまでの上限でもある |

これで**実測された最も遅い構成（ハブ経由のUSBメモリ）が3,690 msで通り、
余裕は約1.5倍**です。反対側のコスト、つまり**USB-Aに何も挿さない起動の
待ち時間は実測1,081 ms**で、`BOOT_CONNECT_WAIT_MS`とほぼ等しくなります
（この経路だけが上限を使い切るため）。1,000 msはこの2つのバランスを
取った値です。

ただし2つ、値の意味を誤解しないための注意があります。

- **ready budgetはコマンドの「間」でしか効きません。** 上の観測2のとおり
  待ち時間は単一のBOTコマンドの中にあるので、4,000 msを設定しても、1本の
  コマンドがBOT側のタイムアウト（約5秒）まで粘れば起動はそこまで待ちます。
  **起動の実効的な最悪値は「scan最大1.15秒＋約5秒」で、budgetでは切れません。**
  ここを本当に縛るなら、起動時のプローブだけBOTの転送タイムアウトを短くする
  必要があり、それはStage 4で判断します。
- **時間ではなくsense keyで諦めるほうが正確です。** これはStage 3.5として
  実装済みです（下記）。4,000 msの予算は、それでも判定できない相手のための
  安全網という位置づけになりました。

ハブ配下は起動時スキャンの対象に含めたままにします。コストは+430 msで、
実測でも直結と同じく確実に読めているためです。

## Stage 3.5: メディア無しの即断（実機確認済み）

TEST UNIT READYが「not ready」を返したとき、`msc::UsbMassStorage`は続けて
REQUEST SENSEを出し、**sense key 2かつASC `0x3A`（MEDIUM NOT PRESENT）なら
予算を使い切らずにその場で諦めます**。空のカードリーダーは挿さっている間
ずっとこの答えを返すので、待っても状況は変わらないからです。「起動中」
（ASC `0x04`）やその他の理由は従来どおり予算まで待ちます。REQUEST SENSE自体が
失敗した場合は「理由が読めない」だけなので、待ち続ける側に倒します。

結果は`ReadyTiming::outcome`（`ReadyOutcome`）で区別できます。
`NoMedium`は**デバイスは正常でメディアだけが無い**状態で、起動は次のメディアへ
進むべき場合、`NotReady`は予算切れで**もっと待てば成功したかもしれない**場合です。
起動ログとコマンド表示も分かれます。

| 状況 | 起動ログ | `usbmargin` |
| --- | --- | --- |
| 読めた | `USB BOOT: usable from VBUS on, total ms=` | `total=NNN` |
| メディア無し | `USB BOOT: mass storage has no medium, not usable` | `NO MEDIUM` |
| 予算切れ・転送失敗 | `USB BOOT: mass storage did not become readable` | `UNREADABLE` |

実機結果:

```text
USB BOOT: unit ready ms=1
USB BOOT: unit ready attempts=1
USB BOOT: mass storage has no medium, not usable
```

**1回のTEST UNIT READYとその直後のREQUEST SENSEだけ、1 msで判定できました。**
10,047 ms／91試行だったものが、待ち時間ゼロになっています。メディア無し
カードリーダーの起動コストは、これで実質scan分（981 ms）だけです。
メディア有りの同じリーダーとUSBメモリが従来どおり読めることも確認済みで、
`attempts=2`のリーダーを誤って`NoMedium`と判定することはありませんでした。

## Stage 4: ファイルシステム層への反映（別計画）

起動時のメディア選択と、選べなかったときのフォールバックはFAT実装の計画側で
扱います。この計画が確定させたのはそこへ渡す**時間の予算**だけです。
申し送り事項:

- 起動時の優先順は USB MSC → microSD →（将来の）内蔵Flash。
- USB MSCが予算内にreadyにならなければ諦めてSDへ。**諦めた事実は必ずログに
  残す**（黙って別のメディアで起動しない）。
- 起動時プローブだけBOTの転送タイムアウトを短くするかどうかは、Stage 3の
  注意点のとおりここで決める。

## 実機での判断記録

- **暫定の3,000 ms connect待ちは無駄だった。** 計測を切り詰めないための
  安全側の初期値だったが、実測の最悪は246 msで、従来の500 msでも足りていた。
  connectはボトルネックではない。
- **`unit ready attempts=1`で2.5秒**という結果が、この計測でいちばん重要な
  発見だった。「readyになるまでpollする」という設計上の想像と違い、実際には
  **最初の1コマンドが返ってこない**。poll間隔の調整では何も改善しないし、
  budgetによる打ち切りもコマンド境界でしか効かない。
- **メディア無しカードリーダーは10秒待っても読めない。** 待ち時間の設計は
  「遅いデバイスを待つ」だけでなく「来ない相手を切る」問題でもある。
- 冷起動と`usbmargin`の差が25 ms以内だったので、以後デバイスを増やして
  計測し直すときに電源の入れ直しを繰り返す必要はない。
- **メディア無しの判定は待つ必要がまったくなかった。** 予算を4,000 msに
  縮めるかどうかを議論していたが、実機では**1 ms・1試行**で確定した。
  「遅いデバイスを待つ」問題と「来ない相手を切る」問題は別物で、後者は
  時間ではなくsense dataで解くのが正解だった。予算はsense dataで判定
  できない相手のための安全網として残る。
- **`BOOT_CONNECT_WAIT_MS`はそのまま「USB無し起動の遅さ」になる。** 実測
  1,081 msは固定約80 ms＋1,000 msと一致し、この経路にはデバイス差がない。
  マージンを増やす判断は、必ずこの起動コストの増加とセットで考える。
- **「USBストレージ無し」には2種類ある。** ハブだけ挿さっている状態（672 ms、
  connectは即座に返る）と、何も繋がっていない状態（connect待ちを使い切る）は
  起動コストが別物で、ログを取り違えると後者のコストを見落とす。当初は後者の
  所要時間をログに出しておらず、取り違えたまま「USB無しでも672 ms」と読める
  状態だった。
