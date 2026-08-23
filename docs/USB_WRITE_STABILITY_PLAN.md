# USB MSC書き込みの不安定さ 調査記録

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は調査記録と対策計画です。現在の実装仕様は現状文書
> （[`STORAGE.md`](STORAGE.md)、[`USB.md`](USB.md)）とコードを優先してください。
> 読み出しまでの実装計画は[`USB_MSC_PLAN.md`](USB_MSC_PLAN.md)です。

## 状態: **MSC／HID安定化完了**（第24版のHigh-Speedハブ複合回帰まで実機確認済み）

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
| **このUSBメモリはSYNCHRONIZE CACHE(10)非対応** | sense key 5／ASC `0x24`。安価なコントローラは非対応を`0x20`ではなく`0x24`で返す |

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
| 成功16 READごとの予防的BOT再同期 | 33〜52回の連続READ後にBulkとEP0が無応答になる前にBOT境界を再確立 | `proactive BOT resync before READ(10)` |

詳細は[`USB.md`](USB.md)の「電力問題の切り分け」「転送失敗の巻き添え」
「periodic HIDの停止検出」「転送失敗からの自動復帰」を参照。

## 診断ログの読み方

- `HCINT`が`0x02`（ChHltdのみ）で`bytes transferred=0` → **相手が応答していない**。
- `HCINT`にXactErr（bit 7）→ トランザクションが壊れている。第19〜23版で調査した
  Split HID障害はこの形だった。
- `USB BOT: ... during <phase>` → `CBW`／`data OUT`／`data IN`／`CSW`のどれで落ちたか。
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
