#!/usr/bin/env bash
# Regenerates crates/views/tests/golden/b from the reference app.
#
# Runs in a working copy of reference/ (REF, default target/views-b-ref) with its gems
# installed; see README in this directory's parent or NOTES.md ("views") for the setup.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
REF=${REF:-$ROOT/target/views-b-ref}
export OUT=${OUT:-$ROOT/crates/views/tests/golden/b}
export RAILS_ENV=production DISABLE_DATABASE_ENVIRONMENT_CHECK=1 DISABLE_SSL=1 RAILS_LOG_LEVEL=error
export SECRET_KEY_BASE=${SECRET_KEY_BASE:-views-b-secret-key-base-0123456789abcdef0123456789abcdef}
cd "$REF"
bin/rails db:prepare db:fixtures:load
bin/rails runner "$ROOT/reference-tools/views/b/setup.rb"
bin/rails runner "$ROOT/reference-tools/views/b/goldens.rb"
