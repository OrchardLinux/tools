#!/bin/bash
# Type-checks and lints the installer's crates for Linux (x86_64 and arm64)
# from a Mac, with zig as the C compiler for dependencies' build scripts.
# Nothing here runs the result; Linux behaviour still needs a Linux machine.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v zig >/dev/null || { echo "zig is needed (glu install zig)" >&2; exit 1; }
wrappers=$(mktemp -d)
trap 'rm -rf "$wrappers"' EXIT
for arch in x86_64 aarch64; do
  cat > "$wrappers/cc-$arch" <<WRAP
#!/bin/sh
# cc passes its own --target, and sha2's assembly a -march zig spells -mcpu.
skip=0
for a in "\$@"; do
  shift
  if [ \$skip = 1 ]; then skip=0; continue; fi
  case "\$a" in
    --target=*) continue ;;
    -target) skip=1; continue ;;
    -march=armv8-a+crypto) set -- "\$@" -mcpu=generic+aes+sha2; continue ;;
  esac
  set -- "\$@" "\$a"
done
exec zig cc -target $arch-linux-gnu "\$@"
WRAP
  chmod +x "$wrappers/cc-$arch"
done
printf '#!/bin/sh\nexec zig ar "$@"\n' > "$wrappers/ar" && chmod +x "$wrappers/ar"
crates=(-p orchard-runtime -p orchard-flow -p orchard-usb -p orchard-boards -p orchard-formats -p orchard-cache)
for arch in x86_64 aarch64; do
  echo "== $arch-unknown-linux-gnu"
  var=${arch}_unknown_linux_gnu
  env "CC_$var=$wrappers/cc-$arch" "AR_$var=$wrappers/ar" \
    cargo clippy --offline --all-targets --target "$arch-unknown-linux-gnu" "${crates[@]}" -- -D warnings
done
