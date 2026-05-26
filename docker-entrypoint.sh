#!/bin/sh
set -e

# Allow git to operate on directories owned by a different uid (e.g. host-mounted repos).
# This must be written to the gitconfig file because git checks ownership before
# processing GIT_CONFIG_* environment variables.
git config --global --add safe.directory '*'

if [ -d "/tmp/ssh_keys" ] && [ "$(ls -A /tmp/ssh_keys 2>/dev/null)" ]; then
    mkdir -p /root/.ssh
    cp -R /tmp/ssh_keys/* /root/.ssh/
    chown -R root:root /root/.ssh
    chmod 700 /root/.ssh
    find /root/.ssh -type f -exec chmod 600 {} +
fi

exec "$@"
