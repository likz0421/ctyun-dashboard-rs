# 云电脑控制中心 · Rust Edition

自用精简版。基于天翼云电脑协议的 Rust 重写，**仅保留天翼云电脑，不含移动云**。Rust (axum) 后端 + 毛玻璃前端，单文件二进制。

前端资源在编译期嵌入二进制，部署无需外部 `static/` 目录；默认端口 `8571`，默认账号 `admin` / `admin123`。

## 一键管理（自动识别系统 + 架构）

一条命令进入交互菜单，安装 / 卸载 / 看状态都在这里选，不用记多个地址：

```bash
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
# 没有 wget 时用 curl：
sh -c "$(curl -fsSL https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
```

运行后弹出菜单：

```
  1) 安装 / 重装（保留数据）
  2) 卸载（保留账号与数据）
  3) 彻底卸载（同时删除数据）
  4) 查看服务状态
  5) 退出
```

非交互 / 自动化场景用管道带参数，跳过菜单（`sh -s` 的 `s` 是占位符，别省）：

```bash
URL=https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh
wget -4 -qO- "$URL" | sh -s install        # 安装/重装（保留数据）
wget -4 -qO- "$URL" | sh -s uninstall      # 卸载（保留数据）
wget -4 -qO- "$URL" | sh -s uninstall-all  # 彻底卸载（含数据）
wget -4 -qO- "$URL" | sh -s status         # 查看状态
```

网络方面：`-4` 规避 IPv6 解析卡死；脚本对 wget/curl 逐级回退，国内访问 GitHub 不畅时**自动切换 ghfast / ghproxy / gh-proxy / jsDelivr 镜像**（二进制下载与脚本拉取均适用）。若全部失败，可手动挂代理：

```bash
export https_proxy=http://代理主机:端口
sh -c "$(wget -4 -qO- .../install.sh)"
```

> 旧的 `uninstall.sh` 一键地址仍然可用（等价于菜单里的「卸载，保留数据」；`KEEP_DATA=0` 则彻底卸载）。

## 代码结构

源码（两个系统共用）：

```
src/          Rust 后端（axum / WebSocket 保活 / 任务调度 / REDQ 协议）
static/       毛玻璃前端（原生 HTML/CSS/JS，编译期嵌入二进制）
Cargo.toml
```

部署入口：

```
install.sh / uninstall.sh   统一一键引导（自动识别系统+架构）→ 拉取下列子脚本
linux/        普通 Linux 服务器 · systemd 一键装/卸   → 见 linux/README.md
openwrt/      OpenWrt 路由器 · LuCI 插件一键装/卸      → 见 openwrt/README.md
.github/      GitHub Actions（Release 交叉编译 musl 二进制）
```

| 目标系统 | 直接安装子脚本 | 说明文档 |
|---|---|---|
| 自动识别（推荐） | 根目录 `install.sh` | 上文一键命令 |
| Linux 服务器（systemd） | `linux/install.sh` | [linux/README.md](linux/README.md) |
| OpenWrt 路由器（LuCI） | `openwrt/install.sh` | [openwrt/README.md](openwrt/README.md) |

Linux 也可直接跑裸二进制（适合临时验证）：

```bash
cargo build --release
CTYUN_PORT=8571 ./target/release/ctyun-dashboard-rs
# 浏览器打开 http://127.0.0.1:8571
```

## 从源码编译

```bash
cargo build --release                 # 本机 glibc 版，约 8MB
bash openwrt/build.sh                 # 多架构 musl 静态版（需 rustup + zig）
```

## 许可

[MIT](LICENSE) — 仅供学习与个人使用，请遵守天翼云相关服务条款。
