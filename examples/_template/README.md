# <use-case-name> — <one-line summary>

> One sentence on **why this use case is worth showing** — the moment it lands, the
> audience, the outcome it proves.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> <the sentence wxctl composes into config.yaml>

## What it provisions

`config.yaml` declares:

- `kind: <a>` — <what it is and why>
- `kind: <b>` — <…>, wired to `<a>` via `${<a>.<ref>}`

Plus `kind: test` checks that exercise the deployed use case.

## Run it

Credential-free: secrets are `${env:VAR}` placeholders. Configure a profile in
`~/.wxctl/config.json`, export the vars below, then:

```bash
# export ANY_REQUIRED_ENV_VAR=...
wxctl plan    -f config.yaml
wxctl apply   -f config.yaml
wxctl test    -f config.yaml
wxctl destroy -f config.yaml
```

## Expected output

- <what `apply` creates>
- <what `test` confirms>
