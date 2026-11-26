# 天翼云电脑控制中心 · Linux / systemd

把 Rust 版天翼云电脑控制面板部署到普通 Linux 服务器（Ubuntu / Debian / CentOS / Rocky 等 systemd 发行版）。注册为 systemd 服务，开机自启，单文件二进制。

> 前端资源在编译期嵌入二进制，**无需外部 `static/` 目录**，部署只有一个可执行文件。

## 一键安装

**推荐（自动识别系统+架构）**——在任意目录执行，脚本自动判断是 OpenWrt 还是 Linux 并拉取对应脚本：

```bash
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
```

若确定是普通 Linux，也可直接用本子脚本：

```bash
cd ctyun-dashboard-rs/linux
bash install.sh
```

也可指定端口或用本地二进制：

```bash
CTYUN_PORT=9000 bash install.sh
CTYUN_BIN=/path/to/ctyun-dashboard-rs bash install.sh
```

脚本会自动：识别架构（x86_64 / aarch64 / armv7）、选择二进制来源（本地编译产物优先，否则从 GitHub Release 下载 musl 静态版并校验 MD5）、检测端口占用（占用则顺延）、创建专用系统用户、写入 systemd 单元、启动并设为开机自启。

安装后：

- 面板地址：`http://服务器IP:8571`（实际端口以安装输出为准）
- 默认账号：`admin` / `admin123`（**首次登录后立即修改**）

## 管理命令

```bash
systemctl status  ctyun-dashboard      # 运行状态
systemctl restart ctyun-dashboard      # 重启
systemctl stop    ctyun-dashboard      # 停止
journalctl -u ctyun-dashboard -f       # 实时日志
```

## 配置

运行配置写在 `/etc/ctyun-dashboard/ctyun-dashboard.env`：

```ini
CTYUN_PORT=8571
CTYUN_DATA_DIR=/var/lib/ctyun-dashboard
RUST_LOG=info
```

修改后执行 `systemctl restart ctyun-dashboard` 生效。

- 端口：也可用环境变量 `CTYUN_PORT` 覆盖（优先级高于配置文件内旧值）
- 数据目录：`CTYUN_DATA_DIR`，含 `app_config.json`、AES 密钥 `.master.key`、日志 `logs/`
- 日志级别：`RUST_LOG`（trace/debug/info/warn/error）

## 一键卸载

```bash
# 自动识别系统：
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/uninstall.sh)"

# 仅 Linux 子脚本：
cd ctyun-dashboard-rs/linux
bash uninstall.sh                 # 默认保留数据目录
KEEP_DATA=0 bash uninstall.sh      # 连数据一起彻底删除
```

## 从源码自行编译

```bash
cd ctyun-dashboard-rs
cargo build --release
CTYUN_BIN="$PWD/target/release/ctyun-dashboard-rs" bash linux/install.sh
```

> 安全提示：本服务管理天翼云账号凭据，默认监听 `0.0.0.0`。建议用防火墙限制来源 IP，或置于反向代理之后，不要直接暴露到公网。
