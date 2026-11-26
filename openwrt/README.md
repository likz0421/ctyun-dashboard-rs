# 天翼云电脑控制中心 · OpenWrt / LuCI

把 Rust 版天翼云电脑控制面板一键部署到 OpenWrt 路由器。安装脚本自动识别设备与网络环境并适配，LuCI 内即可完成日常操作。

## 特性

- **设备自动识别**：CPU 架构、设备型号、内存、存储余量、系统版本、包管理器（opkg/apk），低资源自动告警
- **网络自动探测**：LAN IP、默认网关、DNS、外网连通性（GitHub 可达性）、端口占用；8571 被占自动顺延到空闲端口；GitHub 不通自动切镜像下载源
- **下载自适应**：Release → raw 分支 → 镜像源多级回退，MD5 完整性校验，fw3/fw4 防火墙自动兼容
- **procd 服务**：开机自启、异常自动拉起、UCI 配置
- **LuCI 日常操作台**：「服务 → 天翼云电脑 → 运行状态」内直接登录、管理账号、执行签到/挂机/AI 对话任务、电源开关机、查看积分任务进度、实时日志、通知与配置管理
- **独立面板**：扫码登录、积分商城、短信绑定、远程桌面等高级功能保留在完整 Web 面板
- **一键卸载**：精准清理，不误删用户其他防火墙规则

## 一键安装

**推荐（自动识别系统+架构，一条命令通用）**——在路由器 SSH 中执行：

```bash
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/install.sh)"
```

若确定是 OpenWrt，也可直接用本子脚本（`-4` 规避 IPv6 解析卡死）：

```bash
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/openwrt/install.sh)"
```

脚本会自动打印识别到的设备与网络信息，例如：

```
[ctyun] 设备型号   : Xiaomi Mi Router AX6000
[ctyun] CPU 架构   : aarch64 -> aarch64-musl
[ctyun] 内存       : 512 MB
[ctyun] 网络       : LAN 192.168.1.1 / 网关 192.168.1.1 / DNS 114.114.114.114
[ctyun] 服务端口   : 8571（可用）
```

安装完成后：

- LuCI 日常操作台：**服务 → 天翼云电脑 → 运行状态**
- 独立完整面板：`http://路由器IP:8571`
- 默认账号：`admin` / `admin123`（**首次登录后请立即修改**）

## LuCI 能做什么（日常操作齐全）

进入「运行状态」页后用面板账号登录，token 仅存当前浏览器会话。所有操作直接在 LuCI 内完成：

| 标签 | 功能 |
|---|---|
| 总览 | 账号数 / 在线保活 / 今日积分 / 异常账号 KPI，各账号与云电脑实时状态 |
| 账号 | 添加账号（手机号+密码）、启用/停用开关、改名、删除 |
| 任务 | 签到认领、挂机 1 小时、AI 对话、一键触发全部每日任务、重启保活；实时查看积分任务进度 |
| 电源 | 按账号+云电脑下发开机 / 唤醒 / 重启 / 关机 |
| 日志 | 最近 200 条日志、8 秒自动刷新、一键清空 |
| 设置 | 通知渠道配置与测试、配置导出/导入、跳转完整面板 |

> 扫码登录、积分商城兑换、短信绑定、Web 远程桌面等需在独立完整面板操作，LuCI 页内有直达入口。

## 支持的架构

| 设备架构 (`uname -m`) | 二进制目标 | 常见设备 |
|---|---|---|
| `x86_64` / `amd64` | `x86_64-unknown-linux-musl` | x86 软路由、NUC、虚拟机 |
| `aarch64` / `arm64` | `aarch64-unknown-linux-musl` | ARM64 路由器、树莓派 4、AX6000 |
| `armv7l` / `armhf` | `armv7-unknown-linux-musleabihf` | 老款 ARM 路由器 |

> 均为 **musl 静态链接**，不依赖路由器上的 glibc 版本，兼容性好。
> MIPS 老设备请用 [从源码编译](#从源码编译) 方式。

## 管理命令

```bash
/etc/init.d/ctyun-dashboard start|stop|restart   # 服务管理
ctyun-dashboard-ctl status                        # 运行状态 + 健康检查
ctyun-dashboard-ctl port 9000                     # 修改端口
ctyun-dashboard-ctl open                          # 打印面板地址
ctyun-dashboard-ctl logs                          # 查看日志
```

## 一键卸载

```bash
sh -c "$(wget -4 -qO- https://raw.githubusercontent.com/likz0421/ctyun-dashboard-rs/main/uninstall.sh)"
```

## 从源码编译

如需 MIPS 等非常见架构，可用 OpenWrt SDK 或 zig 交叉编译：

```bash
rustup target add aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
bash openwrt/build.sh
```

## 端口与防火墙

默认端口 `8571`，脚本自动放行 **LAN 区** TCP 8571（fw3 写 uci 规则，fw4 写 nftables.d 规则）。端口被占用时自动顺延。如需手动改端口：

```bash
uci set ctyun_dashboard.main.port=9000
uci commit ctyun_dashboard
/etc/init.d/ctyun-dashboard restart
```

> 安全提示：本服务管理天翼云账号凭据，建议**仅 LAN 访问**，不要暴露到公网。LuCI 的 API 代理已拦截远程桌面外部请求代理等危险接口，仅开放业务接口。

## 常见问题

**Q: 安装后打不开面板？**
A: 先 `ctyun-dashboard-ctl status` 看服务是否在跑；再看安装时打印的实际端口（可能被占用顺延）；检查防火墙规则。

**Q: LuCI 菜单没出现？**
A: 删缓存 `rm -rf /tmp/luci-*`，强制刷新浏览器（Ctrl+F5）。

**Q: LuCI 操作台点按钮报"未登录"？**
A: 刷新页面重新登录，token 有效期有限；或在「总览」标签确认服务健康。

**Q: 提示架构不支持？**
A: 用 `uname -m` 查看实际架构，到 [Releases](https://github.com/likz0421/ctyun-dashboard-rs/releases) 手动下载或从源码编译。

**Q: 账号凭据存在哪里？**
A: `/var/lib/ctyun-dashboard`，AES-256-GCM 加密落盘，密钥为隐藏文件 `.master.key`。卸载时默认保留，可手动删除。
