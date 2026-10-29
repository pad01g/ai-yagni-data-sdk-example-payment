# ai-yagni-data-sdk-example-payment

EC の例（[ai-yagni-data-sdk-example-backend-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-rust)）で使う、模擬の決済サービスです。HTTP + JSON で話します。

このサービスを呼ぶのは [backend-proxy](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-proxy) だけです。
backend の compose では、このサービスを proxy と同じ内部ネットワークにだけ置きます。そのため backend-rust からは届きません。

## API

| メソッド | パス | 中身 |
|---|---|---|
| POST | `/charges` | `{"idempotency_key", "amount", "currency", "card_token"}` を請求する。承認なら 201、拒否なら 402 |
| GET | `/charges/{id}` | 決済を返す |
| GET | `/health` | `ok` |

- 同じ冪等キーには同じ結果を返します。中身の違う依頼に同じキーを使うと 409 です。
- 状態はメモリに置くので、再起動すると消えます。

カードのトークンごとの結果は次のとおりです。

| トークン | 結果 |
|---|---|
| `tok_visa` | いつも承認する |
| `tok_declined` | いつも拒否する |
| `tok_limited` | 利用枠 10,000 まで承認する |
| それ以外 | 拒否する |

## Verus

[`verified/`](verified/src/lib.rs) は、利用枠の判定 `authorize` を証明しています。

- 承認した額は 0 より大きい。
- 足し算はあふれない。
- 新しい使った額は枠を超えない。
- 拒否するのは、額が 0 のときか、枠を超えるときだけ。

## 動かし方（ホストには docker 以外何も入れない）

```sh
make test     # テスト
make lint     # rustfmt と clippy
make verify   # Verus（amd64 のイメージ）
make run      # http://localhost:8081 で起動する
```

## ライセンス

MIT
