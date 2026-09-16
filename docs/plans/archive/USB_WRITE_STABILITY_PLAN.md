# USB MSC書き込みの不安定さ 調査記録

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)
> この文書は調査記録と対策計画です。現在の実装仕様は現状文書
> （[`STORAGE.md`](../../STORAGE.md)、[`USB.md`](../../USB.md)）とコードを優先してください。
> 読み出しまでの実装計画は[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)です。

## 状態: **完了**（機能受入。根本原因は未特定のまま緩和策で封じ込め）

単一ブロックは第24版までに、複数ブロックWRITE(10)とFull-Speed固定ハブ経路の故障は
[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md) Stage 7で解消した。
`src/fs/usb_msc.rs`の`MAX_WRITE_BLOCKS`は回避策の`1`から`8`になり、2媒体×2 topologyで
2／4／8 blockが各10回PASS、A／B／Cの`usbcheck`／`fswritetest`／HID／PC媒体検査も
上限変更後のbinaryでPASSしている。受入時のcounterは`recovery ok+0 failed+0`、
`proactive read+0`で、下の「症状」にある頻繁な回復は再現しない。

**根本原因は未特定である。** 下の「まだ分かっていないこと」と「根本原因を追う場合の
次の手」は、緩和策を外して原因まで特定したい場合の入口として残す。同節が書いている
とおり、これらは機能受入の残作業ではない。

MSCとHIDの単一ブロック経路は第24版のHigh-Speedハブ複合回帰まで実機確認済みである。
一方、第25版で判明した複数ブロックのWRITE(10)は決定論的に失敗するため、`src/fs/usb_msc.rs`の
`MAX_WRITE_BLOCKS = 1`で1ブロックずつ発行して回避している。この回避は有効だが根本原因は
未特定で、第25版の決定論的な再現手順（下記）が現時点で唯一の入口である。

WRITE(10)と`usbwritetest`／`usbzero`は実装済みで、当初は間欠的にデバイスが無応答になった。
第18版ではREAD 16回ごとと各WRITE直前のBOT再同期、MSC故障の局所隔離、ハブ初期化順序を
組み合わせ、High-Speed直結とFS-onlyハブ＋HID併用の実機受入条件を完走している。
2026-08-23の一連の実機試験で分かったこと・否定したこと・入れた緩和策をまとめる。

## 症状

`usbwritetest <lba>`（1ブロック書き→照合→復元）の途中で、**デバイスがバス上の
すべての要求に応答しなくなる**。

```text
USB BOT: bulk IN packet retries exhausted during CSW   ← どの段階かは実行ごとに違う
USB: transfer QTD packet error, status=0x00000001
USB:   HCINT=0x00000002                                 ← ChHltdのみ。エラービット無し
USB:   bytes transferred=0x00000000
USB: control transfer failed at the SETUP stage         ← controlまで巻き込まれる
USB BOT: reset recovery failed
```

エラービットが1つも立たず0 byteという形は、**ホストのトランザクションが壊れた形では
なく、相手が何も返していない形**である。以後はcontrol転送も通らず、Reset Recoveryも
実行できない。

## 確定した事実

| 事実 | 根拠 |
| --- | --- |
| **宛先ずれではない** | `usbwritetest`が対象LBAの前1・後2ブロックを照合し、`collateral changes=0`が一貫して出る |
| **CDBは正しい** | WRITE(10)のCDB構造はREAD(10)と同一で、READは実機で長時間動作している |
| **読み出しも同じ故障を起こす** | `mix`のsoakで4 KiB READ(10)が37回成功後にBulk IN timeout（[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)の2026-08-21追補）。今回も`bulk IN timed out during data IN`が複数回 |
| **書き込みは引き金が桁違いに強い** | 読み出しは長時間動作するが、書き込みは数回に1回失敗する |
| **チャネル0の共有が被害を広げる** | control／bulkが全部チャネル0。一度おかしくなると再列挙まで何も通らない |
| **`reset recovery complete`は回復の証拠にならない** | 確かめているのはcontrol転送が通ったことだけ。bulkが1パケットも動かないまま「成功」と報告する |
| **セルフパワーハブではroot VBUSのpower cycleが効かない** | 上流のVBUSが消えても下流ポートへ給電し続けるため、デバイスの状態が保存される |
| **このUSBメモリはSYNCHRONIZE CACHE(10)非対応** | sense key 5／ASC `0x24`。安価なコントローラは非対応を`0x20`ではなく`0x24`で返す。CSW `0x01`で失敗しながらsenseを18 byteのゼロで返す個体もある（response codeまで`0x00`＝規格上無効）。非対応の返し方はデバイスごとにばらつくので、senseのresponse codeを検証したうえで「故障を報告していない」応答は非対応として扱う |
| **複数ブロックのWRITE(10)は決定論的に失敗する** | ファイルシステム経路で判明（第25版）。1回の`mkdir`のうち単一ブロックのWRITE(10)は3本とも成功し、**最初の8ブロック（4 KiB）で必ず失敗**する。3回実行して3回とも同じ。パケットはすべて受理されたあとデバイスがCSWを返さず、bulk INがtimeoutする |

## 否定した仮説

**同じ間違いを繰り返さないために残す。**

| 仮説 | 否定した根拠 |
| --- | --- |
| デバイスが書き込み処理でビジーなだけ | SYNCHRONIZE CACHE＋ready待ちを挟んでも同じ場所で落ちる。またビジーならSETUPはACKされるはず |
| ハブ経由固有 | 直結でも同じ形で落ちる |
| 「2本目のWRITE」固有 | 1本目で落ちる実行もある |
| 100 ms間隔のready pollingが足りない | `unit ready attempts=1`のまま2.5秒経過。待ちは**単一コマンドの中**にあり、poll間隔は無関係 |
| 電力不足だけが原因 | 電源付きハブでもHigh-Speedでは落ちる。`port since bus came up: no events`（過電流・脱落なし） |
| チャネル0がhaltできないのが原因 | `HCINT=0x02`＝ChHltdが毎回立っており、チャネルは正常にhaltしている |
| メディア無しは待てば来る | 空のカードリーダーは110 msごとにnot readyを返し続け、10秒待っても変わらない（sense ASC `0x3A`で即断すべき） |

## 決定論的な再現手順（第25版で判明）

> 第25版の試験は**複数のUSBメモリ**にまたがって行われている。規格の守り方に個体差が
> 大きいデバイス群で、VPDページの返し方もsenseの返し方も個体ごとに違った。以下の
> 記述は観測した挙動であって、どれか1台の性質として読まないこと。


**これがこのプランで初めての決定論的な再現である。** ほかの症状はすべて間欠だった。

複数ブロックのWRITE(10)——1回のdata OUTフェーズに512 byteパケットを複数流す形——が
必ず失敗する。単一ブロックのWRITE(10)は通る。

この形は**ファイルシステム経路が初めて発行した**。`usbwritetest`は1ブロック、
`usbzero`は1ブロックずつのループなので、それまでどのコマンドも複数ブロックの
WRITE(10)を出していない。受入試験が全て単一ブロックだったのはそのためで、
「書き込みは数回に1回失敗する」という間欠故障とは**別の問題**である可能性が高い。

現在は`src/fs/usb_msc.rs`の`MAX_WRITE_BLOCKS = 1`で回避している。2ブロックは未試験で、
この定数がそのまま実験の入口になる。上記候補2（OUT側data toggle）と候補3（OUT側の
staging／cache同期）は、この再現手順で直接試せる。

## まだ分かっていないこと

**なぜHigh-Speedの書き込み中にデバイスがバス全体へ応答しなくなるのか。** 候補:

1. Bulk QTDのtimeout（約1秒×4回＝約5秒）が、このデバイスの書き込み完了待ちに短い。
   ただしtimeout後にcontrolまで死ぬことの説明にはならない。
2. OUT側data toggleが、data phaseを跨いだ後のCBWでずれる。
3. 512 byteのOUT data phase固有の問題。この転送はMSCの書き込みでしか発生せず、
   READ経路にあるMPS境界・staging・cache同期の扱いがOUT側にも要るのではないか。
4. 電力の瞬時的な落ち込み。USBテスターで供給電流が少なめに見えるという観察があり、
   電源付きハブで頻度は下がった。ただし過電流ビットもデバイス脱落も記録されていない。

**Split転送経由のHIDは別問題として第24版までに解消した。** High-Speedハブ配下の
Low-Speedキーボードでは当初`HCINT=0x82`（XactErr＋ChHltd）で失敗し、`usbfs on`
（FS/LS固定＝Splitを消す）だけが回避策だった。第19〜24版でmicroframe配置、endpoint type、
周期CSPLITの上限、1 kHz前景poll、下流port単位の切断、不正なLS `bInterval=1`の10ms補正を
順に行った。第24版は入力安定・エラーログなし・10秒静止時約1,000 Split packetを実機確認済み。

## 実装した緩和策

根本原因は直っていない。**「壊れたときに自力で戻る」「壊れたことを取り違えない」**
ための対策である。

| 対策 | 何を防ぐか | ログ |
| --- | --- | --- |
| controller使用不能フラグ（`hcd::note_bus_unusable`）＋自動再列挙 | channel 0をhaltできないcontroller故障がcold bootまで残る状態 | `the bus needs re-enumeration` |
| 連続回復回数での判定（`RECOVERY_ATTEMPT_LIMIT`） | 「回復成功」を繰り返して何も直っていない状態を見逃すこと | `recovery is not holding` |
| MSC sessionの隔離＋使用不能中はBOT commandを即失敗 | 1台のMSC故障によるHIDの自動再列挙と、死んだsessionへのcommand乱打 | `session is unusable, skipping commands` |
| root VBUSの自動power cycle（30秒に1回まで） | port resetで戻らないデバイス | `power-cycling USB-A` |
| **ハブポート単位の電源断** | セルフパワーハブ配下のデバイス（root VBUSでは電源が切れない） | `power-cycling hub port N` |
| periodicがarm中はnon-periodic TX FIFOだけflush | MSCの転送失敗がHIDのsessionを巻き添えで殺すこと | `flushed only the non-periodic FIFO` |
| periodicチャネル停止検出（`HCCHAR.ChEna`で判定） | HIDが無言で止まり`usbinfo`にだけ残る状態 | `periodic channel stalled` |
| stale session再列挙のバックオフ | attach直後に失敗するデバイスがバス全体を毎秒リセットすること | `repeated stale sessions` |
| ポートイベントのラッチ | ISRが消してしまう過電流・デバイス脱落の証拠 | `port since bus came up:` |
| REQUEST SENSEでのCHECK CONDITION回収 | 失敗理由が分からないこと、senseを保持したままのデバイス | `sense key=` `ASC=` |
| メディア無しの即断（ASC `0x3A`） | 空のカードリーダーで起動が10秒延びること | `has no medium` |
| 成功16 READごとの予防的BOT再同期 | 33〜52回の連続READ後にBulkとEP0が無応答になる前にBOT境界を再確立 | `proactive BOT resyncs before READ(10)=N`（初回と64回ごと） |

詳細は[`USB.md`](../../USB.md)の「電力問題の切り分け」「転送失敗の巻き添え」
「periodic HIDの停止検出」「転送失敗からの自動復帰」を参照。

## 診断ログの読み方

- `HCINT`が`0x02`（ChHltdのみ）で`bytes transferred=0` → **相手が応答していない**。
- `HCINT`にXactErr（bit 7）→ トランザクションが壊れている。第19〜23版で調査した
  Split HID障害はこの形だった。
- `USB BOT: ... during <phase>` → `CBW`／`data OUT`／`data IN`／`CSW`のどれで落ちたか。
- `USB BOT: failed command opcode=` → 失敗したSCSI command。続くtagはattach後のcommand
  通番、data bytesとdirectionはdata phaseの形である。主なopcodeはTEST UNIT READY `0x00`、
  INQUIRY `0x12`、READ CAPACITY(10) `0x25`、READ(10) `0x28`、WRITE(10) `0x2A`、
  SYNCHRONIZE CACHE(10) `0x35`。
- `USB MSC:   at LBA=`以下 → 失敗したREAD／WRITEの範囲。READでは`FUA=1`が媒体からの
  検証読み出し、`FUA=0`が通常読み出し。方向別`proactive READ/WRITE resyncs=`は、そのattachで
  失敗までに実行した予防再同期の累計である。成功時の同一ログは初回と64回ごとに間引く。
- `HCCHAR`の下位11 bitがMPS、bit 15が方向、bit 19〜18がタイプ、bit 28〜22がデバイス
  アドレス。MPS 64＋EP0はcontrol転送、MPS 512はHigh-Speed bulk。
- `port since bus came up:` に`OVER-CURRENT`が出れば**電力問題は確定**。出ないことは
  弱い証拠にしかならない（5Vスイッチのfault出力が配線されていない可能性）。
  `connect-change`はデバイスが一度バスから消えたこと＝ブラウンアウトの示唆。
- ハブ配下なら`usbhub`の`OVERCURRENT`（規格で定義されたポートごとの過電流ビット）が
  より確実。

## 根本原因を追う場合の次の手（受入の残作業ではない）

1. **Bulk転送のtimeout予算を書き込み向けに見直す。** 現状は読み出しと同じ約5秒。
   単一コマンドの中で数秒待たされる実測があるので、まずここを延ばして頻度が変わるかを
   見る。効かなければ仮説1は消える。
2. **OUT data phaseのtoggle／cache同期をREAD経路と突き合わせる。** 512 byte OUTは
   MSCの書き込みでしか通らない経路で、READ側にある扱いが欠けていないか確認する。
3. 電力の直接測定。USB-Aの5Vを外部で測るのが確実（`ina226.rs`はバッテリーを測って
   おりVBUSではない）。

採用中の予防的BOT再同期でREAD／WRITEの受入条件は満たしているため、上記は緩和策を
外して根本原因まで特定したい場合の調査候補であり、機能受入の残作業ではない。

## 再現・確認手順

**データの入ったUSBメモリでは実行しないこと。**

```sh
cargo run --release
```

1. `usbwritetest <lba>` — 空のUSBメモリの、ファイルシステムが無いLBAで。
2. 失敗したら`usbzero <lba>`で残骸を消す（`usbrescan`でsessionを作り直してから）。
3. `usbhw`で`port events since bus came up`を確認。
4. ハブ経由なら`usbhub`で`OVERCURRENT`とpower switchingの種別を確認。
5. HIDを併用する場合も通常のHigh-Speed設定を使う。`usbfs on`はSplit切り分け用の診断に限る。

## 時系列の記録

各版で何を直し、実機で何が起きたかの要約。

| 版 | 変更 | 実機結果 |
| --- | --- | --- |
| 1 | WRITE(10)と`usbwritetest`を実装 | 直結成功・ハブ失敗。**成功と表示されたのにデータが壊れた** |
| 2 | 窓照合、SYNCHRONIZE CACHE、ブロック長確認 | 宛先ずれを否定（`collateral changes=0`）。2本目のWRITEで失敗 |
| 3 | CHECK CONDITIONのsense回収、FUA照合 | ASC `0x24`でSYNCHRONIZE CACHE非対応と判明 |
| 4 | 非対応判定をASC非依存に、flush失敗を復元失敗と数えない | **判定バグで「成功した実行を失敗と表示していた」** |
| 5 | packet error時のHCINT／HCCHARダンプ、BOT段階のログ | 「相手が無応答」と確定。ハブ固有・2本目固有を否定 |
| 6 | チャネルhalt失敗でバス使用不能フラグ | **発火せず**。チャネルは正常にhaltしていた |
| 7 | Reset Recovery失敗でもフラグを立てる | **発火せず**。recoveryは毎回「成功」と報告していた |
| 8 | 連続回復回数で判定、テストの早期打ち切り | 自動復帰が動作。cold boot必須の状態は解消 |
| 9 | ポートイベントのラッチ、periodic停止検出、FIFO巻き添え回避 | HIDが死ななくなった。HS書き込みは依然失敗 |
| 10 | バス使用不能中はコマンドを送らない、ハブポート電源断 | 実機確認待ち |
| 11 | BOT故障をcontroller故障から分離し、MSC sessionだけを隔離 | 実機確認待ち。HIDを維持し、MSCは`usbrescan`まで即失敗する設計 |
| 12 | periodic完了公開と`ChEna`停止判定の競合を解消 | `usbfs on`＋HID併用の`ut 100`は33/100でMSC timeout。ただしMSC sessionだけが停止し、自動全バス再列挙なし |
| 13 | MSC併用時はperiodic HIDを停止し、全classをchannel 0へ逐次化 | FS-only `ut 100`は39/100でMSC timeout。ただし試験後もHIDは動作し、MSC sessionだけが停止 |
| 14 | Bulk IN/OUTをすべて1 packet QTDへ限定 | FS-onlyは40/100、High-Speed直結／MPS 512も52/100でMSC timeout。速度・ハブ・HID・QTD集約を原因候補から除外 |
| 15 | 成功16 READごとにBOT境界を予防再同期 | High-Speed直結／MPS 512と、FS-onlyハブ／MPS 64＋HID併用の両方で`ut 100`が100/100 PASS。各々予防再同期6回、packet／command retry 0。FS-only試験後もHID動作 |
| 16 | 各WRITE(10)直前にもBOT境界を予防再同期 | High-Speed直結とFS-onlyハブ＋HID併用で`usbwritetest 2`が各10/10 PASS。各回のpattern照合、原本復元、周辺照合すべて成功。ただしHID事前接続のハブ初回列挙に別の失敗あり |
| 17 | ハブ全port列挙後までHID periodic開始を延期 | v18と合わせて実機確認。HID後挿しだけ成功する初回列挙順序差を解消 |
| 18 | 給電中ハブの下流reset完了をedgeで待つ | 給電中ハブ＋HID事前接続で上流再接続5/5認識。FS-only `ut 100`も100/100、予防再同期6回、retry 0でPASS |
| 19 | SSPLIT／CSPLITをHigh-Speed microframeへ配置 | 列挙・HID attach成功。続くInterrupt IN CSPLITは`HCINT=0x82` |
| 20 | Split HIDの`HCCHAR.EPType`をInterruptへ修正 | 文字入力まで成功。ただしidle CSPLIT NYETを最大5000 round追い、`giving up mid-split`連続と長いfreeze |
| 21 | Interrupt CSPLITを1 full frame内へ制限 | エラー・freezeなし、文字入力可能。ただし描画と同じ約57 Hz pollで反応が鈍い |
| 22 | Split keyboardを`bInterval`周期でpoll | 新HSハブ＋LS keyboardで入力正常。50,661 packet／202,076 round、error／freeze／conflict／stale token／port eventなし。この版時点ではMSC併用回帰待ち |
| 23 | stale Split HIDの下流slotだけを切断 | 抜去・再挿入は成功。ただし通常入力は145,394 packet／436,167 roundで取りこぼし多発 |
| 24 | periodic SSPLIT位相とLS intervalを正常化 | High-Speedハブ＋Low-Speed keyboardで入力安定、エラーログなし、10秒静止時のSplit packet増加約1,000回。同じハブのHigh-Speed MSCとの`ut 100`も100/100、retry 0、予防再同期6回でPASS。Split 1,126 packet／2,370 round、conflict 0、active 0、stale token 0、port eventなし |
| 25 | ファイルシステム経由の書き込みを実装（[`FILESYSTEM_WRITE_REFACTOR_PLAN.md`](FILESYSTEM_WRITE_REFACTOR_PLAN.md)） | `fswritetest /vol/usb0p1`が最初の`mkdir`で失敗。WRITE(10)が4本通ったあと`bulk IN timed out during CSW`。Reset Recoveryは成功、WRITEは再送せず、MSC sessionと当該mountだけが使用不能になった——**故障時の要求どおりの挙動を実機で確認**。成功経路は未確認 |
| 25a | 上のときのstaging bufferにアラインメント宣言が無いことが判明 | `fs/usb_msc.rs`のWRITE(10)用stagingが`[u8; 4096]`（アラインメント1）で、`hcd.rs`が転送前に行うcache writebackはROM側が行頭でない範囲を拒否する。`cache_writeback_invalidate`の結果はこちら側で捨てているため無言で通る。`align(64)`を宣言。上記「まだ分かっていないこと」候補3（OUT側のcache同期）の一部にあたる |
| 25b | WRITE(10)のLBAとブロック数をログへ、複数ブロック初回を通知 | **単一ブロック3本成功→最初の8ブロックで失敗**を確認（LBA 0x7D50、blocks=8）。`MAX_WRITE_BLOCKS = 1`で回避。決定論的な再現手順として上に記録 |
| 25c | 複数ブロックWRITE(10)を1ブロックへ制限 | **WRITE(10) 23本が転送故障なしで通過。** 続いて`SYNCHRONIZE CACHE(10)`が理由を返さずに失敗し、これを非対応扱いへ分類。senseのresponse code検証も追加 |
| 25d | 予防的BOT再同期の成功ログを方向別の初回と64回ごとへ間引き、転送失敗時のREAD／WRITE範囲と方向別累計を追加 | `fswritetest` 2回はともに検査1〜3を通過したが、1回目は検査4のCSW、再列挙後の2回目は検査5のREAD data INで停止。途中の障害から復帰した箇所もあり、固定LBAではなくコマンド累積後のsession不調を示す。従来ログ1,050行中737行を占めた同一再同期行を圧縮して次回確認待ち |
| 25e | BOT transport失敗時にCDB opcode、command tag、data長、方向を追加 | 短縮ログは1,050行から142行へ減少。検査2中のREAD(10)（LBA `0x7E20`、8 blocks、tag `0x14F`）はReset Recovery後の再送で復帰。検査4先頭のCSW timeoutはopcode `0x00`、tag `0x9E4`、data 0で、媒体確認のTEST UNIT READYと確定 |
| 25f | TEST UNIT READYもReset Recovery成功後に1回だけ再送 | 検査4と検査8先頭のTEST UNIT READY timeoutはどちらも再送で復帰し、従来の検査4停止から検査10まで進行。途中のREAD(10)障害4回もすべて既存の再送で復帰 |
| 25g | 媒体確認の再送安全な照会を共通化し、READ CAPACITY(10)とINQUIRY／EVPDにも1回再送を適用 | 検査10先頭の媒体確認でREAD CAPACITY(10)（opcode `0x25`、tag `0x1597`、8 bytes）がdata INでtimeoutしていた経路を回復可能にした。`fswritetest /vol/usb0p1 1 1`は**12検査PASS、45,383 ms**。ただし途中のrecoveryは多く、相互運用上の根本原因は未解明 |
| 25h | 別メーカー媒体で再試験し、短いBulk INの超過時に要求長・実受信長・先頭16 byteを追加 | 別媒体でもREAD(10)／READ CAPACITY(10)のdata INとCSWが無応答になり、検査5でReset RecoveryのEP0まで停止した。ポートは接続・enable・給電を維持し、媒体sectorに依存しないopcode `0x25`でも再現したため、flash媒体不良説はさらに弱まった。最初のREAD CAPACITYは8 byte要求に対する長さ超過だった。次回、13 byteかつ先頭`0x53425355`ならCSW先着＝BOT phase不一致、別の値ならdevice応答またはHCD実受信長計上を調べる |
| 25i | 失敗したハブポートを無効化してから後続ポートを列挙 | 別媒体試験後の起動scanでport 2の最初の8-byte device descriptorが失敗し、その後port 3のLS HIDとport 4のFS HIDもSTALLして一台もattachされなかった。列挙失敗portをenable／address 0のまま残していたため、`CLEAR_FEATURE(PORT_ENABLE)`で隔離して後続HIDのaddress 0列挙を保護する。実機再確認待ち |
| 25j | 正常command間の予防再同期からpacket-failure用HCD回復／FIFO flushを除外し、CSW tag不一致の詳細を追加 | 別媒体の再試験でも検査4最初の致命的READが前回と同じtag `0xA36`、READ再同期132回、WRITE再同期254回で再現し、LBAだけ`0x43D0`から`0x4400`へ移動したためA/Bを実施。結果はexpected tag `0x16`に対してreceived tag `0x15`、residue 0、PASSEDという直前commandの正常CSWを確認し、2回目のWRITE（tag `0x27`）で停止した。flush原因説は否定され、controller側residueを掃除する緩和効果が確定したため変更を戻した |
| 25k | 内部Bulk packet retryで初回と転送済みbyteを記録 | READ CAPACITY(10)の8-byte data phaseで13-byte `USBS`、tag `0x1A`を受信し、current commandはtag `0x1B`だったため、直前commandの正常CSW再提示を確定。先行する内部retryは無く、後のCSW／data IN timeout retryも`bytes already transferred=0`だけだった。DMA済みbyteをtimeout後に再投入する仮説は否定 |
| 25l | channel-0成功判定にHCINT.XferComplとQTD Active解除を必須化し、異常長時にraw HCINT／QTDを追加 | 通常経路はChHltd後にQTD status 0だけで成功扱いしており、periodic HID経路が既に行うXferCompl検査を欠いていた。古いChHltd snapshotまたはhardware所有中QTDを前commandのshort packetとして回収し得るため、両条件を満たさなければtransport errorにする。実機再確認待ち。なお25k試験のMSC停止後もHID操作は可能で、故障局所化は確認済み |
| 25m | 正常command間の予防処理をhost channel／FIFO cleanupだけにし、device-facing Reset Recoveryを実失敗時へ限定 | 25l版では誤成功検出も異常長も出ず、内部retryはすべて0 byteだったが、tag `0x1E`、`0xA36`、`0xA59`で相手がBulk INからEP0まで無応答になった。XferCompl判定漏れはこの再現の主因ではない。二媒体で同じcommand位置までにREAD前132回＋WRITE前254回のMass Storage Reset／CLEAR_FEATUREを行う非標準的な緩和策が共通しているため、controller FIFO cleanupは維持しつつdevice reset 386回を外してA/Bした。実際のfailure後は完全なBOT Reset Recoveryを維持。同一起動で**host cleanupだけの`ut 100`と`fswritetest /vol/usb0p1 1 1`が両方PASS**し、READ／WRITEとも正常command間のdevice resetが不要と確認 |
| 26 | Stage 0の観測契約（[`USB_BOT_HCD_REFACTOR_PLAN.md`](USB_BOT_HCD_REFACTOR_PLAN.md)）を実装し、2媒体×3接続でbaselineを採取 | 転送挙動は変えず、cache拒否・packet失敗のrequested／actual・FIFO flush timeout・retry種別・CSW異常を数えた。`ut 100`は6構成すべてPASS、cache拒否0、FIFO flush timeout 0、CSW異常0。**Full-Speed固定ハブ経路だけがWRITEで落ちる**（`fswritetest`が両媒体とも検査1でFAIL、`usbwritetest`は片媒体で復元WRITEが10回中8回失敗）。HS直結・HSハブは同じ試験を通過。25kが「否定」とした**timeout後の転送済みbyte再投入がこの経路で実在**した：31 byte CBWの10 byte転送後に同一DATA PIDで先頭から再送、および64 byte OUTが`requested=64 actual=64`のままhaltせず4回再送。ただし`QTD final=0x00000000`は「全量転送済み」と「descriptor未書き戻し」を区別できず、`cancel`はこの妥当性を確かめずにactualを計算している |
| 27 | Stage 1: DMA bufferとcache同期の契約化（HCD所有の64 byte整列packet staging、拒否を`PacketOutcome::CacheSyncFailed`として転送失敗にする、periodic report bufferを64 byte境界へ） | 6構成すべてでcache拒否`+0`、`pkt-fail cache`+0、FIFO flush timeout `+0`、CSW異常`+0`。read soakとwrite 10回も6構成PASS。Stage 0で復元WRITEが10回中8回失敗したB1（FS固定ハブ／不明媒体）が10/10になったが、**Stage 1の効果とは断定しない**——Stage 0のB1の失敗はCSW timeoutで、cache拒否は当時も0だった。今回B1で出たのは34件のQTD packet error（全て`bytes already transferred=0`、全てretryで回復）。B1／B2の`fswritetest`は予定どおり検査1で落ちる。B2では**Reset Recovery自体が失敗**した（`CLEAR_FEATURE(ENDPOINT_HALT)`のcontrol IN status stageがQTD packet errorで2回とも失敗）——Stage 0ではrecoveryは完了していたので新しい観測。先行するのは25kが否定した形と同じ、64 byte OUTの`requested=64 actual=64`のまま4回再送 |
| 28 | Stage 2: `actual`を1箇所で導出し「0 byte」と「不明」を型で区別、再送を`Known(0)`のpacketだけに制限、短いOUTを`reap`で失敗に | `usbcachefail`（cache同期拒否／古い世代の完了／短いOUTの3注入）が9ゲートPASS——3つの契約が実機で成立。A1／A2／C1／C2は`usbcheck` RESULT PASS、`fswritetest`もPASS。**ただし信頼性判定を広く取りすぎてB1（FS固定ハブ／不明媒体）を壊した**: 「`XferCompl`が無いなら`QTD_EOL`必須」としたため、これまで`bytes already transferred=0`で正常に再送できていたpacket errorが`Unknown`になり、Stage 1で10/10だった書き込みが0/10に。実測できたcontrol wordは完了SETUPの`0x07000000`（EOL保持）とtimeoutの`0x00000000`だけ。判定を「`XferCompl`無し＋要求長>0＋control word丸ごと0」へ絞り、`refusing to resubmit`とretryのログへ`reap QTD control=`を追加して根拠を残すようにした。counterも`refused`と`progressed`へ分離（B1は`progressed+10 bytes=0`という矛盾した行を出していた） |
| 29 | 再送規則を「放棄されたpacket」と「失敗が報告されたpacket」へ分離（28の後退修正） | `refusing to resubmit`へ`reap QTD control=`を追加して実測したところ、**このcoreはpacket error時に不可能な残量を書き戻す**ことが判明: 64 byte OUTに対して`0x16018889`（残量100,489）、31 byte CBWに対して`0x16000080`（残量128）。いずれも`Active`解除・status 1・`EOL`保持の正規writebackで、byte欄だけが成立していない。28で`remaining > requested`を`Unknown`とし再送禁止にしたため、この経路のpacket errorが全て再送不可になりB1が0/10に。**packet error（QTD status 1）は残量に関係なく再送**（USB packetは不可分、1 packet QTD、同一PID再送はtoggleで重複が捨てられる。Stage 0/1でFUA read-back 10/10確認済み）、**timeoutは`Known(0)`のときだけ再送**へ変更。B2で分離が正しく働くことを確認（`0x16000040`は再送、`0x06000000`は拒否）——`requested=64 actual=64`の4回再送は消えた。不可能残量は`impossible-len` counterで可視化。計画の完了条件「impossible lengthが0」は実測で否定されたため「byte数として使わない」へ改めた |
| 30 | 29の再測定（B1のみ） | pattern writeが0/10→**10/10**で後退解消、`refused+0`——Stage 2のretry規則は1件も再送を止めていない。counterは全て自己整合（`proactive write+20`＝10 pattern＋10復元、`timeout+32`＝4 retry×8失敗round、`impossible-len+12`＝`0x16018889`×10＋`0x16014849`×2）。**復元WRITEは2/10でStage 0と同一**、Stage 1の10/10は再現せず——1サンプルずつなのでStage 1が効いたのか運かは区別できない。不可能残量12件は全て`Unknown`として弾かれ、byte数として使われず正当な再送も妨げなかった。残る8失敗は全て同形: 復元WRITEのdata OUTがpacket error（`0x16000040`＝1 byteも出ていない）→再送成功→**CSW INに`HCINT=0x00000000`でdeviceが一切応答しない**（`0x06000040`＝1 byteも受信せず）→5試行で失敗。実転送長でもretry安全性でもなくBOT phase／Recoveryの問題なので、この完了条件をStage 3へ移した |
| 31 | Stage 3実機A/B: reported OUT packet error後のcleanupをnon-periodic TX FIFOへ限定し、Mass Storage Reset後に150 ms待機 | B1／B2の`usbcheck`はread 100/100、write／restore 10/10、CSW異常0で全PASS。`out-nptx`はpacket errorと34／1件で一致し、cleanup全撤去時のstatus 1連続再発はなかったため方向別cleanupを採用する。`fswritetest`はB1がdata OUT retry後のCSW無応答、B2がdata OUT retry後の次packet無応答で検査1のままFAIL。ただしB2のReset Recoveryは150 ms待機後にcompleteし、前回のCLEAR_FEATURE失敗を解消した。Stage 3全体はNo-Go |
| 32 | Stage 3実機A/B: direct channelのHCCHAR MC/ECを1へ設定 | **No-Go。**B1は`usbcheck`全PASS、packet error 20件、`fswritetest`は従来と同じdata OUT retry後のCSW無応答。B2はv25のread 100/100から**4/100**へ後退し、data INがhaltしないtimeoutを2 command続けて起こした。Reset Recovery自体は2回completeしたが、READ再送にも保持せずsession unusable。`fswritetest`も従来と同じ次data OUT無応答。通常の非周期channelはMC/EC=0へ即時rollbackし、splitだけ1を維持する |
| 33 | Stage 3診断: v25の転送動作へ戻し、失敗channelのregister snapshotを追加 | B1／B2の`usbcheck`はread 100/100、write／restore 10/10へ復帰しrollback確認。B1の失敗CSWは強制halt前HCCHAR `0x80C88840`（ChEna=1、Bulk IN）、HCTSIZ `0x400000FF`（DATA1、schedule `0xFF`）で正しくarm済み、HCINT 0、port正常、HFNUM進行のままhaltしなかった。B2もpacket error再送後の次data OUTが従来形で停止。古いendpoint／PID設定の持ち越しではなく、再送成功直後の次channelをcoreが処理しない形へ絞れた。B2のReset Recoveryは今回はCLEAR_FEATUREで失敗し、150 ms待機は成功保証ではない |
| 34 | Stage 3実機A/B: packet error再送の成功後に50 ms settle | **No-Go。**B1／B2の`usbcheck`は100/100、write／restore 10/10を維持したが、`fswritetest`は両方とも検査1で従来と同じ失敗。B1は`0x16000040`再送後のCSW INがHCCHAR `0x80C88840`、HCTSIZ `0x400000FF`、HCINT 0のまま停止。B2は同じdata OUT error再送後の次data OUTが停止し、Reset RecoveryもIN status stageのpacket errorで失敗。再送成功後50 msはregister値にも結果にも影響しないため撤去 |
| 35 | Stage 3実機A/B: 0 byte進行OUT packet errorの再送成功後にchannel／NPTX cleanup | **No-Go。**B1／B2の`usbcheck`は全PASSを維持。B1 raw burstは3/32後に`QTD=0x16000040`が20回連続してretry exhaustedとなり、再送成功後cleanupへ一度も到達しなかった。B2 raw burstは32/32、pattern、復元PASS。局所cleanupを撤去した |
| 36 | 不安定なUSB WRITEの実機試験をfilesystemからraw犠牲範囲へ移行 | `usbrawcheck 1 32 1`はB1の従来故障をfilesystem objectなしで3/32に再現し、Reset Recovery後のsnapshot復元にも成功した。B2は32/32と復元がPASS。以後raw WRITEが安定するまで`fswritetest`を受入から外す。現在のB1／B2手順は`usbcheck 100 1`＋`usbrawcheck 1 32 1` |
| 37 | Stage 3実機A/B: MPS 64以下の非Split BOT Bulkをbuffer DMAへ変更 | **programming No-Go。**B1は起動中にMSC session unusable。B2は最初の31-byte CBWから`HCINT=0x92`（ChHltd＋NAK＋XactErr）、HCTSIZ未進行で失敗し、20 retryを使い切った。従来の累積後QTD故障ではなく、direct buffer DMAをMC/EC=0でarmした即時設定不良 |
| 38 | direct buffer DMAだけHCCHAR MC/EC=1 | Split buffer DMAと同じく1 transactionを明示する。v26のNo-Goはdescriptor DMAへ1を設定した結果なので、descriptor DMAは0、direct／Split buffer DMAだけ1とする。起動markerは`FS Bulk buffer DMA MC1 v31`。まずB2 usbcheckでCBW即時失敗が消えるか確認待ち |
| 39 | v31結果とbuffer DMA撤回 | **No-Go。**B2はHCCHAR `0x00D81040`でMC/EC=1を確認したが、HCINT `0x92`とHCTSIZ未進行は同一。B1も最初のCBWでtimeout／short OUT。非Split buffer DMAを撤回しdescriptor DMAへ戻した |
| 40 | channel-0 QTDを固定2-slot bankで交互使用 | **通常回帰Go／根本対策No-Go。**B2はusbcheckとraw 32/32が全PASS、B1もusbcheckは全PASSし、packet error時のHCDMAが`0x4FF51400`／`0x4FF51600`へ交互に切り替わった。しかしB1 rawは従来どおり3/32後のzero-progress errorを20回連続してFAIL（復元成功）。同一物理descriptor即時再利用説は否定。2-slot bankは所有境界として維持する |
| 41 | zero-progress Bulk OUT packet error後だけdescriptor-DMA modeをoff→on | **発火条件No-Go。**B2はusbcheck／rawが全PASS、B1もusbcheckは全PASSしたがrawは3/32で従来どおりFAILし復元成功。停止packetの最初の20 retryable errorはすべて不可能残量、上限後の最後だけ`QTD=0x16000040`だったため`descdma-restart`は0。descriptor残量は同じ故障中にも揺れ、recovery selectorにできない。mode再始動仮説は未判定 |
| 42 | 同一packetの2回目のreported OUT errorからdescriptor DMAを再始動 | 最初のerrorは従来NPTX cleanupだけで再送し、そのretryもerrorなら残量に依存せずoff→onする。B1 usbcheckの一過性33 errorはすべて最初のretryで回復しているため通常系を避け、停止系列だけを狙う。markerは`repeated-error DMA restart v34`、初回restartログも追加。実機確認待ち |
| 43 | v34結果とmode再始動撤去 | **No-Go。**B2 usbcheck／rawとB1 usbcheckは全PASS。B1 usbcheckでは33 retry中13回、raw停止packetでは2回目以降約19回の再始動が確実に発火したが、rawは同じ3/32でFAILし復元成功。descriptor fetch内部状態説を棄却し、controller-wide切替とcounterを撤去 |
| 44 | raw WRITE command間隔でdevice busy／excessive NAKを切り分け | **No-Go。**B1は250 msで0/32、`usbrescan`後の1000 msでも3/32で従来同様に停止。どちらもReset Recovery後のsnapshot復元は成功。command間busy説を棄却し、gap引数だけ診断用に残した |
| 45 | 非Split Full-Speed WRITE dataを512 byte／1 QTDへ集約 | **programming No-Go。**B2 READは100/100を維持したがWRITE／復元は1/10へ悪化。multi-QTDは12回発火し、data OUTのstatus 1が5、XferComplなしが2、CSW timeoutも発生。HCCHARはMC/EC=0の`0x00C81040`だった。B1／rawへ進まず |
| 46 | 複数packet descriptor-DMA QTDだけHCCHAR MC/EC=1 | **No-Go。**B2 READは100/100だがWRITE 1/10、復元0/10。失敗時HCCHAR `0x00D81040`で確実に発火したがv36から改善なし。公式v5.5.3のdirect descriptor設定もMC/EC=0と確認し撤回 |
| 47 | 64-byte QTD×8を1 descriptor list／1 channel activationで実行 | 長い512-byte QTDを使わず1 packet契約を維持し、packetごとのhalt／rearmだけ除く。最後だけHOC／EOL、HCTSIZ NTD=7、MC/EC=0。失敗listは再送しない。markerは`WRITE data packet-list v38`、まずB2 usbcheckのみ |
| 48 | v38結果 | **No-Go。**B2 READは100/100だがWRITE／復元0/10。全10回がQTD 0のstatus 1、不可能残量100,489で停止。HCTSIZ NTD=7、MC/EC=0、HCDMA=base+8なのでlistは正しく発火し、後続QTD前の最初のpacket errorを再送しない契約が全WRITEを止めた |
| 49 | status 1を検証済みQTD境界から再開 | 失敗QTDより前が全量成功、後ろがsubmit時とbit単位で同一の場合だけ、完了prefixを除いたlistを作り直す。失敗packetは同じDATA PIDで最大20回retryし、NPTX cleanup＋50 msを維持。markerは`QTD-list safe resume v39`、まずB2 usbcheckのみ |
| 50 | v39結果とQTD list撤回 | **No-Go。**B2 READ／WRITEは100/100・10/10まで回復したが、QTD 1から再開した復元WRITE後のCSWがtimeoutし復元9/10。packet error 33件、list activation 53回を要し、従来B2正常系より明確に後退。複数packet QTD／複数QTD listを撤回し1 packet QTDへ戻す |
| 51 | ESP-IDF v5.5.3既定のbalanced FIFO分割へ一致 | P4のusable FIFO 896 linesに対し、従来の独自比率RX/NPTX/PTX=448/224/224を、公式既定の512/256/128へ変更。packet処理ではなくglobal controller初期化の差をA/Bする。markerは`ESP-IDF balanced FIFO v40`、まずB2 usbcheckのみ |
| 52 | v40結果とFIFO再適用漏れの特定 | B2はREAD 100/100、WRITE／復元10/10、collateral 0でPASSし、1 packet QTDへのrollbackを確認。ただしhost行は`fifo=512/1024/1024`で、初期化時の設定が後続root-port resetで既定値へ戻っていた。FIFO仮説は未試験なのでraw／B1へ進まない |
| 53 | root-port reset後にbalanced FIFOを再適用 | ESP-IDFもport reset成功後に保存したFIFO設定を再適用する。初期化前の書込みをこの位置へ移し、実レジスタ`512/256/128`を保証してから同じB2を再試験する。markerは`post-reset balanced FIFO v41` |
| 54 | v41 B2 usbcheck結果 | **Go。**host行は`fifo=512/256/128`。READ 100/100、WRITE／復元10/10、collateral 0で全PASSし、v40のpacket error 34件が0になった。cache拒否、retry、impossible remainder、timeout、Recovery、CSW違反も0。同じB2のraw WRITE burstへ進む |
| 55 | v41 B2 raw WRITE結果 | **Go。**`usbrawcheck 1 32 1`はWRITE 32/32、pattern match、snapshot restoredで全PASS。READを挟まない連続WRITEでも安定した。次は同じbinaryをB1で確認する |
| 56 | v41 B1 usbcheck／raw結果 | **Go、旧故障解消。**`usbcheck`はREAD 100/100、WRITE／復元10/10、全counter異常0。従来3/32で停止した`usbrawcheck 1 32 1`もWRITE 32/32、pattern match、snapshot restoredでPASS。B1／B2の差は解消し、不正なFIFO設定を根本原因と判断。故障注入と残りの接続matrixへ進む |
| 57 | v41 usbcachefail結果 | **契約Go／harness No-Go。**[1/3] cache拒否の3 gateは全PASSしたが、session引退後の自動rescanでdownstream MSCの最初の8-byte descriptorがQTD status 1となり再取得できず、[2/3]・[3/3]をskipしてRESULT FAIL。media WRITEはない。注入間の1回だけのrescanが不足 |
| 58 | 故障注入間の再列挙をbounded retry | readyなMSCが無ければfull recovery rescanを最大3回、2回目以降は500 ms待って実行する。通常USB経路や注入内容は変えない。markerは`fault-rescan retry v42`、`usbcachefail`だけ再確認する |
| 59 | v42 usbcachefail結果 | **Go。**cache拒否、stale completion、short OUTの全9 gateとRESULTがPASS。注入間の2回の自動rescanは各1回目でMSC／keyboard／mouseを再取得し、最後の手動rescanも成功。topology非依存の故障注入を完了し、C構成matrixへ進む |
| 60 | v42 C1 usbcheck結果 | **Go。**High-Speed／MPS 512、FIFO 512/256/128。READ 100/100、WRITE／復元10/10、collateral 0、CSW契約違反0でPASS。開始付近にCSW timeout 2件が出たが各1回retryで回復し、command retry／Recoveryは0。同じC1のrawへ進む |
| 61 | v42 C1 raw WRITE結果 | **Go。**`usbrawcheck 1 32 1`はWRITE 32/32、pattern match、snapshot restoredでPASS。C1を完了し、同じHigh-SpeedハブのC2（Sony）へ進む |
| 62 | v42 C2 usbcheck結果 | **Go。**High-Speed／MPS 512、FIFO 512/256/128。READ 100/100、WRITE／復元10/10、collateral 0でPASS。cache拒否、packet failure、retry、timeout、Recovery、CSW違反は全0。同じC2のrawへ進む |
| 63 | v42 C2 raw WRITE結果 | **Go。**`usbrawcheck 1 32 1`はWRITE 32/32、pattern match、snapshot restoredでPASS。HID併用のB1／B2／C1／C2を完了し、High-Speed直結A1／A2の回帰確認へ進む |
| 64 | Stage 3のfilesystem試験をraw受入へ正式置換 | B1／B2の旧故障は`usbrawcheck`でも3/32停止として再現でき、v41では両媒体32/32へ改善した。失敗時にmetadataを残す`fswritetest`をtransport完了条件から外し、6構成の`usbcheck`＋`usbrawcheck`を正式な受入とする。残りはA1／A2 |
| 65 | v42 A1／A2結果、Stage 3完了 | **Go。**High-Speed直結の両媒体も`usbcheck 100 1`と`usbrawcheck 1 32 1`がすべてPASS。B1／B2／C1／C2、故障注入と合わせて6構成のStage 3受入を完了。次はStage 4のRecovery API、現時点で中断 |
| 66 | Stage 4: cleanup APIの分離とcleanup失敗の伝播 | 正常境界と失敗回復が共有していた`recover_channel_after_packet_failure`を廃し、`recover_failed_packet`（失敗path）と`proactive_cleanup`（暫定）へ分離。FIFO flushを`Result`化してtimeoutしたFIFO名を保持し、flush・timeout・skipをFIFOごとに数える。flush timeoutは対象commandを開始させない。`usbcachefail`へ[4/4] flush timeout注入を追加 |
| 67 | Stage 4実機結果 | **Go。**故障注入は全12 gateとRESULT PASS。A／B／C 3構成の`usbcheck 100 1`もPASSし、`fifo-flush`がcleanup回数（proactive read＋write＋実失敗）と全行で一致。3構成とも`proactive` 26回に対し`recovery ok+0 failed+0`で、「正常command数だけではdevice-facing resetしない」を実測。Cの実失敗recovery 2件はHID併用下で起き、HIDは再列挙されず生存 |
| 68 | Stage 5: READ前cleanup間隔をbuild時A/B | 間隔を`16`／`32`／`disabled`から`TAB5_USB_READ_CLEANUP`で選ぶ。未設定と`16`はバイナリ一致、3設定は別バイナリ、不正値はビルドエラー。shell optionにはせず、`usbcheck`が使用設定を毎回表示する |
| 69 | Stage 5実機結果 | **Go。**6行すべてPASS。`16`で100 readsに6回、`32`で3回と間隔が効き、`disabled`ではHigh-Speed直結／FS-onlyハブ＋HID／HSハブ＋LS HIDの3構成とも`usbcheck 1000`が`proactive read+0`で完走——旧故障の最短33 READ(10)の30倍。`mix` 120分も`retries packet=0 command=0 rescans=0`。C構成のCSW timeout 2件はStage 4と同一signature（`HCINT=0`、`QTD=0x06000200`＝`Known(0)`）で、cleanup有無に関わらず出る媒体側の性質と判断 |
| 70 | Stage 6: WRITE前cleanupをbuild時A/B | `TAB5_USB_WRITE_CLEANUP`へ`on`／`off`。READ側とは別enum・別変数にした。削除する順序も理由も違うため（READは再発行できる、WRITEは決して再送しない） |
| 71 | Stage 6実機結果と予防cleanup撤去 | **Go。**`read=disabled write=off`で全行PASS。Stage 5・6のGoを受けて予防cleanupをコードから撤去し、channel 0のcleanupは失敗path（`recover_failed_packet`）だけになった。`maintain_command_boundary`、`proactive_maintain`、`reads_since_resync`、方向別counter、2つのenvironment variableを削除。`usbcachefail`の[4/4]は狙いを失うので、cache拒否とflush timeoutの二重注入で「実失敗後のcleanup失敗はReset Recoveryを実行せずsessionを引退させる」を測る形へ作り直した |
