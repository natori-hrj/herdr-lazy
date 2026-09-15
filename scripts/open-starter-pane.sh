#!/bin/sh
# Open the workspace starter pane.
#
# Herdr actions do not get a PTY, so the action asks Herdr to open the real pane. The pane
# then owns the terminal and runs the confirmation-first starter workflow.

set -eu

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
    --plugin herdr-lazy \
    --entrypoint starter \
    --focus
