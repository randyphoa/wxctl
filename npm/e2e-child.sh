#!/usr/bin/env bash
# Controllable stand-in for the native wxctl binary. e2e.sh installs this as the
# host sub-package's bin so it can assert the shim's spawn-and-forward fidelity:
# argv, stdio, exit code, the WXCTL_INSTALL_METHOD env marker, and signal death.
case "${1:-}" in
  --argv) shift; printf 'argv:%s\n' "$*" ;;
  --cat) cat ;;
  --exit) exit "${2:-0}" ;;
  --env) printf 'WXCTL_INSTALL_METHOD=%s\n' "${WXCTL_INSTALL_METHOD:-unset}" ;;
  --sleep) exec sleep "${2:-30}" ;;
  *) printf 'wxctl-fake %s\n' "${*:-}" ;;
esac
