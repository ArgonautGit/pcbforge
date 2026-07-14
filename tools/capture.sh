#!/usr/bin/env bash
# DRV-1 — B4 USB capture recorder.
#
# Records ONE experiment per file from a usbmon interface into
# captures/<NN>-<slug>.pcapng and appends a row to captures/MANIFEST.csv.
# Passive: this only sniffs; LightBurn drives the laser. See
# docs/capture-plan.md for the full procedure and the experiment matrix.
#
# Authored without a USB stack to verify against (cloud container); reconcile
# the dumpcap/usbmon invocation with `man usbmon` / `man dumpcap` on the real
# machine before trusting it (docs/capture-plan.md §2 is the acceptance test).

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: tools/capture.sh --interface usbmonN --dev ADDR --exp NN [options]

Required:
  --interface NAME   usbmon capture interface, e.g. usbmon3 (see `tshark -D`)
  --dev ADDR         USB device address of the B4 this session (from lsusb)
  --exp NN           experiment number 00..13 (or 99 for the tooling dry-run)

Options:
  --desc TEXT        description / exact recipe (manifest + filename slug)
  --baseline NN      experiment this one varies from (default: - )
  --variable NAME    the single parameter changed vs baseline (default: - )
  --params TEXT      exact recipe string (power/speed/freq/interval/passes/angle)
  --seconds N        auto-stop after N seconds; omit to stop with Ctrl-C
  --dry-target       mark the row as a non-B4 tooling test (exp 99)
  -h, --help         this help

Examples:
  tools/capture.sh --interface usbmon3 --dev 11 --exp 03 --seconds 20 \
    --params "20% 500mm/s 30kHz 2ns" --desc "10mm line, Line mode"
  tools/capture.sh --interface usbmon3 --dev 11 --exp 04 --baseline 03 \
    --variable power --params "22% 500mm/s 30kHz 2ns" --desc "line, power +10%"
EOF
}

# --- defaults ---
interface="" dev="" exp="" desc="" baseline="-" variable="-" params="-"
seconds="" dry_target=0

while [ $# -gt 0 ]; do
  case "$1" in
    --interface) interface="$2"; shift 2 ;;
    --dev)       dev="$2"; shift 2 ;;
    --exp)       exp="$2"; shift 2 ;;
    --desc)      desc="$2"; shift 2 ;;
    --baseline)  baseline="$2"; shift 2 ;;
    --variable)  variable="$2"; shift 2 ;;
    --params)    params="$2"; shift 2 ;;
    --seconds)   seconds="$2"; shift 2 ;;
    --dry-target) dry_target=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    *) echo "capture.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# --- validate ---
for req in interface dev exp; do
  if [ -z "${!req}" ]; then
    echo "capture.sh: --$req is required" >&2; usage >&2; exit 2
  fi
done
if ! printf '%s' "$exp" | grep -Eq '^[0-9]{2}$'; then
  echo "capture.sh: --exp must be two digits (00..13, or 99)" >&2; exit 2
fi

# Locate the repo's captures/ dir relative to this script (tools/..).
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
captures_dir="$repo_root/captures"
manifest="$captures_dir/MANIFEST.csv"
mkdir -p "$captures_dir"

# --- pick the recorder: prefer dumpcap (lighter), fall back to tshark ---
if command -v dumpcap >/dev/null 2>&1; then
  recorder=dumpcap
elif command -v tshark >/dev/null 2>&1; then
  recorder=tshark
else
  echo "capture.sh: neither dumpcap nor tshark found on PATH" >&2
  echo "  install wireshark/tshark and load usbmon (see docs/capture-plan.md)" >&2
  exit 3
fi

# --- filename: NN-slug.pcapng, refuse to clobber an existing experiment ---
slug="$(printf '%s' "${desc:-exp}" \
  | tr '[:upper:]' '[:lower:]' \
  | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//' \
  | cut -c1-40)"
[ -n "$slug" ] || slug="exp"
outfile="$captures_dir/${exp}-${slug}.pcapng"

if compgen -G "$captures_dir/${exp}-*.pcapng" >/dev/null; then
  echo "capture.sh: experiment $exp already has a capture:" >&2
  ls "$captures_dir/${exp}-"*.pcapng >&2
  echo "  remove/rename it before re-recording (one experiment per file)." >&2
  exit 4
fi

# --- capture ---
# usbmon linktype: capture the whole bus interface; device-address filtering is
# done in decode (DRV-2). A display filter here would drop URB context.
duration_args=()
if [ -n "$seconds" ]; then
  duration_args=(-a "duration:$seconds")
  echo "capture.sh: recording $interface for ${seconds}s -> $outfile"
else
  echo "capture.sh: recording $interface -> $outfile (press Ctrl-C to stop)"
fi
echo "capture.sh: start LightBurn action NOW (give it a second of lead-in)."

# dumpcap and tshark share -i/-w/-a flags. Errors (perms, missing iface) surface
# directly. SIGINT (Ctrl-C) is a normal stop for an untimed capture.
set +e
"$recorder" -i "$interface" -w "$outfile" "${duration_args[@]}"
rc=$?
set -e
# 0 = clean stop; tshark/dumpcap also exit 0 on SIGINT duration end. A nonzero
# code with no file is a real failure.
if [ ! -s "$outfile" ]; then
  echo "capture.sh: no data captured (recorder exit $rc). Interface/permission?" >&2
  echo "  see docs/capture-plan.md §7 (troubleshooting)." >&2
  exit 5
fi

# --- manifest row ---
if [ ! -f "$manifest" ]; then
  echo "exp,file,date,interface,dev_addr,baseline,variable,params,desc,sha256" > "$manifest"
fi
# CSV-escape any field that contains a comma or quote.
csv() { printf '%s' "$1" | sed 's/"/""/g; s/.*/"&"/'; }
date_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
rel="${outfile#"$captures_dir/"}"
tag="$dev"; [ "$dry_target" -eq 1 ] && tag="${dev} (non-B4 dry-run)"
{
  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$(csv "$exp")" "$(csv "$rel")" "$(csv "$date_iso")" "$(csv "$interface")" \
    "$(csv "$tag")" "$(csv "$baseline")" "$(csv "$variable")" "$(csv "$params")" \
    "$(csv "$desc")" "$(csv "-")"
} >> "$manifest"

echo "capture.sh: wrote $rel ($(wc -c < "$outfile") bytes) and manifest row."
echo "capture.sh: record the exact LightBurn params in RUNLOG.md too."
