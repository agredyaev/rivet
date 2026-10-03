#!/usr/bin/env bash

if [[ -t 1 && ${NO_COLOR+x} != x ]]; then
  rivet_cyan=$'\033[36m'
  rivet_green=$'\033[32m'
  rivet_yellow=$'\033[33m'
  rivet_red=$'\033[31m'
  rivet_bold=$'\033[1m'
  rivet_reset=$'\033[0m'
else
  rivet_cyan=''
  rivet_green=''
  rivet_yellow=''
  rivet_red=''
  rivet_bold=''
  rivet_reset=''
fi

rivet_header() { printf '\n%bRivet setup%b\n' "$rivet_bold$rivet_cyan" "$rivet_reset"; }
rivet_ok() { printf '  %b✓%b %s\n' "$rivet_green" "$rivet_reset" "$1"; }
rivet_note() { printf '  %b%s%b\n' "$rivet_yellow" "$1" "$rivet_reset"; }
rivet_error() { printf '  %b✗%b %s\n' "$rivet_red" "$rivet_reset" "$1" >&2; }
