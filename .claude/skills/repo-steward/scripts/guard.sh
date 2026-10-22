#!/usr/bin/env bash
# Sourced by act.sh and board.sh, never run on its own: the one definition of what a credential
# looks like, so a text that reaches GitHub through any of the scripts goes through the same check.

# Succeeds when $1 contains something shaped like a credential.
credential_shaped() {
	printf '%s' "$1" | grep -Eq 'gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-ant-[A-Za-z0-9_-]{10,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----|xox[abpr]-[A-Za-z0-9-]{10,}'
}
