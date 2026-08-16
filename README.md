# SWPU 校园网自动登录（Rust 版）

> ⚡ 针对 **西南石油大学** 校园网的自动登录客户端，Rust 原生实现。
> 解决学校更新网络后，官方客户端在**有线连接上"秒连秒断"**的问题。
> 单文件 EXE 仅 **~310 KB**，秒开，零依赖。

> 🔗 **与旧版的关系**：本仓库是原 Python 版（[swpu-srun-autologin](https://github.com/HCLEMINI/swpu-srun-autologin)）的 **Rust 重构版**，
> 功能等价、体积缩小 47 倍（14.7 MB → 310 KB）。旧仓库已归档，仅保留 Python 实现。

---

## 特点

| 特性 | 说明 |
|------|------|
| 🦀 原生实现 | 纯 `std::net` 手写 HTTP（门户为明文 http，零网络依赖）+ `windows-sys` 裸 Win32 GUI/托盘 |
| 📦 极小体积 | 单 EXE ~310 KB，启动 <50ms，无运行时依赖 |
| 🔁 自动登录/重连 | 后台周期探活，断连自动重连（失败退避，不刷屏） |
| 📶 WiFi 自连 | 断网时自动连接 `SWPU-EDU`（可在配置中修改），系统未自动连 WiFi 也能登录 |
| 🔌 关机注销 | 检测到系统关机/注销/重启时先向校园网退出登录，避免直接断电触发 reject、下次开机被拒连 WiFi |
| 📌 系统托盘 | 最小化缩回托盘；关闭按钮真正退出；`--minimized` 静默启动 |
| ⏯ 开机自启 | 任务计划「用户登录时」触发（`--install`/`--uninstall`，一次 UAC） |
| 🔒 加密验证 | XXTEA + Srun 自定义 base64 + HMAC-MD5 + SHA1，与官方前端**逐字节交叉验证** |

## 快速开始

```bash
# 1) 构建(需要 Rust 工具链; 或直接用 release/srun.exe)
cargo build --release          # 或用 build.bat

# 2) 配置: 复制 config.example.json 为 config.json, 填入学号密码
#    wifi_ssid: 断网时自动连接的 WiFi 名(留空 = 不自动连 WiFi, 纯有线场景可关闭)
#    (release\ 目录下运行同理)

# 3) 运行
srun.exe                # GUI(托盘)
srun.exe --minimized    # 静默启动进托盘
srun.exe --headless     # 无界面服务(后台常驻连网)
```

命令行：`--check` 查在线状态 / `--login` 登录 / `--logout` 注销 / `--selftest` 加密自检 / `--wifi [SSID]` 查询当前 WiFi 或尝试连接指定 WiFi。

> 📶 自动连 WiFi 需系统已保存过目标网络的配置文件（手动连接过一次即可），程序用 Windows 原生 Native Wifi API 连接，无需管理员权限。

## 线路对照（GUI「线路」下拉）

| 显示 | 后缀 | 说明 |
|------|------|------|
| 移动无线 | `@yd` | 校园 WiFi |
| 移动有线 | `@ydyx` | **插网线的真正有线网络** |
| 电信 | `@dxwx` | 电信线路 |
| 学生 | `@stu` / 教师 | `@tch` |

## 目录结构

```
├── src/            # main/crypto/http/srun/config/gui/autostart
├── release/        # 成品 srun.exe(运行时需同目录 config.json)
├── build.bat       # 一键构建
├── config.example.json
├── LICENSE / README.md
```

## 免责声明

仅供西南石油大学在校学生个人学习与日常联网使用；程序仅执行与浏览器等价的认证请求，**不绕过计费**。使用者自负责任。

## License

[MIT](LICENSE)
