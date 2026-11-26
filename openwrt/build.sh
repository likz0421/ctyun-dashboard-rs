#!/bin/bash
# 多架构交叉编译脚本（musl 静态二进制）
# 依赖: rustup + zig (作为 musl C 链接器)
#   pip3 install ziglang
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf

set -e
cd "$(dirname "$0")/.."

OUT="openwrt/bin"
mkdir -p "$OUT"

# 定位 zig
ZIG=$(find "$(python3 -c 'import ziglang,os;print(os.path.dirname(ziglang.__file__))' 2>/dev/null)" -name zig -type f 2>/dev/null | head -1)
if [ -z "$ZIG" ]; then
  echo "未找到 zig，请先: pip3 install ziglang"; exit 1
fi

# 1) CC wrapper：把 cc-rs 的 rust 三元组 target 转成 zig 格式
WRAP="/tmp/zigcc-wrap"
mkdir -p "$(dirname "$WRAP")"
cat > "$WRAP" <<EOF
#!/bin/sh
out=""; target=""
for a in "\$@"; do
  case "\$a" in
    --target=*)
      t="\${a#--target=}"
      target=\$(printf '%s' "\$t" | sed \
        -e 's/x86_64-unknown-linux-musl/x86_64-linux-musl/' \
        -e 's/aarch64-unknown-linux-musl/aarch64-linux-musl/' \
        -e 's/armv7-unknown-linux-musleabihf/arm-linux-musleabihf/' \
        -e 's/arm-unknown-linux-musleabihf/arm-linux-musleabihf/')
      ;;
    *) out="\$out \\"\$a\\"" ;;
  esac
done
[ -n "\$target" ] && eval "exec \"$ZIG\" cc -target \"\$target\" \$out" || eval "exec \"$ZIG\" cc \$out"
EOF
chmod +x "$WRAP"

# 2) linker wrapper：zig cc 模式，避免 rustc self-contained crt 与 zig crt 重复
mkld() {
  printf '#!/bin/sh\nexec "%s" cc -target %s "$@"\n' "$ZIG" "$2" > "/tmp/zigld-$1"
  chmod +x "/tmp/zigld-$1"
}
# 注意：名字必须与下方 build() 的 name 参数一致（带 -musl 后缀）
mkld x86_64-musl  x86_64-linux-musl
mkld aarch64-musl aarch64-linux-musl
mkld armv7-musl   arm-linux-musleabihf

# 关键：禁用 rustc self-contained CRT，交给 zig
export RUSTFLAGS="-C link-self-contained=no"

build() {
  local target="$1" name="$2" ccvar="$3" ldvar="$4"
  echo "=== 编译 $name ($target) ==="
  export "$ccvar=$WRAP"
  export "$ldvar=/tmp/zigld-$name"
  cargo build --release --target "$target"
  cp "target/$target/release/ctyun-dashboard-rs" "$OUT/ctyun-dashboard-$name"
  md5sum "$OUT/ctyun-dashboard-$name" | awk '{print $1}' > "$OUT/ctyun-dashboard-$name.md5"
  file "$OUT/ctyun-dashboard-$name"
}

# 注意：name 必须与 install.sh 下载的文件名一致（带 -musl 后缀）
build x86_64-unknown-linux-musl       x86_64-musl  CC_x86_64_unknown_linux_musl       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER
build aarch64-unknown-linux-musl      aarch64-musl CC_aarch64_unknown_linux_musl      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER
build armv7-unknown-linux-musleabihf  armv7-musl   CC_armv7_unknown_linux_musleabihf  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER

echo "=== 完成，产物在 $OUT ==="
ls -lh "$OUT"
