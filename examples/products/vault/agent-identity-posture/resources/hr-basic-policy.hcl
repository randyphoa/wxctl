# HR Basic Policy — access to non-sensitive employee information only.
# Grants read on the hr-basic-reader dynamic DB credential and self-token ops.

path "database/creds/hr-basic-reader" {
  capabilities = ["read"]
}

path "auth/token/lookup-self" {
  capabilities = ["read"]
}

path "auth/token/renew-self" {
  capabilities = ["update"]
}

path "auth/token/revoke-self" {
  capabilities = ["update"]
}

path "database/roles" {
  capabilities = ["list"]
}

path "database/config/employee-db" {
  capabilities = ["read"]
}
