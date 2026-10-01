#!/bin/sh
# Loads the AppArmor profile that lets CEF's sandbox use user namespaces (DESIGN §23.2).
set -e
if [ "$1" = configure ] && command -v apparmor_parser >/dev/null 2>&1 && [ -d /sys/kernel/security/apparmor ]; then
  apparmor_parser -r -T -W /etc/apparmor.d/jess-notes || true
fi
