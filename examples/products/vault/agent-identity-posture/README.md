# Agent identity posture (HashiCorp Vault)

Provision the intra-Vault posture that governs an AI agent's access to employee
data. One `config.yaml` wires all eight `vault_*` kinds through the `${vault.*}`
reference DAG.

## What it provisions

| Kind | Resource | Role |
|---|---|---|
| `vault_audit_device` | `file` | audit log of every request/response |
| `vault_auth_method` | `jwt` | JWT auth backend for IBM Verify |
| `vault_jwt_role` | `agent-role` | binds token audiences, claims, policies |
| `vault_policy` | `hr-basic-policy` | read basic employee data (from `resources/hr-basic-policy.hcl`) |
| `vault_policy` | `hr-admin-policy` | read all employee data (from `resources/hr-admin-policy.hcl`) |
| `vault_secret_engine` | `database` | PostgreSQL dynamic-credentials engine |
| `vault_database_role` | `hr-basic-reader` | SQL statements Vault runs to mint credentials |
| `vault_identity_group` | `hr-basic-group` | external group granting `hr-basic-policy` |
| `vault_group_alias` | `hr-basic` | maps the IdP group to the identity group |

The secrets engine sets `verify_connection: false`, so it applies without a
reachable database. The DB password is read from `${env:EMPLOYEE_DB_PASSWORD}`;
no secret ships in the config.

## Prerequisites

- A reachable, unsealed Vault and a profile with a `vault` block (`auth_type:
  vault_token`, token from `${env:VAULT_TOKEN}`, optional `namespace`).
- Export a placeholder DB password before every command (required by
  `${env:EMPLOYEE_DB_PASSWORD}`; the value is unused because
  `verify_connection: false`):

  ```bash
  export EMPLOYEE_DB_PASSWORD=placeholder
  ```

## Run it

```bash
wxctl -p <vault-profile> plan    -f config.yaml   # nine resources to create
wxctl -p <vault-profile> apply   -f config.yaml
wxctl -p <vault-profile> plan    -f config.yaml   # No changes (idempotent)
wxctl -p <vault-profile> destroy -f config.yaml
```

Expected: `apply` creates all nine resources; the immediate re-`plan` reports no
changes; `destroy` deletes them and a following `plan` shows all nine to create.
