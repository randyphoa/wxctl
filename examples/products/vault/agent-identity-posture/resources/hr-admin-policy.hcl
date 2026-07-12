# HR Admin Policy — full access to all employee information, including salary.
# Grants read on the hr-admin-reader dynamic DB credential and self-token ops.

path "database/creds/hr-admin-reader" {
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

path "database/roles/*" {
  capabilities = ["read"]
}

path "database/config/employee-db" {
  capabilities = ["read"]
}

path "database/static-roles/*" {
  capabilities = ["read"]
}
