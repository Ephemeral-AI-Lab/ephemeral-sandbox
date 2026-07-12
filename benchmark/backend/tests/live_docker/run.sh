#!/bin/sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
web_root=$(CDPATH= cd -- "$here/../../../web" && pwd)

exec npm --prefix "$web_root" run test:real-backend
