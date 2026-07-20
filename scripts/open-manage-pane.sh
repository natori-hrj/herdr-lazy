#!/bin/sh
# Open the manage pane.
#
# Why an action cannot just run the TUI directly: herdr runs plugin *actions* without a
# terminal, so `herdr-lazy ui` there dies with "Device not configured (os error 6)" the moment
# it asks for raw mode. Only a *pane* gets a PTY. So the action's job is to ask herdr to open
# the pane — which is also how the other TUI plugins in the ecosystem do it.
#
# This indirection is what makes a keybinding work:
#   [[keys.command]] type = "plugin_action", command = "herdr-lazy.manage"

set -eu

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
    --plugin herdr-lazy \
    --entrypoint manage \
    --focus
