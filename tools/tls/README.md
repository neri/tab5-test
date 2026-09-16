# TLS fixture keys

> 索引: [`../../DESIGN.md`](../../DESIGN.md) ／
> [`../../docs/plans/archive/TLS_PLAN.md`](../../docs/plans/archive/TLS_PLAN.md)

`browser_fixture_server.py --tls-port`が提示する証明書と鍵です。
**秘密鍵がリポジトリに入っています。秘匿価値はありません。**

安全なのは、対応するpinが`tls-fixture-pins` featureを付けたビルドにしか
入らないからです。通常releaseにこれらの鍵を信用する経路はなく、
`tools/pins/generate.py --check-release`がELFを走査してそれを確かめます。
この前提が崩れた瞬間に安全でなくなるので、**これらの鍵を通常releaseで
信用させないでください**。

| ファイル | 用途 |
| --- | --- |
| `fixture-current.*` | 現用鍵。`fixture_pins.txt`に登録済み → `TLS PINNED`になる |
| `fixture-next.*` | rotation先の鍵。同じく登録済み → `TLS PINNED`になる |
| `fixture-other.*` | 登録していない鍵 → `tls-pin`で失敗しなければならない |

```sh
python3 tools/browser_fixture_server.py --tls-key-name current
python3 tools/browser_fixture_server.py --tls-key-name next
python3 tools/browser_fixture_server.py --tls-key-name other
```

いずれもP-256のself-signed、有効期間20年です。証明書の内容（CN、SAN、
有効期限）はpinningの判断材料に**なりません**。pinはleafの
`SubjectPublicKeyInfo`のSHA-256だけを見ます。

再生成する場合は`tools/pins/fixture_pins.txt`のpinも取り直して
`tools/pins/generate.py`を走らせる必要があります。

```sh
openssl ecparam -name prime256v1 -genkey -noout -out fixture-current.key
openssl req -new -x509 -key fixture-current.key -sha256 -days 7300 \
  -subj "/CN=tls-fixture.invalid" \
  -addext "subjectAltName=DNS:tls-fixture.invalid,DNS:localhost,IP:127.0.0.1" \
  -out fixture-current.crt
tools/pins/generate.py --from-cert tools/tls/fixture-current.crt
```
