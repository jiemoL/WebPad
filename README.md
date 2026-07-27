# WebPad / 虚拟手柄模拟器

> A WebSocket-based virtual gamepad simulator built with Rust. Turn your phone/tablet into a virtual Xbox 360 controller.
>
> 基于 Rust 的 WebSocket 虚拟游戏手柄模拟器。将你的手机/平板变成虚拟 Xbox 360 控制器。

---

## Features / 功能特性

- 🎮 **Virtual Xbox 360 Controller** — Simulates Xbox 360 controller via ViGEmBus driver (Windows only)
  - **虚拟 Xbox 360 手柄** — 通过 ViGEmBus 驱动模拟 Xbox 360 手柄（仅 Windows）
- 📱 **Mobile-Friendly Web Interface** — Touchscreen gamepad with joysticks, triggers, and buttons
  - **移动端友好的 Web 界面** — 触屏手柄，支持摇杆、扳机和按键
- 🔐 **Secure Authentication** — Argon2id password hashing with exponential backoff against brute force
  - **安全认证** — Argon2id 密码哈希，指数退抵御暴力破解
- 🔒 **TLS by Default** — HTTPS/WSS encryption, HTTP auto-redirects to HTTPS
  - **默认启用 TLS** — HTTPS/WSS 加密传输，HTTP 自动重定向到 HTTPS
- ❤️ **Heartbeat Detection** — Automatic disconnection for inactive clients
  - **心跳检测** — 自动断开不活跃的客户端连接
- 🌐 **UPnP Port Mapping** — Automatic port forwarding for external access
  - **UPnP 端口映射** — 自动端口转发，支持外网访问
- 📊 **Connection Limiting** — Protection against DoS attacks
  - **连接数限制** — 防护 DoS 攻击

---

## Tech Stack / 技术栈

| Component / 组件 | Technology / 技术 |
|-------------------|-------------------|
| Language / 语言 | Rust (Edition 2021) |
| Web Framework / Web 框架 | Axum 0.8 |
| Web Server / Web 服务器 | axum-server 0.7 (TLS) |
| Virtual Gamepad / 虚拟手柄 | ViGEmClient 0.1.4 |
| TLS | rustls + rcgen |
| Password Hashing / 密码哈希 | Argon2id |
| Serialization / 序列化 | serde + serde_json |

---

## Prerequisites / 前置条件

- **Windows 10/11** — Full gamepad emulation. Any OS works for Web server only.
  - **Windows 10/11** — 完整手柄模拟功能。其他系统仅支持 Web 服务。
- **Rust** stable toolchain
  - **Rust** 稳定版工具链
- **ViGEmBus Driver** — Required for gamepad emulation on Windows
  - **ViGEmBus 驱动** — Windows 手柄模拟必需
  - Download / 下载: https://github.com/ViGEm/ViGEmBus/releases

---

## Installation / 安装

```bash
# Clone the repository
# 克隆仓库
git clone https://github.com/jiemoL/WebPad.git
cd WebPad

# Build
# 编译构建
cargo build --release

# Run
# 运行
cargo run --release
```

---

## Usage / 使用说明

1. Run the server:
   ```bash
   cargo run --release
   ```
   启动服务端。

2. Note the password shown in the console output.
   - 记录控制台输出的连接密码。

3. Open your mobile browser and navigate to:
   ```
   https://<your-computer-ip>:8443/
   ```
   在手机浏览器中打开上述地址。

4. Enter the password when prompted.
   - 输入连接密码进行认证。

5. Start using the virtual gamepad!
   - 开始使用虚拟手柄！

---

## Configuration / 配置

The configuration file `webpad.toml` is created in the same directory as the executable.
配置文件 `webpad.toml` 会在可执行文件同目录下自动创建。

```toml
port = 8443
http_redirect_port = 8080
password = "your_password_here"  # 连接密码
# cert_path = "path/to/cert.pem"  # TLS 证书路径
# key_path = "path/to/key.pem"    # TLS 私钥路径
enable_upnp = true
heartbeat_timeout_secs = 30
max_connections = 8
max_unauth_connections = 3
```

### Command Line Options / 命令行选项

```
WebPad 0.1.0
Virtual gamepad simulator / 虚拟手柄模拟器

USAGE / 用法:
    webpad.exe [OPTIONS]

OPTIONS / 选项:
    -p, --port <PORT>                    Listening port / 监听端口
    -w, --password <PASSWORD>            Connection password / 连接密码
        --no-upnp                        Disable UPnP port mapping / 禁用 UPnP 端口映射
        --heartbeat-timeout <SECONDS>    Heartbeat timeout in seconds / 心跳超时秒数
    -h, --help                           Print help / 打印帮助信息
    -V, --version                        Print version / 打印版本信息
```

---

## Web Interface / Web 界面

The web interface provides:
Web 界面提供以下功能：

- Dual analog joysticks / 双模拟摇杆
- ABXY action buttons / ABXY 功能按键
- D-pad (directional pad) / 十字方向键
- LB/RB shoulder buttons / LB/RB 肩键
- Back/Start buttons / Back/Start 按键
- Left/Right triggers / 左右扳机键
- Password authentication dialog / 密码认证对话框
- Connection status indicator / 连接状态指示
- Automatic reconnection with exponential backoff / 指数退避自动重连

---

## Security / 安全机制

- TLS encryption enabled by default / 默认启用 TLS 加密
- Argon2id password hashing (no SHA-256 fallback for new hashes) / Argon2id 密码哈希（新哈希不降级到 SHA-256）
- Connection limiting (max 8 total, 3 unauthenticated) / 连接数限制（最多 8 个总连接，3 个未认证连接）
- Authentication failure exponential backoff (500ms - 10s) / 认证失败指数退避（500毫秒 - 10秒）
- Maximum 5 authentication failures per connection / 每个连接最多 5 次认证失败
- Session tokens (32-byte random) for reconnection / 32 字节随机 Session Token 用于重连
- Fail-closed: No password = all authentication fails / 关闭失败模式：未设置密码时所有认证均失败

---

## Project Structure / 项目结构

```
src/
├── main.rs              # Entry point / 程序入口
├── config.rs            # Configuration management / 配置管理
├── protocol.rs          # WebSocket message protocol / WebSocket 消息协议
├── auth.rs              # Authentication manager / 认证管理器
├── password.rs          # Password hashing utilities / 密码哈希工具
├── gamepad/
│   ├── mod.rs           # Module exports / 模块导出
│   ├── types.rs         # Gamepad state types / 手柄状态类型
│   └── manager.rs       # ViGEmBus controller management / ViGEmBus 控制器管理
├── upnp/
│   ├── mod.rs           # Module exports / 模块导出
│   └── mapper.rs        # UPnP port mapping / UPnP 端口映射
└── web/
    ├── mod.rs           # Module exports / 模块导出
    ├── server.rs        # Axum router and state / Axum 路由与状态
    └── handler.rs       # HTTP/WebSocket handlers / HTTP/WebSocket 处理器
```

---

## Contributing / 贡献

Contributions are welcome! Please feel free to submit issues and pull requests.
欢迎贡献！请随时提交 Issue 和 Pull Request。

---

## License / 许可证

MIT License